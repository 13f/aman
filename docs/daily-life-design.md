# Daily Life System — Architecture Design (v3: Lifecycle Engine)

> **核心变更**：与 Work/Study System 对齐——从「独立 2 状态机」重构为「LifecycleEngine 的领域适配层」。
> 共用逻辑（状态机、FIFO 队列、步骤链式执行、中断/重试、IdleSignal 反馈、全局总线通知）
> 全部由 `crates/lifecycle::LifecycleEngine<S>` 提供。
> Daily Life System 只需实现 `SystemSpec` trait，提供日常领域特有的类型和逻辑。
>
> 架构层次：
> ```
> LifecycleEngine<DailySpec>  ← 通用引擎（lifecycle crate）
>   └─ DailySpec              ← 领域适配（daily-life crate，实现 SystemSpec trait）
>        ├─ Item  = DailyItem
>        ├─ Step  = Routine
>        ├─ decompose()       → 根据 TimeWindow 确定例行事项列表
>        ├─ execute_step_impl() → 执行单个例行（查日历、天气、习惯、反思等）
>        └─ collect_result()  → 收集日志 + 健康快照 + 习惯完成记录
> ```

---

## 1. Why This Refactoring

v2 中三个系统各自独立实现相同的队列/状态/步骤逻辑。v3 将通用部分提取到 `lifecycle` crate。

关键区别：Daily Life 的"定时触发"由 **外部 Cron/Timer Source** 管理，不在 Daily System 内部。
Cron Source 在配置的时间点构造 `DailyItem` + `DailyItemAssigned` 事件推送入队，
与用户查询、健康同步、日历更新的处理路径完全一致。

---

## 2. Lifecycle Engine Architecture

### 2.1 DailyLifeSystem — Thin Wrapper

```rust
pub struct DailyLifeSystem {
    engine: LifecycleEngine<DailySpec>,
    config: DailyLifeConfig,
    local_bus: Arc<dyn EventBus>,
    global_bus: Arc<dyn EventBus>,
    persistence: DailyLifeStore,             // Daily 特有
    calendar: Arc<dyn CalendarClient>,       // Daily 特有
    health_data: Arc<dyn HealthDataClient>,  // Daily 特有
    idle_signal_tx: Mutex<Option<mpsc::UnboundedSender<IdleSignal>>>,
}

impl DailyLifeSystem {
    pub fn new(agent_id, config, local_bus, global_bus, persistence, calendar, health, system_state) -> Self {
        let spec = DailySpec::new();
        let engine = LifecycleEngine::new(
            agent_id, spec,
            config.queue.max_size,
            0,  // routines don't have step-level retries
            local_bus, global_bus,
            system_state,
            AgentSystemState::DailyLife,  // BUSY 时设置的系统状态
        );
        // ...
    }

    pub async fn handle(&self, event: DailyEvent) -> DailyResult<()> {
        match event {
            DailyEvent::Interrupt { reason, by_system } => {
                self.engine.handle_interrupt(&reason, &by_system).await?;
            }
            DailyEvent::DailyItemAssigned { item, source } => {
                self.engine.handle_assigned(item, source_json).await?;
            }
            DailyEvent::DailyItemCompleted { item_id, outcome, duration } => {
                // 持久化今日快照（如 Night 窗口）
                self.engine.handle_completed(&item_id, result_json, duration).await?;
            }
            DailyEvent::DailyItemFailed { item_id, error, retryable } => {
                self.engine.handle_failed(&item_id, lc_error, retryable).await?;
            }
        }
    }
}
```

---

## 3. State Machine (provided by LifecycleEngine)

与 Work/Study System 完全相同的 2 状态机（参见 work-design.md §3）。

引擎在状态切换时自动更新 `AgentSystemState`：
- `Idle` → `AgentSystemState::Idle`
- `Busy` → `AgentSystemState::DailyLife`

---

## 4. Domain Types (daily-life-specific)

### 4.1 DailyEvent

```rust
pub enum DailyEvent {
    DailyItemAssigned { item: DailyItem, source: DailyItemSource },
    DailyItemCompleted { item_id: DailyItemId, outcome: DailyItemOutcome, duration: Duration },
    DailyItemFailed { item_id: DailyItemId, error: DailyError, retryable: bool },
    Interrupt { reason: String, by_system: String },
}
```

### 4.2 DailyItemSource

```rust
pub enum DailyItemSource {
    TimeTrigger { window: TimeWindow, trigger: String },
    UserAction { operator: String, action: String },
    HealthDataSync { source: HealthDataSource },
    CalendarUpdated,
    SeekResponse { request_id: String },
    Custom { name: String, metadata: HashMap<String, Value> },
}
```

### 4.3 DailyItem & TimeWindow

```rust
pub struct DailyItem {
    pub id: DailyItemId,
    pub window: TimeWindow,
    pub routines: Option<Vec<Routine>>,   // 预设例行（为空时由 Spec 根据 window 确定）
    pub priority: Priority,
    pub context: HashMap<String, Value>,
    pub created_at: Timestamp,
}

pub enum TimeWindow {
    Morning,     // 06:00–11:59
    Midday,      // 12:00–13:59
    Afternoon,   // 14:00–17:59
    Evening,     // 18:00–20:59
    Night,       // 21:00–05:59
}
```

### 4.4 Routine

```rust
pub struct Routine {
    pub name: String,
    pub action: RoutineAction,
    pub priority: RoutinePriority,
}

pub enum RoutineAction {
    CheckCalendar { days_ahead: u32 },
    CheckWeather,
    CheckHabits,
    CheckHealth,
    GuideReflection { template: String },
    DailyBrief,
    CustomPrompt { prompt: String },
}
```

---

## 5. DailySpec — Domain Adapter

`DailySpec` 是 Daily Life 领域对 `SystemSpec` trait 的实现。

### 5.1 Step Type: Routine

```rust
// DailySpec::Step = Routine
// 每个例行事项是一个步骤，引擎依次执行
```

### 5.2 Decomposition by TimeWindow

```rust
impl SystemSpec for DailySpec {
    type Item = DailyItem;
    type Step = Routine;

    async fn decompose(&self, item: &DailyItem, _max_retries: u32) -> Vec<Routine> {
        // 有预设例行 → 直接使用
        if let Some(ref predefined) = item.routines {
            if !predefined.is_empty() {
                return predefined.clone();
            }
        }

        // 根据时间窗 + 配置确定例行列表
        self.config.routines.for_window(item.window)
    }
}
```

### 5.3 Step Execution by RoutineAction

```rust
async fn execute_step_impl(
    &self,
    item: &DailyItem,
    routine: &Routine,
    _step_index: usize,
) -> Result<StepOutput, LifecycleError> {
    match &routine.action {
        RoutineAction::CheckCalendar { days_ahead } => {
            let events = self.calendar.get_events(from, to).await?;
            Ok(StepOutput {
                success: true,
                summary: self.format_calendar_brief(&events),
                artifacts: vec![],
                duration: elapsed,
            })
        }
        RoutineAction::CheckWeather => {
            let forecast = self.fetch_and_format_weather().await?;
            Ok(StepOutput { summary: forecast, .. })
        }
        RoutineAction::CheckHabits => {
            let (completed, reminders) = self.check_habits(item.window).await?;
            for reminder in reminders {
                self.deliver_reminder(&reminder).await?;
            }
            Ok(StepOutput { .. })
        }
        RoutineAction::CheckHealth => {
            let snapshot = self.check_health_and_alert().await?;
            Ok(StepOutput { .. })
        }
        RoutineAction::GuideReflection { template } => {
            let (reflection, insights) = self.run_reflection(template).await?;
            self.persistence.save_reflection(&reflection).await?;
            Ok(StepOutput { summary: insights.join("; "), .. })
        }
        RoutineAction::DailyBrief => {
            let brief = self.generate_daily_brief().await?;
            Ok(StepOutput { summary: brief, .. })
        }
        RoutineAction::CustomPrompt { prompt } => {
            let response = self.execute_custom_prompt(prompt).await?;
            Ok(StepOutput { summary: response, .. })
        }
    }
}
```

### 5.4 Result Collection

```rust
fn collect_result(item: &DailyItem, outputs: &[StepOutput]) -> serde_json::Value {
    let routines_completed = outputs.iter().filter(|o| o.success).count();
    serde_json::json!({
        "window": item.window,
        "routines_completed": routines_completed,
        "total_routines": outputs.len(),
        "summaries": outputs.iter().map(|o| &o.summary).collect::<Vec<_>>(),
    })
}
```

---

## 6. Cron/Timer Source — External Time Management

Daily System 不管理时钟。时间触发由全局 Cron Source 统一管理：

```yaml
sources:
  cron:
    - schedule: "0 6 * * *"
      action: push_daily_item
      params:
        window: morning
        trigger: morning_tick

    - schedule: "0 12 * * *"
      action: push_daily_item
      params:
        window: midday
        trigger: midday_tick

    - schedule: "0 18 * * *"
      action: push_daily_item
      params:
        window: evening
        trigger: evening_tick

    - schedule: "0 21 * * *"
      action: push_daily_item
      params:
        window: night
        trigger: night_tick
```

### Daily Timeline

```
06:00 ─ Cron Source pushes DailyItemAssigned(window=Morning)
  → BUSY
    → CheckCalendar → CheckWeather → CheckHabits → DailyBrief → MorningReflection
  → DailyItemCompleted → IDLE

12:00 ─ Cron Source pushes DailyItemAssigned(window=Midday)
  → BUSY → CheckHabits → CheckHealth → DailyItemCompleted → IDLE

18:00 ─ Cron Source pushes DailyItemAssigned(window=Evening)
  → BUSY → CheckHabits → CheckHealth → DailyItemCompleted → IDLE

21:00 ─ Cron Source pushes DailyItemAssigned(window=Night)
  → BUSY
    → CheckHabits → GuideReflection(EveningReview) → Logging → generate tomorrow plan
  → DailyItemCompleted → IDLE (until next morning)
```

---

## 7. Habit Reminder Escalation

柔性提醒保留——它是 `CheckHabits` routine（即 `execute_step_impl` 内部）的行为：

```
Habit: 晨间冥想 (TimeWindow: Morning, target: Daily)

Day 1: 09:00 未完成 → HabitReminder(urgency=Gentle)
Day 2: 09:00 未完成 → HabitReminder(urgency=Friendly)
Day 3: 09:00 未完成 → HabitReminder(urgency=Firm)
Day 7: 09:00 未完成 → HabitReminder(urgency=Concerned)
        "要不要把目标降到每天 1 分钟？习惯比强度重要。"
```

`CheckHabits` 在 `execute_step_impl` 中对比当前时间和期望完成时间，未完成则通过提醒通道发送通知。

---

## 8. Health & Calendar Integration

Health 和 Calendar 是独立 Plugin/Source，通过推送 `DailyItemAssigned` 与 Daily System 交互：

```
┌──────────────┐     HealthDataSync      ┌─────────────────┐
│ Health Plugin│ ───────────────────────→│ Daily System     │
│ (Apple Health│    DailyItemAssigned    │ (被动消费)       │
│  Fitbit, etc)│                         │                  │
└──────────────┘                         └─────────────────┘

┌──────────────┐     CalendarUpdated     ┌─────────────────┐
│ Calendar     │ ───────────────────────→│ Daily System     │
│ Plugin       │    DailyItemAssigned    │                  │
└──────────────┘                         └─────────────────┘
```

同时 `execute_step_impl` 中也会主动查询 Calendar/Health API（如 `CheckCalendar`、`CheckHealth` routine）。

---

## 9. Configuration

```yaml
daily_life:
  timezone: "Asia/Shanghai"

  time_windows:
    morning_start: "06:00"
    midday_start: "12:00"
    evening_start: "18:00"
    night_start: "21:00"

  routines:
    morning:
      - name: "今日日程"
        action: check_calendar
        params: { days_ahead: 1 }
        priority: essential
      - name: "天气播报"
        action: check_weather
        priority: standard
      - name: "习惯检查"
        action: check_habits
        priority: essential
      - name: "晨间简报"
        action: daily_brief
        priority: essential

    midday:
      - name: "午间习惯提醒"
        action: check_habits
        priority: standard
      - name: "活动进度"
        action: check_health
        priority: optional

    evening:
      - name: "全天习惯回顾"
        action: check_habits
        priority: standard

    night:
      - name: "晚间习惯确认"
        action: check_habits
        priority: essential
      - name: "晚间回顾引导"
        action: guide_reflection
        params: { template: evening_review }
        priority: essential

  habits:
    - id: "morning-meditation"
      name: "晨间冥想"
      habit_type: duration
      target: { daily: 10 }
      trigger_window: morning
      reminder:
        enabled: true
        preferred_time: "07:00"
        re_remind_interval: 1800s
        max_reminders: 2
        escalation_days: 3

  queue:
    max_size: 50
    priority_queue: false

  health:
    metrics: [steps, active_energy, sleep_duration, weight, mood]
    anomaly_thresholds:
      sleep_duration:
        low: 6.0
        high: 10.0

  retention:
    health_metrics: 365d
    life_logs: 90d
    daily_reflections: forever
```

---

## 10. Event Routing

```yaml
routes:
  - match: { event_type: "daily.item.assigned" }   → handler:daily_life
  - match: { event_type: "daily.item.completed" }  → handler:daily_life
  - match: { event_type: "daily.item.failed" }     → handler:daily_life
  - match: { event_type: "daily.interrupt" }       → handler:daily_life
```

内部 step 事件由引擎管理，不走路由表。

---

## 11. Summary

| 维度 | v2 (独立实现) | v3 (Lifecycle Engine) |
|------|-------------|----------------------|
| **状态机** | 手写 `DailyState` | `LifecycleState` (引擎提供) |
| **队列/上下文** | 手写 `DailyContext` | `LifecycleContext<DailyItem, Routine>` |
| **步骤链** | 手写 Routine 遍历 | 引擎自动推进 |
| **定时管理** | Cron Source 外部推送 | 不变（Cron 与 Daily 解耦） |
| **Health/Calendar** | 独立 Plugin 推送 | 不变 |
| **习惯提醒** | `CheckHabits` routine 内部 | 不变（在 `execute_step_impl` 中） |
| **领域代码量** | ~550 行 | ~100 行 (spec + wrapper) |

**核心原则**：
1. Daily Life System 是 `LifecycleEngine<DailySpec>` 的薄封装。
2. **时间的感知不在 Daily System 内部**——Cron/Timer Source 在配置的时间点推送 `DailyItem`。
3. 每个 Routine 是一个 Step，由引擎依次执行。
4. 习惯提醒、健康检测、反思引导都是 `execute_step_impl` 内的领域逻辑。
5. Health/Calendar 数据通过独立 Plugin 推送 OR `execute_step_impl` 内主动查询，双路径。
