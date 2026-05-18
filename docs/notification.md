# Notification Center

User-facing alert system for the Aman agent runtime. Provides severity-classed
notifications (critical/warning) that appear as in-app overlays in the Tauri UI.

## Architecture

```
EventBus ──→ NotificationSubscriber → NotificationStore ──→ HTTP GET /notifications
                                         ↓
                                     Tauri polling (2s)
                                         ↓
                               NotificationOverlay.svelte
                               NotificationBell.svelte
```

## Notification Model

Defined in `crates/notification/src/model.rs`:

```rust
pub enum Severity { Critical, Warning }

pub struct Notification {
    pub id: String,
    pub severity: Severity,
    pub category: Category,       // Backpressure, Security, Llm, Workflow, etc.
    pub title: String,
    pub message: String,
    pub dismissed: bool,
    pub dismissible: bool,        // Critical = false, Warning = true
    pub action_label: Option<String>,  // Button text, e.g. "View DLQ"
    pub action_route: Option<String>,  // Frontend route, e.g. "/dlq"
    pub event_id: Option<String>,
    pub source: Option<String>,
}
```

## Severity Levels

| Severity | Color | Auto-dismiss | Dismissible | Action Required |
|---|---|---|---|---|
| Critical | Red (#DC2626) | No | No | Must acknowledge |
| Warning | Yellow (#EAB308) | Yes (5s) | Yes | Optional dismiss |

## Notification Rules

### Critical (must acknowledge)
| Trigger | Title |
|---|---|
| Backpressure L4B `OverflowDirEmergency` | "系统严重过载：磁盘容量警告" |
| `InjectionDetected` | "检测到提示注入攻击" |
| Plugin load/unload failure | "插件 {name} 加载失败" |
| WAL checkpoint failure | "WAL 检查点写入失败" |
| LLM continuous errors (3× in 60s) | "LLM 持续故障" |

### Warning (dismissible)
| Trigger | Title |
|---|---|
| Single `llm_error` | "LLM 调用失败" |
| `output_blocked` | "输出被安全策略拦截" |
| Workflow → ERROR state | "工作流进入错误状态" |
| Session timeout → CLOSED | "会话因超时已关闭" |
| Backpressure L3 | "事件队列拥挤" |
| OverflowedToDisk | "事件溢出到磁盘" |
| DroppedAtMostOnce | "部分事件已丢弃" |
| DLQ impending expiry (7d/3d/1d) | "DLQ 条目即将过期" |
| `message_dropped` (queue full) | "消息被丢弃" |
| Secret rotation failure | "密钥轮换失败" |
| Skill reload failure | "技能 {name} 加载失败" |
| Skill bulk change (≥3 removed) | "多个技能文件变更" |
| Plugin enable/disable failure | "插件操作失败" |
| Retry exhausted (max_retry_count) | "工作流重试已耗尽" |

## HTTP API

| Method | Route | Description |
|---|---|---|
| `GET` | `/notifications` | List, filter: `?active_only=true&severity=critical&limit=20` |
| `GET` | `/notifications/unread-count` | `{"count": 3}` |
| `POST` | `/notifications/{id}/dismiss` | Dismiss a warning notification |
| `POST` | `/notifications/{id}/ack` | Acknowledge a critical notification |
| `POST` | `/notifications/dismiss-all` | Dismiss all dismissible |

## Storage

In-memory ring buffer (cap=500), oldest evicted when full. Not persisted
across restarts. This is intentional — notifications are transient alerts,
not an audit log.

## Frontend Components

- **NotificationOverlay.svelte** — top-center banner overlay, peon-ping style
  - Critical: red, persistent, must click acknowledge
  - Warning: yellow, auto-dismiss after 5s, click × to dismiss
  
- **NotificationBell.svelte** — sidebar icon with unread count badge
  - Click opens dropdown notification list
  - "全部已读" button to dismiss all

## Crate Structure

```
crates/notification/
├── Cargo.toml
└── src/
    ├── lib.rs           # Public API
    ├── model.rs         # Notification, Severity, Category
    ├── store.rs         # NotificationStore (ring buffer)
    └── subscriber.rs    # NotificationSubscriber (EventBus → store)
```

## Future Work

- Persistent notification history (SQLite)
- User-configured notification preferences (which events, per-severity routing)
- Push notifications via peon-ping relay for critical events
