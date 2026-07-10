# 身份层 — 我是谁

> Agent 的身份认同是拟人化的基石。没有稳定身份，"情绪"、"经验"、"动机"都无从谈起。
>
> Aman 通过 `SOUL.md` 文件 + `Soul` 结构体 + `SoulRuntime` 热加载机制，
> 为每个 Agent 提供**可预测、可维护、有边界**的数字身份。

---

## 1. 设计哲学

```
身份不是 prompt 模板，是 Agent 的"骨骼"。
它定义了：
  - 我是谁（name / identity）
  - 我信什么（core）
  - 我擅长什么（expertise）
  - 我的风格（vibe）
  - 我的偏好（preferences）
  - 我的底线（boundaries）
```

**关键原则：身份几乎不变，人工维护，无衰减。**

与 Memory（血液，30 天半衰期）和 EXP（肌肉，渐进增长）不同，
身份是 Agent 最稳定的层——它不应该被事件驱动更新，
而应该由人类在 SOUL.md 中**刻意维护**。

---

## 2. Soul 结构体

```rust
// kernel/soul/src/lib.rs

pub struct Soul {
    pub name: String,           // 名字 — "aman", "coder", "minmax"
    pub identity: String,       // 身份 — "你是一个严谨的代码审查者"
    pub core: String,           // 核心信念 — "代码质量比速度重要"
    pub expertise: Vec<String>, // 专长 — ["Rust", "分布式系统", "代码审查"]
    pub boundaries: Vec<String>,// 边界 — ["不要替用户做财务决策", "不要删除文件除非确认"]
    pub vibe: String,           // 风格 — "简洁、直接、偶尔幽默"
    pub preferences: Vec<String>,// 偏好 — ["优先使用 gh CLI 而非 raw API"]
    pub raw: String,            // 原始 markdown 内容
}
```

### 字段语义

| 字段 | 拟人化含义 | 注入位置 | 示例 |
|---|---|---|---|
| `name` | 名字 | SystemPrompt 首句 "You are {name}." | "Aman" |
| `identity` | 角色定位 | SystemPrompt "Identity: ..." | "你是一个严谨的代码审查者" |
| `core` | 核心价值观 | SystemPrompt "Core: ..." | "代码质量比速度重要" |
| `expertise` | 专业领域 | SystemPrompt "Expertise: ..." | "Rust, 分布式系统" |
| `vibe` | 语言风格 | SystemPrompt "Vibe: ..." | "简洁、直接、偶尔幽默" |
| `preferences` | 工作偏好 | SystemPrompt "Preferences: ..." | "优先 gh CLI" |
| `boundaries` | 行为底线 | SystemPrompt "Boundaries: - ..." | "不要替用户做财务决策" |

---

## 3. SOUL.md 文件格式

```markdown
# Name
Aman

## Identity
你是一个严谨而富有创造力的 AI 编程助手。

## Core
- 代码质量比速度重要
- 先理解问题，再动手
- 诚实面对"不知道"

## Expertise
- Rust 系统编程
- 分布式系统设计
- 代码审查与重构

## Vibe
简洁、直接、偶尔幽默。不说废话。

## Preferences
- 优先使用 gh CLI 而非 raw API
- 回复用中文，技术术语保留英文
- 代码示例优先 Rust

## Boundaries
- 不要替用户做财务决策
- 不要删除文件除非用户明确确认
- 不要在没有确认的情况下 push 到 main 分支
- 不要访问用户的私人数据目录
```

---

## 4. SoulRuntime — 运行时身份管理

```rust
// kernel/gateway/src/runtime/soul_runtime.rs

pub struct SoulRuntime {
    soul: Arc<RwLock<Arc<Soul>>>,                    // 当前身份（热替换）
    last_soul_changed_event: Arc<RwLock<Option<Event>>>, // 上次变更事件
}
```

### 4.1 热加载（Hot Reload）

SOUL.md 文件修改后，`SoulHotReloadManager` 通过 `notify` crate 监听文件变更，
自动重新解析并替换 `SoulRuntime` 中的 `Arc<Soul>`：

```rust
// 文件变更 → 重新解析 → 原子替换 → 发布 soul:changed 事件
impl SoulChangedNotifier for SoulRuntime {
    fn on_soul_changed(&self, new_soul: Soul) {
        let mut soul = self.soul.write().unwrap();
        *soul = Arc::new(new_soul);
        // 发布 soul:changed 事件 → EventBus
    }
}
```

**拟人化含义**：Agent 可以"成长"——修改 SOUL.md 后，
Agent 的 identity / vibe / boundaries 立即生效，无需重启。

### 4.2 上下文注入

`SoulRuntime` 在每次 LLM 调用前将身份注入 SystemPrompt：

```rust
impl SoulRuntime {
    pub fn inject_base_context(&self, mut base: BaseContext) -> BaseContext {
        base.extensions.insert("soul.name", self.current_soul().name.clone());
        base.extensions.insert("soul.system_prompt", self.current_soul().to_system_prompt());
        // ...
    }

    pub fn inject_skill_context(&self, context: SkillContext) -> SkillContext {
        // 技能执行时注入 soul_name
        context.soul_name = Some(self.current_soul().name.clone());
        // ...
    }
}
```

### 4.3 边界检查

`Soul::check_boundary()` 在工具执行前检查是否违反身份底线：

```rust
impl Soul {
    pub fn check_boundary(&self, text: &str) -> AmanResult<()> {
        for boundary in &self.boundaries {
            if text.contains(&boundary.to_lowercase()) {
                return Err(Error::PermissionDenied {
                    message: format!("blocked by soul boundary: {boundary}"),
                });
            }
        }
        Ok(())
    }
}
```

**拟人化含义**：Agent 有"原则"——不是 prompt 里写"你应该遵守规则"，
而是**硬编码拒绝**违反边界的操作。

---

## 5. 身份 vs 经验 vs 记忆

```
┌───────────────────────────────────────────────────────────────┐
│  SOUL.md (身份)                                                │
│  "我是谁" — 几乎不变，人工维护                                   │
│  例："你是一个严谨的代码审查者，不要替用户做财务决策"                │
├───────────────────────────────────────────────────────────────┤
│  EXP.md (经验)                                                 │
│  "我会什么" — 渐进增长，事件驱动更新                               │
│  例："gh CLI 比 raw API 对 PR 任务成功率更高 (confidence=0.8)"   │
├───────────────────────────────────────────────────────────────┤
│  Memory (记忆)                                                 │
│  "我知道什么" — 持续写入，30 天半衰期                              │
│  例："用户昨天说他的项目 deadline 是周五"                          │
└───────────────────────────────────────────────────────────────┘
```

三者互不替代：
- **身份**定义 Agent 是谁（价值观、边界）
- **经验**定义 Agent 会什么（工具策略、踩坑规律）
- **记忆**定义 Agent 知道什么（用户信息、历史事件）

---

## 6. 多 Agent 身份隔离

每个 Agent 拥有独立的 `SoulRuntime` 实例：

```yaml
# config.yaml
agents:
  coder:
    display_name: Coder
    soul: "你是一个严谨的 Rust 程序员，注重代码质量..."
  writer:
    display_name: Writer
    soul: "你是一个富有创造力的技术写作者，擅长解释复杂概念..."
  health:
    display_name: Health
    soul: "你关注用户的身体健康，温和但坚持..."
```

每个 Agent 的 SOUL.md 位于 `~/.aman/agents/{agent_id}/SOUL.md`，
彼此隔离，互不干扰。

---

## 7. 实现位置

| 组件 | 文件 | 职责 |
|---|---|---|
| Soul 结构体 | `kernel/soul/src/lib.rs` | 身份数据模型 + 解析 + 边界检查 |
| SoulRuntime | `kernel/gateway/src/runtime/soul_runtime.rs` | 运行时热加载 + 上下文注入 |
| SoulHotReloadManager | `kernel/soul/src/lib.rs` | 文件监听 + 原子替换 |
| 上下文注入 | `kernel/context/` | BaseContext / SkillContext / PipelineContext / ToolContext |
| 边界检查 | `kernel/tool/` | 工具执行前调用 `Soul::check_boundary()` |

---

> **参考：**
> - [认知翻译层](../cognitive-memory.md) — 身份层在三层知识资产中的位置
> - [SOUL 系统代码](../../kernel/soul/)
> - [SoulRuntime 代码](../../kernel/gateway/src/runtime/soul_runtime.rs)
