// ---------------------------------------------------------------------------
// Enhanced markdown renderer — syntax highlighting + code block headers
// ---------------------------------------------------------------------------
//
// Wraps `marked` with post-processing that:
//  1. Applies highlight.js syntax highlighting to code blocks
//  2. Adds a header bar (language label + copy button) above each block
//  3. Supports diff view (+/- lines) when language is "diff"
//  4. Caps code block height with internal scroll + bottom fade-out

import "highlight.js/styles/github-dark.css";
import { marked } from "marked";
import { t } from "./i18n.svelte";
import hljs from "highlight.js/lib/core";
import bash from "highlight.js/lib/languages/bash";
import c from "highlight.js/lib/languages/c";
import css from "highlight.js/lib/languages/css";
import diff from "highlight.js/lib/languages/diff";
import javascript from "highlight.js/lib/languages/javascript";
import json from "highlight.js/lib/languages/json";
import python from "highlight.js/lib/languages/python";
import rust from "highlight.js/lib/languages/rust";
import sql from "highlight.js/lib/languages/sql";
import typescript from "highlight.js/lib/languages/typescript";
import xml from "highlight.js/lib/languages/xml";
import yaml from "highlight.js/lib/languages/yaml";

// Register commonly used languages (tree-shakeable — only these ship)
hljs.registerLanguage("bash", bash);
hljs.registerLanguage("sh", bash);
hljs.registerLanguage("shell", bash);
hljs.registerLanguage("c", c);
hljs.registerLanguage("css", css);
hljs.registerLanguage("diff", diff);
hljs.registerLanguage("javascript", javascript);
hljs.registerLanguage("js", javascript);
hljs.registerLanguage("json", json);
hljs.registerLanguage("python", python);
hljs.registerLanguage("py", python);
hljs.registerLanguage("rust", rust);
hljs.registerLanguage("rs", rust);
hljs.registerLanguage("sql", sql);
hljs.registerLanguage("typescript", typescript);
hljs.registerLanguage("ts", typescript);
hljs.registerLanguage("xml", xml);
hljs.registerLanguage("html", xml);
hljs.registerLanguage("yaml", yaml);
hljs.registerLanguage("yml", yaml);

// ---------------------------------------------------------------------------
// Diff line helpers
// ---------------------------------------------------------------------------

function highlightDiffLines(code: string, lang: string): string {
  if (lang !== "diff") return hljsHighlightSafe(code, lang);

  // For diffs, preserve the +/- markers and apply line-level classes
  const lines = code.split("\n");
  const escaped: string[] = [];
  for (const line of lines) {
    if (line.startsWith("+") && !line.startsWith("+++")) {
      escaped.push(`<span class="diff-add">${escapeHtml(line)}</span>`);
    } else if (line.startsWith("-") && !line.startsWith("---")) {
      escaped.push(`<span class="diff-remove">${escapeHtml(line)}</span>`);
    } else if (line.startsWith("@@")) {
      escaped.push(`<span class="diff-hunk">${escapeHtml(line)}</span>`);
    } else {
      escaped.push(escapeHtml(line));
    }
  }
  return escaped.join("\n");
}

function hljsHighlightSafe(code: string, lang: string): string {
  if (lang && hljs.getLanguage(lang)) {
    try {
      const result = hljs.highlight(code, { language: lang });
      return result.value;
    } catch {
      // fall through to escape
    }
  }
  // Auto-detect
  try {
    const result = hljs.highlightAuto(code);
    return result.value;
  } catch {
    return escapeHtml(code);
  }
}

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

// ---------------------------------------------------------------------------
// Language label (human-readable)
// ---------------------------------------------------------------------------

const LANG_LABELS: Record<string, string> = {
  js: "JavaScript",
  javascript: "JavaScript",
  ts: "TypeScript",
  typescript: "TypeScript",
  py: "Python",
  python: "Python",
  rs: "Rust",
  rust: "Rust",
  bash: "Bash",
  sh: "Shell",
  shell: "Shell",
  json: "JSON",
  yaml: "YAML",
  yml: "YAML",
  sql: "SQL",
  css: "CSS",
  html: "HTML",
  xml: "XML",
  diff: "Diff",
  c: "C",
  md: "Markdown",
  markdown: "Markdown",
};

function langLabel(lang: string): string {
  if (!lang) return "code";
  return LANG_LABELS[lang.toLowerCase()] ?? lang;
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

export interface RenderResult {
  html: string;
}

/**
 * Parse markdown and enhance code blocks with:
 *  - Syntax highlighting (highlight.js)
 *  - Header bar: language label + copy button
 *  - Max height + internal scroll + bottom fade-out
 *  - Diff view (+/- lines with green/red backgrounds)
 */
export function renderMarkdown(src: string): string {
  // Step 1: standard marked parse
  const raw = marked.parse(src, { gfm: true, breaks: true }) as string;

  // Step 2: post-process <pre><code> blocks
  return postProcessCodeBlocks(raw);
}

function postProcessCodeBlocks(html: string): string {
  // Match <pre><code class="language-xxx">...</code></pre>
  // Also match <pre><code> without a language class
  const preRe = /<pre><code(?:\s+class="language-(\w*)")?>([\s\S]*?)<\/code><\/pre>/g;

  return html.replace(preRe, (_match, lang: string, code: string) => {
    // Decode HTML entities that marked may have encoded
    const decoded = decodeHtmlEntities(code);
    const label = langLabel(lang || "");

    // Highlight
    let highlighted: string;
    if (lang === "diff") {
      highlighted = highlightDiffLines(decoded, lang);
    } else {
      highlighted = hljsHighlightSafe(decoded, lang || "");
    }

    // Unique id for copy button
    const blockId = "code-" + Math.random().toString(36).slice(2, 9);

    return `
<div class="code-block-wrapper">
  <div class="code-block-header">
    <span class="code-block-lang">${escapeHtml(label)}</span>
    <button class="code-block-copy-btn" data-code-id="${blockId}" title="${t("chat.copy")}">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <rect x="9" y="9" width="13" height="13" rx="2" ry="2"/>
        <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/>
      </svg>
      <span>${t("chat.copy")}</span>
    </button>
  </div>
  <pre class="code-block-pre"><code class="language-${escapeHtml(lang || "")}">${highlighted}</code></pre>
  <div class="code-block-fade"></div>
</div>`;
  });
}

function decodeHtmlEntities(text: string): string {
  return text
    .replace(/&amp;/g, "&")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&apos;/g, "'");
}
