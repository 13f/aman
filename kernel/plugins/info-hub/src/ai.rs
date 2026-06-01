//! AI processing: scoring, summarization, and highlights generation.
//!
//! Uses the LLM configured via `memory.llm` in aman config. Makes
//! OpenAI-compatible chat completion calls via `cognitive_llm`. No provider-specific logic.

use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::Value;
use tracing::debug;

// Re-export LLM API primitives from the shared crate.
pub use cognitive_llm::simple::LlmApiConfig as LlmConfig;
pub use cognitive_llm::simple::parse_json_response;
use cognitive_llm::simple::SimpleLlmClient;

const DESCRIPTION_MAX_LEN: usize = 384;

// ── Types ───────────────────────────────────────────────────────────

/// One article sent to the scoring/summarization tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArticleInput {
    pub index: usize,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub source_name: String,
    #[serde(default)]
    pub link: String,
    /// Pre-assigned category from tagging step (used by scoring prompt for context).
    #[serde(default)]
    pub category: String,
    /// Pre-assigned keywords from tagging step (used by scoring prompt for context).
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Score from prior scoring step (used by summarizer to skip low-score articles).
    #[serde(default)]
    pub relevance: u32,
    #[serde(default)]
    pub quality: u32,
    #[serde(default)]
    pub timeliness: u32,
}

impl ArticleInput {
    pub fn total_score(&self) -> u32 {
        self.relevance + self.quality + self.timeliness
    }
}

/// Score result for a single article.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreResult {
    pub index: usize,
    pub relevance: u32,
    pub quality: u32,
    pub timeliness: u32,
}

/// Tag result for a single article (category + keywords only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagResult {
    pub index: usize,
    pub category: String,
    pub keywords: Vec<String>,
}

/// Summary result for a single article.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryResult {
    pub index: usize,
    pub title_zh: String,
    pub summary: String,
    pub reason: String,
}

// ── LLM Client (delegates to cognitive-llm) ─────────────────────────

/// One-shot chat completion via the shared `SimpleLlmClient`.
pub async fn chat_completion(
    config: &LlmConfig,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f64,
    max_tokens: u64,
    timeout_secs: u64,
) -> Result<String, String> {
    SimpleLlmClient::new().chat_completion(config, system_prompt, user_prompt, temperature, max_tokens, timeout_secs).await
}

/// Chat completion with retries via the shared `SimpleLlmClient`.
pub async fn chat_completion_with_retries(
    config: &LlmConfig,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f64,
    max_tokens: u64,
    timeout_secs: u64,
    retries: u32,
) -> Result<String, String> {
    SimpleLlmClient::new().chat_completion_with_retries(config, system_prompt, user_prompt, temperature, max_tokens, timeout_secs, retries).await
}

// ── Prompt Templates ────────────────────────────────────────────────

pub fn build_scoring_prompt(
    articles: &[ArticleInput],
) -> (String, String) {
    let system = "你是一个技术内容策展人，正在为一份面向技术爱好者的每日精选摘要筛选文章。".to_string();

    let articles_list = articles
        .iter()
        .map(|a| {
            let tag_line = if !a.category.is_empty() || !a.keywords.is_empty() {
                let kw_str = a.keywords.join(", ");
                if !a.category.is_empty() && !kw_str.is_empty() {
                    format!("[{} | {}] ", a.category, kw_str)
                } else if !a.category.is_empty() {
                    format!("[{}] ", a.category)
                } else {
                    format!("[{}] ", kw_str)
                }
            } else {
                String::new()
            };
            format!(
                "Index {}: {tag_line}[{source}] {title}\n{desc}",
                a.index,
                source = a.source_name,
                title = a.title,
                desc = truncate_description(&a.description, DESCRIPTION_MAX_LEN),
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");

    let user = format!(
        r#"请对以下文章进行三个维度的评分（1-10 整数，10 分最高）。

每篇文章前的 [category | keywords] 标签已预先标注，评分时请结合这些标签判断文章在所属领域内的价值。

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

## 待评分文章

{articles_list}

请严格按 JSON 格式返回，不要包含 markdown 代码块或其他文字：
{{
  "results": [
    {{
      "index": 0,
      "relevance": 8,
      "quality": 7,
      "timeliness": 9
    }}
  ]
}}"#
    );

    (system, user)
}

pub fn build_tagging_prompt(
    articles: &[ArticleInput],
) -> (String, String) {
    let system = "你是一个技术内容分类专家，负责快速识别文章所属的行业/领域。".to_string();

    let articles_list = articles
        .iter()
        .map(|a| {
            format!(
                "Index {}: [{}] {}\n{}",
                a.index,
                a.source_name,
                a.title,
                truncate_description(&a.description, DESCRIPTION_MAX_LEN)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");

    let user = format!(
        r#"请为每篇文章分配一个分类标签，并提取 1-3 个关键词。

## 分类标签
根据文章内容自由选择一个最合适的分类标签（用英文，简短，如 "ai-ml", "security", "engineering", "tools", "opinion", "linux", "rust", "database", "frontend", "career" 等，也可以自创更精确的分类）。

## 关键词提取
提取 1-3 个最能代表文章主题的关键词（用英文，简短，如 "Rust", "LLM", "database", "performance"）。

## 待分类文章

{articles_list}

请严格按 JSON 格式返回，不要包含 markdown 代码块或其他文字：
{{
  "results": [
    {{
      "index": 0,
      "category": "engineering",
      "keywords": ["Rust", "compiler"]
    }}
  ]
}}"#
    );

    (system, user)
}

pub fn build_summary_prompt(
    articles: &[ArticleInput],
    lang: &str,
) -> (String, String) {
    let system = "你是一个技术内容摘要专家。".to_string();

    let articles_list = articles
        .iter()
        .map(|a| {
            format!(
                "Index {}: [{}] {}\n{}",
                a.index,
                a.source_name,
                a.title,
                truncate_description(&a.description, 600)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");

    let lang_instruction = if lang == "zh" {
        "请用中文撰写摘要和推荐理由。如果原文是英文，请翻译为中文。标题翻译也用中文。"
    } else {
        "Write summaries, reasons, and title translations in English."
    };

    let user = format!(
        r#"请为以下文章完成三件事：

1. **中文标题** (title_zh): 将英文标题翻译成自然的中文。如果原标题已经是中文则保持不变。
2. **摘要** (summary): 4-6 句话的结构化摘要，让读者不点进原文也能了解核心内容。包含：
   - 文章讨论的核心问题或主题（1 句）
   - 关键论点、技术方案或发现（2-3 句）
   - 结论或作者的核心观点（1 句）
3. **推荐理由** (reason): 1 句话说明"为什么值得读"，区别于摘要（摘要说"是什么"，推荐理由说"为什么"）。

{lang_instruction}

摘要要求：
- 直接说重点，不要用"本文讨论了..."、"这篇文章介绍了..."这种开头
- 包含具体的技术名词、数据、方案名称或观点
- 保留关键数字和指标（如性能提升百分比、用户数、版本号等）
- 如果文章涉及对比或选型，要点出比较对象和结论
- 目标：读者花 30 秒读完摘要，就能决定是否值得花 10 分钟读原文

## 待摘要文章

{articles_list}

请严格按 JSON 格式返回：
{{
  "results": [
    {{
      "index": 0,
      "title_zh": "中文翻译的标题",
      "summary": "摘要内容...",
      "reason": "推荐理由..."
    }}
  ]
}}"#
    );

    (system, user)
}

pub fn build_highlights_prompt(
    articles_json: &str,
    lang: &str,
) -> (String, String) {
    let system = "你是一个技术趋势分析专家。".to_string();
    let lang_note = if lang == "zh" {
        "用中文回答。"
    } else {
        "Write in English."
    };

    let user = format!(
        r#"根据以下今日精选技术文章列表，写一段 3-5 句话的"今日看点"总结。
要求：
- 提炼出今天技术圈的 2-3 个主要趋势或话题
- 不要逐篇列举，要做宏观归纳
- 风格简洁有力，像新闻导语
{lang_note}

文章列表：
{articles_json}

直接返回纯文本总结，不要 JSON，不要 markdown 格式。"#
    );

    (system, user)
}

// ── Helpers ─────────────────────────────────────────────────────────

fn truncate_description(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_string();
    }
    let sliced = &text[..max_len];
    // Try to break at a sentence boundary
    if let Some(pos) = sliced.rfind(['.', '!', '?', '。', '！', '？']) {
        return sliced[..=pos].trim_end().to_string();
    }
    // Fallback to last space
    if let Some(pos) = sliced.rfind(' ')
        && pos > max_len * 3 / 5
    {
        return sliced[..pos].to_string();
    }
    sliced.to_string()
}

/// Process articles in batches with bounded concurrency.
pub async fn process_batches<T, F, R>(
    items: &[T],
    batch_size: usize,
    max_concurrent: usize,
    f: F,
) -> Vec<R>
where
    T: Send + Sync,
    F: Fn(Vec<&T>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<R>> + Send>> + Send + Sync + 'static,
    R: Send + 'static,
{
    let mut results: Vec<R> = Vec::new();
    let batches: Vec<Vec<&T>> = items
        .chunks(batch_size)
        .map(|c| c.iter().collect())
        .collect();

    debug!(items = items.len(), batches = batches.len(), "info-hub batch processing");

    for batch_group in batches.chunks(max_concurrent) {
        let tasks: Vec<_> = batch_group
            .iter()
            .map(|batch| {
                let batch: Vec<&T> = batch.to_vec();
                f(batch)
            })
            .collect();

        let group_results = futures::future::join_all(tasks).await;
        for r in group_results {
            results.extend(r);
        }
    }

    results
}

/// Clamp a score to 1..=10
pub fn clamp_score(v: i64) -> u32 {
    v.clamp(1, 10) as u32
}

/// Simple truncation helper (public for use in tool fallback messages).
pub fn truncate_str(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_string();
    }
    let sliced = &text[..max_len];
    if let Some(pos) = sliced.rfind(['.', '!', '?', '。', '！', '？']) {
        return sliced[..=pos].trim_end().to_string();
    }
    if let Some(pos) = sliced.rfind(' ') {
        return sliced[..pos].to_string();
    }
    sliced.to_string()
}

pub const VALID_CATEGORIES: &[&str] = &[
    "ai-ml", "security", "engineering", "tools", "opinion", "other",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_limit() {
        let short = "hello";
        assert_eq!(truncate_description(short, 20), "hello");
        let long = "a".repeat(100);
        assert!(truncate_description(&long, 50).len() <= 50);
    }

    #[test]
    fn truncate_breaks_at_sentence() {
        let text = "First sentence. Second sentence that is very long.";
        let truncated = truncate_description(text, 30);
        assert!(truncated.ends_with('.'));
        assert!(!truncated.contains("Second"));
    }

    #[test]
    fn parse_json_extracts_from_markdown_fence() {
        let raw = "```json\n{\"key\": \"value\"}\n```";
        let parsed: Value = parse_json_response(raw).unwrap();
        assert_eq!(parsed["key"], "value");
    }

    #[test]
    fn parse_json_repairs_truncated() {
        let raw = "{\"results\": [{\"index\": 0}";
        let parsed: Value = parse_json_response(raw).unwrap();
        assert_eq!(parsed["results"][0]["index"], 0);
    }

    #[test]
    fn clamp_score_bounds() {
        assert_eq!(clamp_score(0), 1);
        assert_eq!(clamp_score(5), 5);
        assert_eq!(clamp_score(11), 10);
    }
}
