// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use crate::types::Timestamp;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// Agent 运行时标识与配置。
///
/// 对应 config.yaml 中 agents 段的单个 Agent 条目。
/// AgentRegistry 在 Phase 2 从配置加载后为每个条目创建一个 AgentInstance。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDescriptor {
    /// config.yaml 中的 agent key（唯一标识）
    pub agent_id: String,

    /// UI 显示名
    pub display_name: String,

    /// provider key（必须存在于 providers 段）
    pub provider: String,

    /// 模型名
    pub model: String,

    /// SOUL 文件路径（可选，缺省使用框架级 SOUL）
    pub soul_path: Option<PathBuf>,

    /// 该 Agent 可用的 Tool 列表
    /// None = 全部可用，Some(vec) = 白名单
    pub allowed_tools: Option<Vec<String>>,

    /// 该 Agent 显式拒绝的 Tool 列表（在黑名单之上进一步限制）
    pub denied_tools: Vec<String>,

    /// 该 Agent 可用的 Skill 列表
    /// None = 全部可用，Some(vec) = 白名单
    pub allowed_skills: Option<Vec<String>>,

    /// 配置中是否启用
    pub enabled: bool,

    /// Model max context window in tokens (from provider.models.<model>.max_context_tokens).
    /// None = use hardcoded lookup in TokenBudget::new().
    pub max_context_tokens: Option<usize>,

    /// Model max output tokens per response.
    /// None = default to 4096.
    pub max_output_tokens: Option<usize>,
}

/// 拟人系统状态 — 表示当前哪个拟人系统在掌控 agent。
///
/// 每个系统在进入/退出时原子更新此状态，UI 可直接读取显示。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentSystemState {
    /// idle 系统掌控中（默认状态）
    #[default]
    Idle,
    /// work 系统掌控中
    Working,
    /// chat 对话中
    Chatting,
    /// study 系统掌控中
    Studying,
    /// daily-life 系统掌控中
    DailyLife,
    /// 等待长时任务完成（如 detached 子进程）
    Waiting,
}

/// Agent 运行时状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStatus {
    /// 配置中禁用
    Disabled,
    /// 已加载，无活跃会话
    Idle,
    /// 有活跃会话正在处理
    Busy,
    /// 初始化失败或运行时异常
    Error,
}

/// Agent 运行时实例，由 AgentRegistry 管理。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInstance {
    pub descriptor: AgentDescriptor,
    pub status: AgentStatus,
    /// 当前活跃的 session_id（Busy 状态时）
    pub active_session_id: Option<String>,
    /// 注册到 Registry 的时间
    pub registered_at: Timestamp,
    /// 当前拟人系统状态（idle / work / …）
    #[serde(default)]
    pub system_state: AgentSystemState,
    /// 当前正在做什么（如 "Thinking..."、"Calling tool: grep"）。
    /// 用于 UI 提示，防止用户以为 agent 卡死。
    #[serde(default)]
    pub activity: String,
}

impl AgentInstance {
    #[must_use]
    pub fn new(descriptor: AgentDescriptor) -> Self {
        let status = if descriptor.enabled {
            AgentStatus::Idle
        } else {
            AgentStatus::Disabled
        };
        Self {
            descriptor,
            status,
            active_session_id: None,
            registered_at: Timestamp::now(),
            system_state: AgentSystemState::default(),
            activity: String::new(),
        }
    }
}

/// Agent 级别的事件类型。
///
/// 这些事件由 AgentRegistry 和 AgentHarness 在关键操作点发布，
/// 遵循 aman "万物皆事件"的设计公理。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentEvent {
    Registered(AgentDescriptor),
    StatusChanged {
        agent_id: String,
        old_status: AgentStatus,
        new_status: AgentStatus,
    },
    Removed {
        agent_id: String,
        reason: String,
    },
}

impl AgentEvent {
    /// 返回与此事件对应的 Custom EventType 字符串。
    #[must_use]
    pub fn event_type_str(&self) -> &'static str {
        match self {
            Self::Registered(_) => "agent:registered",
            Self::StatusChanged { .. } => "agent:status_changed",
            Self::Removed { .. } => "agent:removed",
        }
    }
}

/// Content type for agent-to-agent messages (T7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMessageType {
    /// Delegate a task to another agent.
    TaskDelegation,
    /// Share results with another agent.
    ResultSharing,
    /// Query another agent's status.
    StatusQuery,
}

/// Standard format for agent-to-agent communication (T7.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub message_id: Uuid,
    pub from_agent: String,
    pub to_agent: String,
    pub content_type: AgentMessageType,
    pub payload: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<Uuid>,
}
