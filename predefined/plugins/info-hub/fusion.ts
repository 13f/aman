import { writeFile, mkdir, readFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import process from 'node:process';
import { Database } from 'bun:sqlite';
import { homedir } from 'node:os';

// ============================================================================
// Constants
// ============================================================================

const MINIMAX_API_URL = 'https://api.minimax.chat/v1/text/chatcompletion_v2';
const FEED_FETCH_TIMEOUT_MS = 15_000;
const FEED_CONCURRENCY = 10;
const MINIMAX_BATCH_SIZE = 5;
const MAX_CONCURRENT_MINIMAX = 3;
const DESCRIPTION_MAX_LEN = 384;

// ============================================================================
// Types
// ============================================================================

type CategoryId = 'ai-ml' | 'security' | 'engineering' | 'tools' | 'opinion' | 'other';

const CATEGORY_META: Record<CategoryId, { emoji: string; label: string }> = {
  'ai-ml':       { emoji: '🤖', label: 'AI / ML' },
  'security':    { emoji: '🔒', label: '安全' },
  'engineering': { emoji: '⚙️', label: '工程' },
  'tools':       { emoji: '🛠', label: '工具 / 开源' },
  'opinion':     { emoji: '💡', label: '观点 / 杂谈' },
  'other':       { emoji: '📝', label: '其他' },
};

interface Article {
  title: string;
  link: string;
  pubDate: Date;
  description: string;
  sourceName: string;
  sourceUrl: string;
}

interface ScoredArticle extends Article {
  score: number;
  scoreBreakdown: {
    relevance: number;
    quality: number;
    timeliness: number;
  };
  category: CategoryId;
  keywords: string[];
  titleZh: string;
  summary: string;
  reason: string;
}

interface MinimaxScoringResult {
  results: Array<{
    index: number;
    relevance: number;
    quality: number;
    timeliness: number;
    category: string;
    keywords: string[];
  }>;
}

interface MinimaxSummaryResult {
  results: Array<{
    index: number;
    titleZh: string;
    summary: string;
    reason: string;
  }>;
}

interface IConfig {
  minimaxApiKey?: string;
  fusionDbPath?: string;
  outputDir?: string;
  timeRange?: number;
  topN?: number;
  language?: string;
  lastUsed?: string;
}

// ============================================================================
// RSS/Atom Parsing (using Bun's built-in HTMLRewriter or manual XML parsing)
// ============================================================================

function stripHtml(html: string): string {
  return html
    .replace(/<[^>]*>/g, '')
    .replace(/&amp;/g, '&')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&nbsp;/g, ' ')
    .replace(/&#(\d+);/g, (_, code) => String.fromCharCode(parseInt(code)))
    .trim();
}

function truncateWords(text: string, maxLen: number): string {
  if (text.length <= maxLen) return text;
  const sliced = text.slice(0, maxLen);
  // 1. Try sentence boundary: last . ! ? 。 ！ ？
  const sentence = sliced.match(/^(.*[.!?。！？])/);
  if (sentence) return sentence[1].trimEnd();
  // 2. Fallback to last space
  const lastSpace = sliced.lastIndexOf(' ');
  if (lastSpace > maxLen * 0.6) return sliced.slice(0, lastSpace);
  return sliced;
}

function extractCDATA(text: string): string {
  const cdataMatch = text.match(/<!\[CDATA\[([\s\S]*?)\]\]>/);
  return cdataMatch ? cdataMatch[1] : text;
}

function getTagContent(xml: string, tagName: string): string {
  // Handle namespaced and non-namespaced tags
  const patterns = [
    new RegExp(`<${tagName}[^>]*>([\\s\\S]*?)</${tagName}>`, 'i'),
    new RegExp(`<${tagName}[^>]*/>`, 'i'), // self-closing
  ];
  
  for (const pattern of patterns) {
    const match = xml.match(pattern);
    if (match?.[1]) {
      return extractCDATA(match[1]).trim();
    }
  }
  return '';
}

function getAttrValue(xml: string, tagName: string, attrName: string): string {
  const pattern = new RegExp(`<${tagName}[^>]*\\s${attrName}=["']([^"']*)["'][^>]*/?>`, 'i');
  const match = xml.match(pattern);
  return match?.[1] || '';
}

function parseDate(dateStr: string): Date | null {
  if (!dateStr) return null;
  
  const d = new Date(dateStr);
  if (!isNaN(d.getTime())) return d;
  
  // Try common RSS date formats
  // RFC 822: "Mon, 01 Jan 2024 00:00:00 GMT"
  const rfc822 = dateStr.match(/(\d{1,2})\s+(\w{3})\s+(\d{4})\s+(\d{2}):(\d{2}):(\d{2})/);
  if (rfc822) {
    const parsed = new Date(dateStr);
    if (!isNaN(parsed.getTime())) return parsed;
  }
  
  return null;
}

function parseRSSItems(xml: string): Array<{ title: string; link: string; pubDate: string; description: string }> {
  const items: Array<{ title: string; link: string; pubDate: string; description: string }> = [];
  
  // Detect format: Atom vs RSS
  const isAtom = xml.includes('<feed') && xml.includes('xmlns="http://www.w3.org/2005/Atom"') || xml.includes('<feed ');
  
  if (isAtom) {
    // Atom format: <entry>
    const entryPattern = /<entry[\s>]([\s\S]*?)<\/entry>/gi;
    let entryMatch;
    while ((entryMatch = entryPattern.exec(xml)) !== null) {
      const entryXml = entryMatch[1];
      const title = stripHtml(getTagContent(entryXml, 'title'));
      
      // Atom link: <link href="..." rel="alternate"/>
      let link = getAttrValue(entryXml, 'link[^>]*rel="alternate"', 'href');
      if (!link) {
        link = getAttrValue(entryXml, 'link', 'href');
      }
      
      const pubDate = getTagContent(entryXml, 'published') 
        || getTagContent(entryXml, 'updated');
      
      const description = stripHtml(
        getTagContent(entryXml, 'summary') 
        || getTagContent(entryXml, 'content')
      );
      
      if (title || link) {
        items.push({ title, link, pubDate, description: truncateWords(description, DESCRIPTION_MAX_LEN) });      }
    }
  } else {
    // RSS format: <item>
    const itemPattern = /<item[\s>]([\s\S]*?)<\/item>/gi;
    let itemMatch;
    while ((itemMatch = itemPattern.exec(xml)) !== null) {
      const itemXml = itemMatch[1];
      const title = stripHtml(getTagContent(itemXml, 'title'));
      const link = getTagContent(itemXml, 'link') || getTagContent(itemXml, 'guid');
      const pubDate = getTagContent(itemXml, 'pubDate') 
        || getTagContent(itemXml, 'dc:date')
        || getTagContent(itemXml, 'date');
      const description = stripHtml(
        getTagContent(itemXml, 'description') 
        || getTagContent(itemXml, 'content:encoded')
      );
      
      if (title || link) {
        items.push({ title, link, pubDate, description: truncateWords(description, DESCRIPTION_MAX_LEN) });      }
    }
  }
  
  return items;
}

// ============================================================================
// Article Content Fetching (for AI summarization)
// ============================================================================

async function fetchArticleContent(url: string): Promise<string> {
  try {
    const response = await fetch(url, {
      method: 'GET',
      headers: {
        'User-Agent': 'Mozilla/5.0 (compatible; RSS Reader)',
        'Accept': 'text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8',
      },
      signal: AbortSignal.timeout(8000),
    });
    
    if (!response.ok) return '';
    
    const html = await response.text();
    return extractMainContent(html);
  } catch {
    return '';
  }
}

function extractMainContent(html: string): string {
  // Limit HTML size first to avoid memory issues
  const limitedHtml = html.slice(0, 50000);
  
  return limitedHtml
    .replace(/<script[^>]*>[\s\S]*?<\/script>/gi, ' ')
    .replace(/<style[^>]*>[\s\S]*?<\/style>/gi, ' ')
    .replace(/<nav[^>]*>[\s\S]*?<\/nav>/gi, ' ')
    .replace(/<header[^>]*>[\s\S]*?<\/header>/gi, ' ')
    .replace(/<footer[^>]*>[\s\S]*?<\/footer>/gi, ' ')
    .replace(/<aside[^>]*>[\s\S]*?<\/aside>/gi, ' ')
    .replace(/<[^>]+>/g, ' ')
    .replace(/\s+/g, ' ')
    .trim()
    .slice(0, 1500); // Strict limit for AI context
}

// ============================================================================
// Config
// ============================================================================

async function loadConfig() : Promise<IConfig> {
  const configPath = join(homedir(), '.ai-digest', 'config.json');
  const content = await readFile(configPath, 'utf-8');
  const config = JSON.parse(content) as IConfig;
  return config;
}

async function loadApiKey(): Promise<string | undefined> {
  if (process.env.MINIMAX_API_KEY) {
    return process.env.MINIMAX_API_KEY;
  }

  try {
    const config = await loadConfig();
    if (config.minimaxApiKey) {
      console.log('[digest] Loaded API key from ~/.ai-digest/config.json');
      return config.minimaxApiKey;
    }
  } catch {
    // Config file not found or invalid, ignore
  }

  return undefined;
}

// ============================================================================
// Fusion Integration
// ============================================================================

async function fetchFromFusion(hours: number, config?: IConfig): Promise<Article[]> {
  const dbPath = config?.fusionDbPath || process.env.FUSION_DB_PATH || '/Users/jerin/apps/fusion/fusion.db';
  console.log(`[digest] Connecting to fusion database: ${dbPath}`);

  const db = new Database(dbPath);

  try {
    const cutoffTime = Math.floor(Date.now() / 1000) - hours * 3600;

    const query = `
      SELECT
        i.id,
        i.title,
        i.link,
        i.content,
        i.pub_date,
        f.name as feed_name,
        f.site_url as feed_site_url
      FROM items i
      JOIN feeds f ON i.feed_id = f.id
      WHERE i.pub_date > ?
      ORDER BY i.pub_date DESC
    `;

    const rows = db.query(query).all(cutoffTime) as Array<{
      id: number;
      title: string;
      link: string;
      content: string;
      pub_date: number;
      feed_name: string;
      feed_site_url: string;
    }>;

    console.log(`[digest] Fetched ${rows.length} articles from fusion (last ${hours} hours)`);

    return rows.map(row => ({
      title: row.title || 'Untitled',
      link: row.link || '',
      pubDate: new Date(row.pub_date * 1000),
      description: stripHtml(row.content || '').slice(0, 2000),
      sourceName: row.feed_name || 'unknown',
      sourceUrl: row.feed_site_url || '',
    }));
  } finally {
    db.close();
  }
}

async function fetchAllFeeds(hours: number, config?: IConfig): Promise<Article[]> {
  const dbPath = config?.fusionDbPath || process.env.FUSION_DB_PATH || '/Users/jerin/apps/fusion/fusion.db';

  if (existsSync(dbPath)) {
    console.log('[digest] Fetching articles from fusion database...');
    const articles = await fetchFromFusion(hours, config);
    console.log(`[digest] ✓ Fetched ${articles.length} articles from fusion database`);
    return articles;
  }

  console.log('[digest] Fusion database not found, fetching feeds directly...');
  const articles = await fetchAllFeedsDirectly(hours);
  console.log(`[digest] ✓ Fetched ${articles.length} articles directly from RSS feeds`);
  return articles;
}

// ============================================================================
// MiniMax API
// ============================================================================

async function callMinimax(prompt: string, apiKey: string, model: string = 'MiniMax-M2.7', retries = 3): Promise<string> {
  let lastError: Error | undefined;

  for (let attempt = 0; attempt < retries; attempt++) {
    try {
      if (attempt > 0) {
        const delay = Math.min(1000 * Math.pow(2, attempt), 8000);
        console.warn(`[digest] Retry ${attempt}/${retries - 1} after ${delay}ms...`);
        await new Promise(r => setTimeout(r, delay));
      }

      const response = await fetch(MINIMAX_API_URL, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'Authorization': `Bearer ${apiKey}`,
        },
        body: JSON.stringify({
          model,
          messages: [
            { role: 'user', content: [{ type: 'text', text: prompt }] }
          ],
          temperature: 0.3,
          max_tokens: 32768,
        }),
        signal: AbortSignal.timeout(90_000),
      });

      if (!response.ok) {
        const errorText = await response.text().catch(() => 'Unknown error');
        throw new Error(`MiniMax API error (${response.status}): ${errorText}`);
      }

      const data = await response.json() as {
        choices?: Array<{
          message?: { content?: string };
        }>;
        base_resp?: { status_code?: number; status_msg?: string };
      };

      // Check for API-level errors
      if (data.base_resp?.status_code && data.base_resp.status_code !== 0) {
        throw new Error(`MiniMax API error: ${data.base_resp.status_msg || 'Unknown error'}`);
      }

      // Handle new content format: could be string or array
      const content = data.choices?.[0]?.message?.content;
      if (typeof content === 'string') {
        return content;
      } else if (Array.isArray(content)) {
        return content.map((c: any) => c.text || c.content || '').join('');
      }
      return '';
    } catch (error) {
      lastError = error instanceof Error ? error : new Error(String(error));
      if (attempt < retries - 1) continue;
    }
  }

  throw lastError || new Error('callMinimax failed after retries');
}

function parseJsonResponse<T>(text: string): T {
  // Replace Chinese/smart quotes with regular quotes inside JSON-safe HTML entities
  // to prevent them from being mistaken for JSON string delimiters
  let jsonText = text
    .replace(/\u201C/g, '\uFF02')  // " → ＂
    .replace(/\u201D/g, '\uFF02')  // " → ＂
    .replace(/\u2018/g, '\uFF07')  // ' → ＇
    .replace(/\u2019/g, '\uFF07')  // ' → ＇
    .trim();
  // Strip markdown code blocks if present
  if (jsonText.startsWith('```')) {
    jsonText = jsonText.replace(/^```(?:json)?\n?/, '').replace(/\n?```$/, '').trim();
  }
  // Also strip trailing ``` that may appear mid-stream
  const trailingFence = jsonText.indexOf('\n```');
  if (trailingFence > 0) jsonText = jsonText.slice(0, trailingFence).trim();
  // Extract JSON object: find first { and its matching }
  const firstBrace = jsonText.indexOf('{');
  if (firstBrace >= 0) {
    let depth = 0;
    let end = -1;
    let inString = false;
    for (let i = firstBrace; i < jsonText.length; i++) {
      const ch = jsonText[i];
      if (inString) {
        if (ch === '\\' && i + 1 < jsonText.length) { i++; continue; }
        if (ch === '"') inString = false;
      } else {
        if (ch === '"') inString = true;
        else if (ch === '{') depth++;
        else if (ch === '}') { depth--; if (depth === 0) { end = i; break; } }
      }
    }
    if (end >= 0) {
      jsonText = jsonText.slice(firstBrace, end + 1);
    } else {
      jsonText = jsonText.slice(firstBrace); // truncated, will try repair below
    }
  }
  try {
    return JSON.parse(jsonText) as T;
  } catch {
    // Attempt to repair truncated JSON
    let repaired = jsonText;
    const quotes = (repaired.match(/"/g) || []).length;
    if (quotes % 2 !== 0) repaired += '"';
    const stack: string[] = [];
    let str = false;
    for (let i = 0; i < repaired.length; i++) {
      const ch = repaired[i];
      if (str) {
        if (ch === '\\' && i + 1 < repaired.length) { i++; continue; }
        if (ch === '"') str = false;
      } else {
        if (ch === '"') str = true;
        else if (ch === '{' || ch === '[') stack.push(ch);
        else if (ch === '}') { if (stack[stack.length - 1] === '{') stack.pop(); }
        else if (ch === ']') { if (stack[stack.length - 1] === '[') stack.pop(); }
      }
    }
    for (let i = stack.length - 1; i >= 0; i--) {
      repaired += stack[i] === '{' ? '}' : ']';
    }
    return JSON.parse(repaired) as T;
  }
}

// ============================================================================
// AI Scoring
// ============================================================================

function buildScoringPrompt(articles: Array<{ index: number; title: string; description: string; sourceName: string }>): string {
  const articlesList = articles.map(a =>
    `Index ${a.index}: [${a.sourceName}] ${a.title}\n${truncateWords(a.description, DESCRIPTION_MAX_LEN)}`
  ).join('\n\n---\n\n');

  return `你是一个技术内容策展人，正在为一份面向技术爱好者的每日精选摘要筛选文章。

请对以下文章进行三个维度的评分（1-10 整数，10 分最高），并为每篇文章分配一个分类标签和提取 2-4 个关键词。

## 评分维度

### 1. 相关性 (relevance) - 对技术/编程/AI/互联网从业者的价值
- 10: 所有技术人都应该知道的重大事件/突破
- 7-9: 对大部分技术从业者有价值
- 4-6: 对特定技术领域有价值
- 1-3: 与技术行业关联不大

### 2. 质量 (quality) - 文章本身的深度和写作质量
- 10: 深度分析，原创洞见，引用丰富
- 7-9: 有深度，观点独到
- 4-6: 信息准确，表达清晰
- 1-3: 浅尝辄止或纯转述

### 3. 时效性 (timeliness) - 当前是否值得阅读
- 10: 正在发生的重大事件/刚发布的重要工具
- 7-9: 近期热点相关
- 4-6: 常青内容，不过时
- 1-3: 过时或无时效价值

## 分类标签（必须从以下选一个）
- ai-ml: AI、机器学习、LLM、深度学习相关
- security: 安全、隐私、漏洞、加密相关
- engineering: 软件工程、架构、编程语言、系统设计
- tools: 开发工具、开源项目、新发布的库/框架
- opinion: 行业观点、个人思考、职业发展、文化评论
- other: 以上都不太适合的

## 关键词提取
提取 2-4 个最能代表文章主题的关键词（用英文，简短，如 "Rust", "LLM", "database", "performance"）

## 待评分文章

${articlesList}

请严格按 JSON 格式返回，不要包含 markdown 代码块或其他文字：
{
  "results": [
    {
      "index": 0,
      "relevance": 8,
      "quality": 7,
      "timeliness": 9,
      "category": "engineering",
      "keywords": ["Rust", "compiler", "performance"]
    }
  ]
}`;
}

async function scoreArticlesWithAI(
  articles: Article[],
  apiKey: string
): Promise<Map<number, { relevance: number; quality: number; timeliness: number; category: CategoryId; keywords: string[] }>> {
  const allScores = new Map<number, { relevance: number; quality: number; timeliness: number; category: CategoryId; keywords: string[] }>();
  
  const indexed = articles.map((article, index) => ({
    index,
    title: article.title,
    description: article.description,
    sourceName: article.sourceName,
  }));
  
  const batches: typeof indexed[] = [];
  for (let i = 0; i < indexed.length; i += MINIMAX_BATCH_SIZE) {
    batches.push(indexed.slice(i, i + MINIMAX_BATCH_SIZE));
  }
  
  console.log(`[digest] AI scoring: ${articles.length} articles in ${batches.length} batches`);
  
  const validCategories = new Set<string>(['ai-ml', 'security', 'engineering', 'tools', 'opinion', 'other']);
  
  for (let i = 0; i < batches.length; i += MAX_CONCURRENT_MINIMAX) {
    const batchGroup = batches.slice(i, i + MAX_CONCURRENT_MINIMAX);
    const promises = batchGroup.map(async (batch) => {
      let responseText: string | undefined;
      try {
        const prompt = buildScoringPrompt(batch);
        responseText = await callMinimax(prompt, apiKey);
        const parsed = parseJsonResponse<MinimaxScoringResult>(responseText);
        
        if (parsed.results && Array.isArray(parsed.results)) {
          for (const result of parsed.results) {
            const clamp = (v: number) => Math.min(10, Math.max(1, Math.round(v)));
            const cat = (validCategories.has(result.category) ? result.category : 'other') as CategoryId;
            allScores.set(result.index, {
              relevance: clamp(result.relevance),
              quality: clamp(result.quality),
              timeliness: clamp(result.timeliness),
              category: cat,
              keywords: Array.isArray(result.keywords) ? result.keywords.slice(0, 4) : [],
            });
          }
        }
      } catch (error) {
        console.warn(`[digest] Scoring batch failed: ${error instanceof Error ? error.message : String(error)}`);
        if (error instanceof SyntaxError || (error instanceof Error && error.message.includes('JSON'))) {
          console.warn(`[digest] [DEBUG] Raw response (first 500 chars):\n${responseText?.slice(0, 500)}`);
        }
        for (const item of batch) {
          allScores.set(item.index, { relevance: 5, quality: 5, timeliness: 5, category: 'other', keywords: [] });
        }
      }
    });
    
    await Promise.all(promises);
    console.log(`[digest] Scoring progress: ${Math.min(i + MAX_CONCURRENT_MINIMAX, batches.length)}/${batches.length} batches`);
  }
  
  return allScores;
}

// ============================================================================
// AI Summarization
// ============================================================================

function buildSummaryPrompt(
  articles: Array<{ index: number; title: string; description: string; sourceName: string; link: string }>,
  lang: 'zh' | 'en'
): string {
  const articlesList = articles.map(a =>
    `Index ${a.index}: [${a.sourceName}] ${a.title}\n${truncateWords(a.description, 600)}`
  ).join('\n\n---\n\n');

  const langInstruction = lang === 'zh'
    ? '请用中文撰写摘要和推荐理由。如果原文是英文，请翻译为中文。标题翻译也用中文。'
    : 'Write summaries, reasons, and title translations in English.';

  return `你是一个技术内容摘要专家。请为以下文章完成三件事：

1. **中文标题** (titleZh): 将英文标题翻译成自然的中文。如果原标题已经是中文则保持不变。
2. **摘要** (summary): 4-6 句话的结构化摘要，让读者不点进原文也能了解核心内容。包含：
   - 文章讨论的核心问题或主题（1 句）
   - 关键论点、技术方案或发现（2-3 句）
   - 结论或作者的核心观点（1 句）
3. **推荐理由** (reason): 1 句话说明"为什么值得读"，区别于摘要（摘要说"是什么"，推荐理由说"为什么"）。

${langInstruction}

摘要要求：
- 直接说重点，不要用"本文讨论了..."、"这篇文章介绍了..."这种开头
- 包含具体的技术名词、数据、方案名称或观点
- 保留关键数字和指标（如性能提升百分比、用户数、版本号等）
- 如果文章涉及对比或选型，要点出比较对象和结论
- 目标：读者花 30 秒读完摘要，就能决定是否值得花 10 分钟读原文

## 待摘要文章

${articlesList}

请严格按 JSON 格式返回：
{
  "results": [
    {
      "index": 0,
      "titleZh": "中文翻译的标题",
      "summary": "摘要内容...",
      "reason": "推荐理由..."
    }
  ]
}`;
}

async function summarizeArticles(
  articles: Array<Article & { index: number }>,
  apiKey: string,
  lang: 'zh' | 'en'
): Promise<Map<number, { titleZh: string; summary: string; reason: string }>> {
  const summaries = new Map<number, { titleZh: string; summary: string; reason: string }>();
  
  const indexed = articles.map(a => ({
    index: a.index,
    title: a.title,
    description: a.description,
    sourceName: a.sourceName,
    link: a.link,
  }));
  
  const batches: typeof indexed[] = [];
  for (let i = 0; i < indexed.length; i += MINIMAX_BATCH_SIZE) {
    batches.push(indexed.slice(i, i + MINIMAX_BATCH_SIZE));
  }
  
  console.log(`[digest] Generating summaries for ${articles.length} articles in ${batches.length} batches`);
  
  for (let i = 0; i < batches.length; i += MAX_CONCURRENT_MINIMAX) {
    const batchGroup = batches.slice(i, i + MAX_CONCURRENT_MINIMAX);
    const promises = batchGroup.map(async (batch) => {
      let responseText: string | undefined;
      try {
        const prompt = buildSummaryPrompt(batch, lang);
        responseText = await callMinimax(prompt, apiKey);
        const parsed = parseJsonResponse<MinimaxSummaryResult>(responseText);
        
        if (parsed.results && Array.isArray(parsed.results)) {
          for (const result of parsed.results) {
            const article = batch.find(a => a.index === result.index) || batch[0];
            summaries.set(result.index, {
              titleZh: result.titleZh || article.title,
              summary: result.summary || truncateWords(article.description, DESCRIPTION_MAX_LEN),
              reason: result.reason || '【AI摘要失败】',
            });
          }
        } else {
          console.warn(`[digest] Summary parse failed for batch, using fallbacks`);
          console.warn(`[digest] [DEBUG] Raw response (first 500 chars):\n${responseText?.slice(0, 500)}`);
          for (const item of batch) {
            summaries.set(item.index, { titleZh: item.title, summary: truncateWords(item.description, DESCRIPTION_MAX_LEN), reason: '【AI解析失败】' });
          }
        }
      } catch (error) {
        console.warn(`[digest] Summary batch failed: ${error instanceof Error ? error.message : String(error)}`);
        if (error instanceof SyntaxError || (error instanceof Error && error.message.includes('JSON'))) {
          console.warn(`[digest] [DEBUG] Raw response (first 500 chars):\n${responseText?.slice(0, 500)}`);
        }
        for (const item of batch) {
          summaries.set(item.index, { titleZh: item.title, summary: truncateWords(item.description, DESCRIPTION_MAX_LEN), reason: '【AI摘要失败，使用原文描述】' });
        }
      }
    });
    
    await Promise.all(promises);
    console.log(`[digest] Summary progress: ${Math.min(i + MAX_CONCURRENT_MINIMAX, batches.length)}/${batches.length} batches`);
  }
  
  return summaries;
}

// ============================================================================
// AI Highlights (Today's Trends)
// ============================================================================

async function generateHighlights(
  articles: ScoredArticle[],
  apiKey: string,
  lang: 'zh' | 'en'
): Promise<string> {
  const articleList = articles.slice(0, 10).map((a, i) =>
    `${i + 1}. [${a.category}] ${a.titleZh || a.title} — ${a.summary.slice(0, 100)}`
  ).join('\n');

  const langNote = lang === 'zh' ? '用中文回答。' : 'Write in English.';

  const prompt = `根据以下今日精选技术文章列表，写一段 3-5 句话的"今日看点"总结。
要求：
- 提炼出今天技术圈的 2-3 个主要趋势或话题
- 不要逐篇列举，要做宏观归纳
- 风格简洁有力，像新闻导语
${langNote}

文章列表：
${articleList}

直接返回纯文本总结，不要 JSON，不要 markdown 格式。`;

  try {
    const text = await callMinimax(prompt, apiKey);
    return text.trim();
  } catch (error) {
    console.warn(`[digest] Highlights generation failed: ${error instanceof Error ? error.message : String(error)}`);
    return '';
  }
}

// ============================================================================
// Visualization Helpers
// ============================================================================

function humanizeTime(pubDate: Date): string {
  const diffMs = Date.now() - pubDate.getTime();
  const diffMins = Math.floor(diffMs / 60_000);
  const diffHours = Math.floor(diffMs / 3_600_000);
  const diffDays = Math.floor(diffMs / 86_400_000);

  if (diffMins < 60) return `${diffMins} 分钟前`;
  if (diffHours < 24) return `${diffHours} 小时前`;
  if (diffDays < 7) return `${diffDays} 天前`;
  return pubDate.toISOString().slice(0, 10);
}

function generateKeywordBarChart(articles: ScoredArticle[]): string {
  const kwCount = new Map<string, number>();
  for (const a of articles) {
    for (const kw of a.keywords) {
      const normalized = kw.toLowerCase();
      kwCount.set(normalized, (kwCount.get(normalized) || 0) + 1);
    }
  }

  const sorted = Array.from(kwCount.entries())
    .sort((a, b) => b[1] - a[1])
    .slice(0, 12);

  if (sorted.length === 0) return '';

  const labels = sorted.map(([k]) => `"${k}"`).join(', ');
  const values = sorted.map(([, v]) => v).join(', ');
  const maxVal = sorted[0][1];

  let chart = '```mermaid\n';
  chart += `xychart-beta horizontal\n`;
  chart += `    title "高频关键词"\n`;
  chart += `    x-axis [${labels}]\n`;
  chart += `    y-axis "出现次数" 0 --> ${maxVal + 2}\n`;
  chart += `    bar [${values}]\n`;
  chart += '```\n';

  return chart;
}

function generateCategoryPieChart(articles: ScoredArticle[]): string {
  const catCount = new Map<CategoryId, number>();
  for (const a of articles) {
    catCount.set(a.category, (catCount.get(a.category) || 0) + 1);
  }

  if (catCount.size === 0) return '';

  const sorted = Array.from(catCount.entries()).sort((a, b) => b[1] - a[1]);

  let chart = '```mermaid\n';
  chart += `pie showData\n`;
  chart += `    title "文章分类分布"\n`;
  for (const [cat, count] of sorted) {
    const meta = CATEGORY_META[cat];
    chart += `    "${meta.emoji} ${meta.label}" : ${count}\n`;
  }
  chart += '```\n';

  return chart;
}

function generateAsciiBarChart(articles: ScoredArticle[]): string {
  const kwCount = new Map<string, number>();
  for (const a of articles) {
    for (const kw of a.keywords) {
      const normalized = kw.toLowerCase();
      kwCount.set(normalized, (kwCount.get(normalized) || 0) + 1);
    }
  }

  const sorted = Array.from(kwCount.entries())
    .sort((a, b) => b[1] - a[1])
    .slice(0, 10);

  if (sorted.length === 0) return '';

  const maxVal = sorted[0][1];
  const maxBarWidth = 20;
  const maxLabelLen = Math.max(...sorted.map(([k]) => k.length));

  let chart = '```\n';
  for (const [label, value] of sorted) {
    const barLen = Math.max(1, Math.round((value / maxVal) * maxBarWidth));
    const bar = '█'.repeat(barLen) + '░'.repeat(maxBarWidth - barLen);
    chart += `${label.padEnd(maxLabelLen)} │ ${bar} ${value}\n`;
  }
  chart += '```\n';

  return chart;
}

function generateTagCloud(articles: ScoredArticle[]): string {
  const kwCount = new Map<string, number>();
  for (const a of articles) {
    for (const kw of a.keywords) {
      const normalized = kw.toLowerCase();
      kwCount.set(normalized, (kwCount.get(normalized) || 0) + 1);
    }
  }

  const sorted = Array.from(kwCount.entries())
    .sort((a, b) => b[1] - a[1])
    .slice(0, 20);

  if (sorted.length === 0) return '';

  return sorted
    .map(([word, count], i) => i < 3 ? `**${word}**(${count})` : `${word}(${count})`)
    .join(' · ');
}

// ============================================================================
// Report Generation
// ============================================================================

function generateDigestReport(articles: ScoredArticle[], highlights: string, stats: {
  totalFeeds: number;
  successFeeds: number;
  totalArticles: number;
  filteredArticles: number;
  hours: number;
  lang: string;
}): string {
  const now = new Date();
  const dateStr = now.toISOString().split('T')[0];
  
  let report = `# 📰 AI 博客每日精选 — ${dateStr}\n\n`;
  report += `> 来自 Fusion 追踪的 ${stats.totalFeeds} 个技术博客，AI 精选 Top ${articles.length}\n\n`;

  // ── Today's Highlights ──
  if (highlights) {
    report += `## 📝 今日看点\n\n`;
    report += `${highlights}\n\n`;
    report += `---\n\n`;
  }

  // ── Top 3 Deep Showcase ──
  if (articles.length >= 3) {
    report += `## 🏆 今日必读\n\n`;
    for (let i = 0; i < Math.min(3, articles.length); i++) {
      const a = articles[i];
      const medal = ['🥇', '🥈', '🥉'][i];
      const catMeta = CATEGORY_META[a.category];
      
      report += `${medal} **${a.titleZh || a.title}**\n\n`;
      report += `[${a.title}](${a.link}) — ${a.sourceName} · ${humanizeTime(a.pubDate)} · ${catMeta.emoji} ${catMeta.label}\n\n`;
      report += `> ${a.summary}\n\n`;
      if (a.reason) {
        report += `💡 **为什么值得读**: ${a.reason}\n\n`;
      }
      if (a.keywords.length > 0) {
        report += `🏷️ ${a.keywords.join(', ')}\n\n`;
      }
    }
    report += `---\n\n`;
  }

  // ── Visual Statistics ──
  report += `## 📊 数据概览\n\n`;

  report += `| 扫描源 | 抓取文章 | 时间范围 | 精选 |\n`;
  report += `|:---:|:---:|:---:|:---:|\n`;
  report += `| ${stats.successFeeds}/${stats.totalFeeds} | ${stats.totalArticles} 篇 → ${stats.filteredArticles} 篇 | ${stats.hours}h | **${articles.length} 篇** |\n\n`;

  const pieChart = generateCategoryPieChart(articles);
  if (pieChart) {
    report += `### 分类分布\n\n${pieChart}\n`;
  }

  const barChart = generateKeywordBarChart(articles);
  if (barChart) {
    report += `### 高频关键词\n\n${barChart}\n`;
  }

  const asciiChart = generateAsciiBarChart(articles);
  if (asciiChart) {
    report += `<details>\n<summary>📈 纯文本关键词图（终端友好）</summary>\n\n${asciiChart}\n</details>\n\n`;
  }

  const tagCloud = generateTagCloud(articles);
  if (tagCloud) {
    report += `### 🏷️ 话题标签\n\n${tagCloud}\n\n`;
  }

  report += `---\n\n`;

  // ── Category-Grouped Articles ──
  const categoryGroups = new Map<CategoryId, ScoredArticle[]>();
  for (const a of articles) {
    const list = categoryGroups.get(a.category) || [];
    list.push(a);
    categoryGroups.set(a.category, list);
  }

  const sortedCategories = Array.from(categoryGroups.entries())
    .sort((a, b) => b[1].length - a[1].length);

  let globalIndex = 0;
  for (const [catId, catArticles] of sortedCategories) {
    const catMeta = CATEGORY_META[catId];
    report += `## ${catMeta.emoji} ${catMeta.label}\n\n`;

    for (const a of catArticles) {
      globalIndex++;
      const scoreTotal = a.scoreBreakdown.relevance + a.scoreBreakdown.quality + a.scoreBreakdown.timeliness;

      report += `### ${globalIndex}. ${a.titleZh || a.title}\n\n`;
      report += `[${a.title}](${a.link}) — **${a.sourceName}** · ${humanizeTime(a.pubDate)} · ⭐ ${scoreTotal}/30\n\n`;
      report += `> ${a.summary}\n\n`;
      if (a.keywords.length > 0) {
        report += `🏷️ ${a.keywords.join(', ')}\n\n`;
      }
      report += `---\n\n`;
    }
  }

  // ── Footer ──
  report += `*生成于 ${dateStr} ${now.toISOString().split('T')[1]?.slice(0, 5) || ''} | 扫描 ${stats.successFeeds} 源 → 获取 ${stats.totalArticles} 篇 → 精选 ${articles.length} 篇*\n`;
  report += `*基于 Fusion 聚合的 RSS 源，使用 MiniMax AI 智能筛选*\n`;

  return report;
}

// ============================================================================
// CLI
// ============================================================================

function printUsage(): never {
  console.log(`AI Daily Digest - AI-powered RSS digest from 90 top tech blogs

Usage:
  bun scripts/digest.ts [options]

Options:
  --hours <n>     Time range in hours (default: 48)
  --top-n <n>     Number of top articles to include (default: 15)
  --lang <lang>   Summary language: zh or en (default: zh)
  --output <path> Output file path (default: ./digest-YYYYMMDD.md)
  --help          Show this help

Environment:
  MINIMAX_API_KEY  Required. Get one at https://platform.minimaxi.com

Examples:
  bun scripts/digest.ts --hours 24 --top-n 10 --lang zh
  bun scripts/digest.ts --hours 72 --top-n 20 --lang en --output ./my-digest.md
`);
  process.exit(0);
}

async function main(): Promise<void> {
  // Sync lowercase proxy env vars to uppercase (Bun's fetch only reads uppercase)
  if (!process.env.HTTPS_PROXY && process.env.https_proxy) {
    process.env.HTTPS_PROXY = process.env.https_proxy;
  }
  if (!process.env.HTTP_PROXY && process.env.http_proxy) {
    process.env.HTTP_PROXY = process.env.http_proxy;
  }
  if (!process.env.NO_PROXY && process.env.no_proxy) {
    process.env.NO_PROXY = process.env.no_proxy;
  }

  const args = process.argv.slice(2);
  if (args.includes('--help') || args.includes('-h')) printUsage();
  
  // Load config file
  let config: IConfig | undefined;
  try {
    config = await loadConfig();
    console.log('[digest] Loaded config from ~/.ai-digest/config.json');
  } catch {
    // Config file not found, will use defaults and env vars
  }

  const apiKey = config?.minimaxApiKey || await loadApiKey();
  if (!apiKey) {
    console.error('[digest] Error: MINIMAX_API_KEY not found.');
    console.error('[digest] Set it via environment variable or ~/.ai-digest/config.json');
    console.error('[digest] Get one at: https://platform.minimaxi.com');
    process.exit(1);
  }

  let hours = config?.timeRange ?? 48;
  let topN = config?.topN ?? 15;
  let lang: 'zh' | 'en' = (config?.language as 'zh' | 'en') ?? 'zh';
  let outputPath = '';

  // CLI args override config file values
  for (let i = 0; i < args.length; i++) {
    const arg = args[i]!;
    if (arg === '--hours' && args[i + 1]) {
      hours = parseInt(args[++i]!, 10);
    } else if (arg === '--top-n' && args[i + 1]) {
      topN = parseInt(args[++i]!, 10);
    } else if (arg === '--lang' && args[i + 1]) {
      lang = args[++i] as 'zh' | 'en';
    } else if (arg === '--output' && args[i + 1]) {
      outputPath = args[++i]!;
    }
  }

  if (!outputPath) {
    const dateStr = new Date().toISOString().slice(0, 10).replace(/-/g, '');
    const outputDir = config?.outputDir ?? './output';
    outputPath = `${outputDir}/digest-${dateStr}.md`;
  }
  
  console.log(`[digest] === AI Daily Digest ===`);
  console.log(`[digest] Time range: ${hours} hours`);
  console.log(`[digest] Top N: ${topN}`);
  console.log(`[digest] Language: ${lang}`);
  console.log(`[digest] Output: ${outputPath}`);
  console.log('');
  
  console.log(`[digest] Step 1/4: Fetching articles from fusion (last ${hours} hours)...`);
  const recentArticles = await fetchAllFeeds(hours, config);

  if (recentArticles.length === 0) {
    console.error(`[digest] Error: No articles found within the last ${hours} hours.`);
    console.error(`[digest] Try increasing --hours (e.g., --hours 168 for one week)`);
    process.exit(1);
  }
  
  console.log(`[digest] Step 2/4: AI scoring ${recentArticles.length} articles...`);
  const scores = await scoreArticlesWithAI(recentArticles, apiKey);
  
  const scoredArticles = recentArticles.map((article, index) => {
    const score = scores.get(index) || { relevance: 5, quality: 5, timeliness: 5, category: 'other' as CategoryId, keywords: [] };
    return {
      ...article,
      totalScore: score.relevance + score.quality + score.timeliness,
      breakdown: score,
    };
  });
  
  scoredArticles.sort((a, b) => b.totalScore - a.totalScore);
  const topArticles = scoredArticles.slice(0, topN);
  
  console.log(`[digest] Top ${topN} articles selected (score range: ${topArticles[topArticles.length - 1]?.totalScore || 0} - ${topArticles[0]?.totalScore || 0})`);
  
  console.log(`[digest] Step 3/4: Fetching content for top ${topN} articles...`);
  // Fetch content for top articles to enable better summarization
  // Process sequentially to avoid overwhelming the network
  let fetchedCount = 0;
  for (const article of topArticles) {
    if (!article.description || article.description.length < 100) {
      const content = await fetchArticleContent(article.link);
      if (content) {
        article.description = content;
        fetchedCount++;
      }
    }
  }
  console.log(`[digest] ✓ Fetched content for ${fetchedCount}/${topN} articles`);
  
  console.log(`[digest] Step 4/4: Generating AI summaries & highlights...`);
  const indexedTopArticles = topArticles.map((a, i) => ({ ...a, index: i }));
  const summaries = await summarizeArticles(indexedTopArticles, apiKey, lang);
  
  const finalArticles: ScoredArticle[] = topArticles.map((a, i) => {
    const sm = summaries.get(i) || { titleZh: a.title, summary: truncateWords(a.description, DESCRIPTION_MAX_LEN), reason: '' };
    return {
      title: a.title,
      link: a.link,
      pubDate: a.pubDate,
      description: a.description,
      sourceName: a.sourceName,
      sourceUrl: a.sourceUrl,
      score: a.totalScore,
      scoreBreakdown: {
        relevance: a.breakdown.relevance,
        quality: a.breakdown.quality,
        timeliness: a.breakdown.timeliness,
      },
      category: a.breakdown.category,
      keywords: a.breakdown.keywords,
      titleZh: sm.titleZh,
      summary: sm.summary,
      reason: sm.reason,
    };
  });
  
  const highlights = await generateHighlights(finalArticles, apiKey, lang);
  
  const successfulSources = new Set(recentArticles.map(a => a.sourceName));
  
  const report = generateDigestReport(finalArticles, highlights, {
    totalFeeds: successfulSources.size,
    successFeeds: successfulSources.size,
    totalArticles: recentArticles.length,
    filteredArticles: recentArticles.length,
    hours,
    lang,
  });

  await mkdir(dirname(outputPath), { recursive: true });
  await writeFile(outputPath, report);

  console.log('');
  console.log(`[digest] ✅ Done!`);
  console.log(`[digest] 📁 Report: ${outputPath}`);
  console.log(`[digest] 📊 Stats: ${successfulSources.size} sources → ${recentArticles.length} articles → ${finalArticles.length} selected`);
  
  if (finalArticles.length > 0) {
    console.log('');
    console.log(`[digest] 🏆 Top 3 Preview:`);
    for (let i = 0; i < Math.min(3, finalArticles.length); i++) {
      const a = finalArticles[i];
      console.log(`  ${i + 1}. ${a.titleZh || a.title}`);
      console.log(`     ${a.summary.slice(0, 80)}...`);
    }
  }
}

await main().catch((err) => {
  console.error(`[digest] Fatal error: ${err instanceof Error ? err.message : String(err)}`);
  process.exit(1);
});
