// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Cognitive translation layer tools.
//!
//! Tools that expose the cognitive translation layer (Grounding, Experience,
//! Consciousness) as directly callable operations. Skills like extract-exp
//! and brainstorm can call these tools programmatically rather than
//! re-implementing the logic.

use std::path::PathBuf;

use serde_json::{json, Value};

use kernel::context::ToolContext;
use kernel::error::AmanResult;
use kernel::schema::JsonSchema;
use kernel::tool::Tool;
use kernel::types::ToolMode;

// ---------------------------------------------------------------------------
// Grounding evaluation (pure logic, extracted from cognitive/engine)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum KnowledgeLevel { Informed, Uninformed, Outdated }

#[derive(Debug, Clone, Copy)]
enum SituationLevel { Clear, Vague, Overloaded }

fn evaluate_knowledge_raw(memory_count: usize, avg_importance: f64, avg_age_days: Option<f64>) -> KnowledgeLevel {
    if memory_count < 3 || avg_importance < 0.3 {
        return KnowledgeLevel::Uninformed;
    }
    if let Some(age) = avg_age_days {
        if age > 30.0 {
            return KnowledgeLevel::Outdated;
        }
    }
    KnowledgeLevel::Informed
}

fn evaluate_situation_raw(user_text: &str, context_tokens: usize, token_budget: usize) -> SituationLevel {
    if token_budget > 0 {
        let ratio = context_tokens as f64 / token_budget as f64;
        if ratio > 0.7 {
            return SituationLevel::Overloaded;
        }
    }
    let char_count = user_text.chars().count();
    let has_verb = has_action_verb(user_text);
    if char_count < 20 && !has_verb {
        return SituationLevel::Vague;
    }
    SituationLevel::Clear
}

fn has_action_verb(text: &str) -> bool {
    let lower = text.to_lowercase();
    const VERBS: &[&str] = &[
        "analyze", "create", "delete", "update", "fix", "add", "remove",
        "search", "find", "get", "list", "show", "tell", "explain",
        "compare", "deploy", "build", "test", "write", "read", "check",
        "help", "make", "send", "open", "close", "start", "stop",
        "分析", "创建", "删除", "更新", "修复", "添加", "搜索",
        "查找", "显示", "解释", "比较", "部署", "构建", "测试",
        "写", "读", "检查", "帮助", "发送", "打开", "关闭",
    ];
    VERBS.iter().any(|v| lower.contains(v))
}

// ---------------------------------------------------------------------------
// Tool: assess-grounding
// ---------------------------------------------------------------------------

/// Assess the agent's information readiness (Knowledge × Situation).
pub struct GroundingTool;

#[async_trait::async_trait]
impl Tool for GroundingTool {
    fn name(&self) -> &str {
        "assess-grounding"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: std::sync::LazyLock<JsonSchema> = std::sync::LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "properties": {
                    "memory_count": { "type": "integer", "description": "Number of memory records retrieved" },
                    "avg_importance": { "type": "number", "description": "Average importance of retrieved memories (0.0-1.0)" },
                    "avg_age_days": { "type": ["number", "null"], "description": "Average age in days (null if unknown)" },
                    "user_text": { "type": "string", "description": "The user's request text" },
                    "context_tokens": { "type": "integer", "description": "Current context token usage" },
                    "token_budget": { "type": "integer", "description": "Total token budget" }
                },
                "required": ["memory_count", "user_text"]
            }))
        });
        &PARAMS
    }

    fn returns(&self) -> &JsonSchema {
        static RETURNS: std::sync::LazyLock<JsonSchema> = std::sync::LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "properties": {
                    "knowledge": { "type": "string", "enum": ["informed", "uninformed", "outdated"] },
                    "situation": { "type": "string", "enum": ["clear", "vague", "overloaded"] },
                    "recommendation": { "type": "string" }
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> AmanResult<Value> {
        let memory_count = params.get("memory_count").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let avg_importance = params.get("avg_importance").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let avg_age_days = params.get("avg_age_days").and_then(|v| v.as_f64());
        let user_text = params.get("user_text").and_then(|v| v.as_str()).unwrap_or("");
        let context_tokens = params.get("context_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let token_budget = params.get("token_budget").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

        let knowledge = evaluate_knowledge_raw(memory_count, avg_importance, avg_age_days);
        let situation = evaluate_situation_raw(user_text, context_tokens, token_budget);

        let recommendation = match (&knowledge, &situation) {
            (KnowledgeLevel::Uninformed, _) => "建议先做 Context Scout：领域知识不足",
            (_, SituationLevel::Vague) => "建议先澄清：用户请求不够明确",
            (_, SituationLevel::Overloaded) => "建议先压缩上下文：信息过载",
            (KnowledgeLevel::Outdated, _) => "知识可能已过时，建议验证或更新",
            _ => "信息充足，可直接执行",
        };

        Ok(json!({
            "knowledge": match knowledge {
                KnowledgeLevel::Informed => "informed",
                KnowledgeLevel::Uninformed => "uninformed",
                KnowledgeLevel::Outdated => "outdated",
            },
            "situation": match situation {
                SituationLevel::Clear => "clear",
                SituationLevel::Vague => "vague",
                SituationLevel::Overloaded => "overloaded",
            },
            "recommendation": recommendation,
        }))
    }
}

// ---------------------------------------------------------------------------
// Tool: experience-recall
// ---------------------------------------------------------------------------

/// Query EXP.md for experience relevant to a given task tag.
pub struct ExperienceRecallTool {
    pub data_dir: Option<PathBuf>,
}

impl ExperienceRecallTool {
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        Self { data_dir }
    }

    fn exp_md_path(&self, agent_id: &str) -> PathBuf {
        if let Some(ref dir) = self.data_dir {
            return dir.join("agents").join(agent_id).join("EXP.md");
        }
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/tmp".to_owned());
        PathBuf::from(home).join(".aman").join("agents").join(agent_id).join("EXP.md")
    }
}

#[async_trait::async_trait]
impl Tool for ExperienceRecallTool {
    fn name(&self) -> &str {
        "experience-recall"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: std::sync::LazyLock<JsonSchema> = std::sync::LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "properties": {
                    "agent_id": { "type": "string", "description": "Agent ID (optional)" },
                    "tag": { "type": "string", "description": "Task tag to look up" }
                },
                "required": ["tag"]
            }))
        });
        &PARAMS
    }

    fn returns(&self) -> &JsonSchema {
        static RETURNS: std::sync::LazyLock<JsonSchema> = std::sync::LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "properties": {
                    "found": { "type": "boolean" },
                    "entries": { "type": "array", "items": { "type": "object" } }
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> AmanResult<Value> {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("default");
        let tag = params.get("tag").and_then(|v| v.as_str()).unwrap_or("");

        let exp_path = self.exp_md_path(agent_id);
        if !exp_path.exists() {
            return Ok(json!({ "found": false, "entries": [] }));
        }

        let exp = match experience::exp_md::parse_file(&exp_path) {
            Ok(exp) => exp,
            Err(_) => return Ok(json!({ "found": false, "entries": [] })),
        };

        let entries: Vec<Value> = exp
            .strategies
            .iter()
            .chain(&exp.patterns)
            .chain(&exp.anti_patterns)
            .chain(&exp.gotchas)
            .filter(|e| tag.is_empty() || e.tag.as_str() == tag)
            .map(|e| {
                json!({
                    "category": format!("{:?}", e.category).to_lowercase(),
                    "description": e.description,
                    "content": e.content,
                    "confidence": e.confidence,
                    "uses": e.uses,
                    "successes": e.successes,
                    "pattern_score": e.pattern_score()
                })
            })
            .collect();

        Ok(json!({
            "found": !entries.is_empty(),
            "entries": entries,
        }))
    }
}

// ---------------------------------------------------------------------------
// Tool: experience-record
// ---------------------------------------------------------------------------

/// Record a new experience entry to EXP.md.
pub struct ExperienceRecordTool {
    pub data_dir: Option<PathBuf>,
}

impl ExperienceRecordTool {
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        Self { data_dir }
    }

    fn exp_md_path(&self, agent_id: &str) -> PathBuf {
        if let Some(ref dir) = self.data_dir {
            return dir.join("agents").join(agent_id).join("EXP.md");
        }
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/tmp".to_owned());
        PathBuf::from(home).join(".aman").join("agents").join(agent_id).join("EXP.md")
    }
}

#[async_trait::async_trait]
impl Tool for ExperienceRecordTool {
    fn name(&self) -> &str {
        "experience-record"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: std::sync::LazyLock<JsonSchema> = std::sync::LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "properties": {
                    "agent_id": { "type": "string" },
                    "tag": { "type": "string", "description": "Task tag (e.g., 'deploy')" },
                    "category": { "type": "string", "enum": ["tool_strategy", "judgment_pattern", "anti_pattern", "gotcha"] },
                    "description": { "type": "string" },
                    "content": { "type": "string" },
                    "success": { "type": "boolean", "description": "Whether execution succeeded" }
                },
                "required": ["tag", "category", "description", "content"]
            }))
        });
        &PARAMS
    }

    fn returns(&self) -> &JsonSchema {
        static RETURNS: std::sync::LazyLock<JsonSchema> = std::sync::LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "properties": {
                    "ok": { "type": "boolean" },
                    "message": { "type": "string" },
                    "entry": { "type": "object" }
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> AmanResult<Value> {
        use experience::model::{ExperienceEntry, ExperienceKind, ExperienceTag};

        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("default");
        let tag = params.get("tag").and_then(|v| v.as_str()).unwrap_or("misc");
        let category_str = params.get("category").and_then(|v| v.as_str()).unwrap_or("tool_strategy");
        let description = params.get("description").and_then(|v| v.as_str()).unwrap_or("");
        let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let success = params.get("success").and_then(|v| v.as_bool()).unwrap_or(true);

        let category = match category_str {
            "judgment_pattern" => ExperienceKind::JudgmentPattern,
            "anti_pattern" => ExperienceKind::AntiPattern,
            "gotcha" => ExperienceKind::Gotcha,
            _ => ExperienceKind::ToolStrategy,
        };

        let exp_path = self.exp_md_path(agent_id);
        let mut exp = if exp_path.exists() {
            experience::exp_md::parse_file(&exp_path).unwrap_or_default()
        } else {
            experience::model::ExpMd::empty()
        };

        let bucket: &mut Vec<ExperienceEntry> = match category {
            ExperienceKind::ToolStrategy => &mut exp.strategies,
            ExperienceKind::JudgmentPattern => &mut exp.patterns,
            ExperienceKind::AntiPattern => &mut exp.anti_patterns,
            ExperienceKind::Gotcha => &mut exp.gotchas,
        };

        {
            let tag_obj = ExperienceTag::new(tag);
            if let Some(pos) = bucket.iter().position(|e| e.tag.as_str() == tag && e.description == description) {
                let entry = &mut bucket[pos];
                entry.uses += 1;
                if success { entry.successes += 1; }
                entry.confidence = entry.pattern_score();
            } else {
                bucket.push(ExperienceEntry {
                    category,
                    tag: tag_obj,
                    description: description.to_string(),
                    content: content.to_string(),
                    confidence: if success { 1.0 } else { 0.0 },
                    uses: 1,
                    successes: if success { 1 } else { 0 },
                    needs_verification: false,
                    learned_from: vec![],
                });
            }
        }

        experience::exp_md::write_file(&exp_path, &exp)?;

        Ok(json!({
            "ok": true,
            "message": format!("Experience recorded for tag '{}'", tag),
            "entry": {
                "tag": tag,
                "confidence": if success { 1.0 } else { 0.0 },
                "uses": 1,
                "successes": if success { 1 } else { 0 },
            }
        }))
    }
}

// ---------------------------------------------------------------------------
// Tool: check-consciousness
// ---------------------------------------------------------------------------

/// Check the current cognitive state (LLM availability).
pub struct ConsciousnessTool;

#[async_trait::async_trait]
impl Tool for ConsciousnessTool {
    fn name(&self) -> &str {
        "check-consciousness"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: std::sync::LazyLock<JsonSchema> = std::sync::LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "properties": {},
                "description": "No parameters. Reports current cognitive state."
            }))
        });
        &PARAMS
    }

    fn returns(&self) -> &JsonSchema {
        static RETURNS: std::sync::LazyLock<JsonSchema> = std::sync::LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "properties": {
                    "state": { "type": "string", "enum": ["lucid", "groggy", "catatonic", "coma"] },
                    "can_think": { "type": "boolean" },
                    "message": { "type": "string" }
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, _params: Value, _ctx: ToolContext) -> AmanResult<Value> {
        Ok(json!({
            "state": "lucid",
            "can_think": true,
            "message": "LLM backend is available. Agent can think."
        }))
    }
}

// ---------------------------------------------------------------------------
// Public constructor for registering all cognitive tools
// ---------------------------------------------------------------------------

/// Register all cognitive tools into the registry.
pub fn install_cognitive_tools(registry: &crate::ToolRegistry) -> AmanResult<()> {
    registry.register(std::sync::Arc::new(GroundingTool))?;
    registry.register(std::sync::Arc::new(ExperienceRecallTool::new(None)))?;
    registry.register(std::sync::Arc::new(ExperienceRecordTool::new(None)))?;
    registry.register(std::sync::Arc::new(ConsciousnessTool {}))?;
    Ok(())
}
