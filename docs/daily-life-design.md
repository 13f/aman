# Daily Life System — Architecture Design (v2: Passive Push Queue)

> **核心变更**：与 Work/Study System v2 对齐——从「内部定时巡检 + 五状态机」退化为
> 「被动队列消费者」。时间触发器由外部 Cron/Timer Source 统一管理，到时间后推送
> `DailyItemAssigned` 到 Agent 的 Daily Life 队列。用户查询、健康数据同步、日历更新
> 同样通过推送入队。Daily Life System 只负责按流程执行，不做巡检、不管理自己的定时器。
>
> 为什么叫 **DailyItem**？推送到 Daily Life 队列的可以是时间触发的晨间例行、
> 用户主动查询（"今天走了多少步"）、健康数据同步异常提醒、日历变更通知等。
> 是通用的"日常生活工作单元"。

---

## 1. Why This Simplification

旧设计（v1）的问题：

| 问题 | v1 做法 | 实际需求 |
|------|--------|---------|
| 自管定时器 | Daily System 内部管理 MorningTick/EveningTick/DelayedDailyTick | 时间触发是通用能力，应由 Cron/Timer Source 统一管理 |
| 状态爆炸 | 5 状态 + 20+ 事件类型 | 例行执行流程是内部步骤序列，不需要每个阶段暴露为状态 |
| 巡检回退 | DelayedDailyTick 循环调度 | Cron Source 在配置的时间点直接推送，不需要"检查有没有事做" |
| 多入口事件 | 7 种不同的触发事件（MorningTick, LifeQuery, HealthDataSync...) | 统一为 `DailyItemAssigned` + `DailyItemSource` |
| Tick 管理复杂 | 需要在每个状态转换后计算 next_tick_delay 并投递 DelayedDailyTick | Cron Source 独立管理调度，与 Daily System 解耦 |

核心理念转变：

```
旧：Daily System 内部管理时钟、巡检、执行、记录、反思
    → Daily System 承担了「时间调度器 + 执行器」双重职责

新：Cron/Timer Source 在配置的时间点推送 DailyItem，
    用户/健康/日历等外部系统同样推送，
    Daily System 只负责执行
    → Daily System 就是一个带 Hook 的 FIFO 日常队列消费者
```

---

## 2. Simplified State Machine

### 2.1 Two States

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DailyState {
    /// 队列为空，无日常活动。Event Bus 空闲时 Idle System 自然运行。
    Idle,
    /// 正在执行当前 DailyItem 的某个阶段。Bus 保持非空，Idle 不触发。
    Busy,
}
```

### 2.2 State Transitions

```
                  DailyItemAssigned
     ┌───────┐  ────────────────────  ┌───────┐
     │ IDLE  │                         │ BUSY  │
     └───┬───┘                         └───┬───┘
         │                                  │
         │  Interrupt                       │  当前 Item 完成 + 队列有下一个
         │  (any state → IDLE)              │  → 继续 BUSY
         │                                  │
         │                                  │  当前 Item 完成 + 队列为空
         │  ◄───────────────────────────────┘  → IDLE
         │
         │  DailyItemAssigned (while IDLE)
         │  → IDLE → BUSY
         │
         └─────────────────────────────────

    Interrupt: 任何状态收到 → 保存 checkpoint → 无条件切回 IDLE。
```

### 2.3 Comparison with v1

| 维度 | v1 (内部定时巡检) | v2 (被动推送) |
|------|-----------------|-------------|
| 状态数 | 5 (IDLE/CHECKING_ROUTINE/EXECUTING/LOGGING/REFLECTING) | 2 (IDLE/BUSY) |
| 事件类型 | 20+ | 3 + Interrupt |
| 定时管理 | 内部 DelayedDailyTick 自循环 | Cron/Timer Source 外部管理 |
| 例行执行 | CHECKING → EXECUTING，分状态处理 | 内部 Phase Pipeline |
| 多入口 | 7 种不同触发事件 | DailyItemAssigned + DailyItemSource |
| 中断 | 4 个活跃状态均可 Interrupt | 1 个活跃状态 (BUSY) |

---

## 3. Type System

### 3.1 DailyEvent

```rust
/// Daily Life System 的领域事件——只有 3 个业务事件 + 1 个系统事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DailyEvent {
    /// 外部系统推送日常项到 Agent。
    DailyItemAssigned {
        item: DailyItem,
        source: DailyItemSource,
    },

    /// 当前日常项完成。
    DailyItemCompleted {
        item_id: DailyItemId,
        outcome: DailyItemOutcome,
        duration: Duration,
    },

    /// 当前日常项失败。
    DailyItemFailed {
        item_id: DailyItemId,
        error: DailyError,
        retryable: bool,
    },

    /// 中断当前执行，强制切回 IDLE。
    Interrupt {
        reason: String,
        by_system: String,
    },
}
```

### 3.2 DailyItemSource

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DailyItemSource {
    /// Cron/Timer Source 在配置的时间点触发。
    TimeTrigger {
        window: TimeWindow,
        /// 具体触发原因（morning_tick, evening_tick 等）。
        trigger: String,
    },
    /// 用户通过 CLI/API 主动查询或记录。
    UserAction {
        operator: String,
        action: String,
    },
    /// 健康数据同步到达。
    HealthDataSync {
        source: HealthDataSource,
    },
    /// 日历事件变更。
    CalendarUpdated,
    /// Idle Boredom → SeekDaily 响应。
    SeekResponse {
        request_id: String,
    },
    /// 其他自定义来源。
    Custom {
        name: String,
        metadata: HashMap<String, Value>,
    },
}
```

### 3.3 DailyItem

```rust
/// 推送到 Daily Life 队列的日常单元。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyItem {
    pub id: DailyItemId,
    /// 当前时间窗（决定执行哪些例行事项）。
    pub window: TimeWindow,

    /// 要执行的例行事项列表（可选）。
    /// 如果为空，Daily System 根据 window + 配置自动确定。
    pub routines: Option<Vec<Routine>>,

    /// 优先级。
    pub priority: Priority,

    /// 附带的上下文。
    pub context: HashMap<String, Value>,

    /// 创建时间。
    pub created_at: Timestamp,
}

/// 一天中的时间窗。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeWindow {
    Morning,    // 06:00–11:59
    Midday,     // 12:00–13:59
    Afternoon,  // 14:00–17:59
    Evening,    // 18:00–20:59
    Night,      // 21:00–05:59
}

/// 一个例行事项。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Routine {
    pub name: String,
    pub action: RoutineAction,
    pub priority: RoutinePriority,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoutineAction {
    CheckCalendar { days_ahead: u32 },
    CheckWeather,
    CheckHabits,
    CheckHealth,
    GuideReflection { template: String },
    DailyBrief,
    CustomPrompt { prompt: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum RoutinePriority {
    Essential,
    Standard,
    Optional,
}
```

### 3.4 DailyContext

```rust
#[derive(Debug, Clone)]
pub struct DailyContext {
    pub state: DailyState,
    /// FIFO 日常队列。
    pub queue: VecDeque<DailyItem>,
    /// 当前正在执行的日常项。
    pub current: Option<DailyItem>,
    /// 当前例行事项的执行进度。
    pub completed_routines: Vec<String>,
    /// 当前项产出的日志条目。
    pub pending_logs: Vec<LifeLogEntry>,
    /// 今日已完成的习惯。
    pub completed_habits: Vec<HabitCompletion>,
    /// 今日健康快照。
    pub health_snapshot: Option<HealthSnapshot>,
    /// 今日日期。
    pub today: NaiveDate,
}

impl DailyContext {
    pub fn new() -> Self {
        Self {
            state: DailyState::Idle,
            queue: VecDeque::new(),
            current: None,
            completed_routines: Vec::new(),
            pending_logs: Vec::new(),
            completed_habits: Vec::new(),
            health_snapshot: None,
            today: Local::now().date_naive(),
        }
    }

    pub fn enqueue(&mut self, item: DailyItem) {
        self.queue.push_back(item);
    }

    pub fn dequeue(&mut self) -> Option<DailyItem> {
        self.queue.pop_front()
    }

    pub fn reset_to_idle(&mut self) {
        self.state = DailyState::Idle;
        self.current = None;
        self.completed_routines.clear();
        self.pending_logs.clear();
    }
}
```

### 3.5 Habit (unchanged from v1)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Habit {
    pub id: HabitId,
    pub name: String,
    pub description: String,
    pub habit_type: HabitType,
    pub target: HabitTarget,
    pub trigger_window: TimeWindow,
    pub reminder: HabitReminderStrategy,
    pub current_streak: u32,
    pub best_streak: u32,
    pub created_at: NaiveDate,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HabitType {
    Daily { target_count: u32 },
    Weekly { target_count: u32, completed_this_week: u32 },
    Duration { target_minutes: u32 },
    Binary,
    Count { target: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HabitTarget {
    pub daily: Option<u32>,
    pub weekly: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HabitReminderStrategy {
    pub enabled: bool,
    pub preferred_time: Option<NaiveTime>,
    pub re_remind_interval: Duration,
    pub max_reminders: u32,
    pub escalation_days: u32,
}
```

---

## 4. Core Execution Logic

### 4.1 Event Handler

```rust
impl DailyLifeSystem {
    pub async fn handle(
        &mut self,
        event: DailyEvent,
        ctx: &mut DailyContext,
        local_bus: &dyn EventBus,
        persistence: &mut DailyLifeStore,
        calendar: &dyn CalendarClient,
        health_data: &dyn HealthDataClient,
        trace: &mut TraceStore,
    ) -> DailyResult<()> {
        match event {
            // ── Interrupt（最高优先级）────────────────────────
            DailyEvent::Interrupt { reason, by_system } => {
                if ctx.state == DailyState::Busy {
                    let checkpoint = self.save_checkpoint(ctx);
                    trace.record(DailyTraceEvent::Interrupted { checkpoint, by_system });
                }
                ctx.reset_to_idle();
                return Ok(());
            }

            // ── 收到新日常项 ──────────────────────────────────
            DailyEvent::DailyItemAssigned { item, source } => {
                trace.record(DailyTraceEvent::ItemReceived {
                    item_id: item.id,
                    window: item.window,
                    source: source.clone(),
                });
                ctx.enqueue(item);

                if ctx.state == DailyState::Idle {
                    ctx.state = DailyState::Busy;
                    let next = ctx.dequeue().unwrap();
                    self.start_item(next, ctx, local_bus, persistence, calendar, health_data).await?;
                }
            }

            // ── 日常项完成 ────────────────────────────────────
            DailyEvent::DailyItemCompleted { item_id, outcome, duration } => {
                trace.record(DailyTraceEvent::ItemCompleted { item_id, outcome, duration });
                self.process_next(ctx, local_bus, persistence, calendar, health_data).await?;
            }

            // ── 日常项失败 ────────────────────────────────────
            DailyEvent::DailyItemFailed { item_id, error, retryable } => {
                trace.record(DailyTraceEvent::ItemFailed { item_id, error: error.to_string(), retryable });
                if retryable && self.should_retry(&error) {
                    if let Some(item) = ctx.current.take() {
                        ctx.queue.push_front(item);
                    }
                }
                self.process_next(ctx, local_bus, persistence, calendar, health_data).await?;
            }
        }
        Ok(())
    }

    async fn process_next(
        &mut self,
        ctx: &mut DailyContext,
        local_bus: &dyn EventBus,
        persistence: &mut DailyLifeStore,
        calendar: &dyn CalendarClient,
        health_data: &dyn HealthDataClient,
    ) -> DailyResult<()> {
        match ctx.dequeue() {
            Some(next) => {
                self.start_item(next, ctx, local_bus, persistence, calendar, health_data).await?;
            }
            None => {
                ctx.reset_to_idle();
            }
        }
        Ok(())
    }

    async fn start_item(
        &mut self,
        item: DailyItem,
        ctx: &mut DailyContext,
        local_bus: &dyn EventBus,
        persistence: &mut DailyLifeStore,
        calendar: &dyn CalendarClient,
        health_data: &dyn HealthDataClient,
    ) -> DailyResult<()> {
        self.run_hooks(HookPoint::BeforeExecution, &item).await?;

        ctx.current = Some(item);

        // 确定要执行的例行事项列表
        let routines = match &ctx.current.as_ref().unwrap().routines {
            Some(predefined) => predefined.clone(),
            None => self.config.routines.for_window(ctx.current.as_ref().unwrap().window),
        };

        // 投递首个内部执行步骤
        if routines.is_empty() {
            // 无例行事项 → 直接完成
            local_bus.post(DailyEvent::DailyItemCompleted {
                item_id: ctx.current.as_ref().unwrap().id.clone(),
                outcome: DailyItemOutcome::NoRoutines,
                duration: Duration::ZERO,
            }).await?;
        } else {
            local_bus.post(DailyStepEvent::ExecuteRoutine {
                routine_index: 0,
                total: routines.len(),
            }).await?;
        }

        Ok(())
    }
}
```

### 4.2 Internal Step Execution

例行执行、记录、反思都是内部步骤，通过 `DailyStepEvent` 链式流转：

```rust
#[derive(Debug, Clone)]
enum DailyStepEvent {
    /// 执行第 N 个例行事项。
    ExecuteRoutine { routine_index: usize, total: usize },
    /// 例行事项完成。
    RoutineComplete { routine_index: usize, outcome: RoutineOutcome },
    /// 需要记录（日志/健康数据）。
    StartLogging,
    /// 记录完成。
    LoggingComplete,
    /// 需要反思（晚间回顾等）。
    StartReflection { reflection_type: ReflectionType },
    /// 反思完成。
    ReflectionComplete { insights: Vec<String>, suggestions: Vec<String> },
}

impl DailyLifeSystem {
    pub async fn execute_step(
        &mut self,
        step: DailyStepEvent,
        ctx: &mut DailyContext,
        local_bus: &dyn EventBus,
        persistence: &mut DailyLifeStore,
        calendar: &dyn CalendarClient,
        health_data: &dyn HealthDataClient,
    ) -> DailyResult<()> {
        let item = ctx.current.as_ref().unwrap();
        let routines = item.routines.as_ref()
            .unwrap_or(&vec![]); // FIXME: pass through context instead

        match step {
            DailyStepEvent::ExecuteRoutine { routine_index, total } => {
                let routine = &routines[routine_index];
                self.run_hooks(HookPoint::BeforePhase, item).await?;

                let outcome = match &routine.action {
                    RoutineAction::CheckCalendar { days_ahead } => {
                        let from = ctx.today;
                        let to = from + Duration::days(*days_ahead as i64);
                        let events = calendar.get_events(from, to).await?;
                        self.format_calendar_brief(&events)
                    }
                    RoutineAction::CheckWeather => {
                        self.fetch_and_format_weather().await
                    }
                    RoutineAction::CheckHabits => {
                        let (completed, reminders) = self.check_habits(ctx, item.window).await?;
                        for reminder in reminders {
                            self.deliver_reminder(&reminder).await?;
                        }
                        Ok(RoutineOutcome::Completed)
                    }
                    RoutineAction::CheckHealth => {
                        self.check_health_and_alert(ctx).await
                    }
                    RoutineAction::GuideReflection { template } => {
                        // 切换到反思阶段
                        local_bus.post(DailyStepEvent::StartReflection {
                            reflection_type: self.reflection_type_for_window(item.window),
                        }).await?;
                        return Ok(());
                    }
                    RoutineAction::DailyBrief => {
                        self.generate_daily_brief(ctx).await
                    }
                    RoutineAction::CustomPrompt { prompt } => {
                        self.execute_custom_prompt(prompt).await
                    }
                };

                ctx.completed_routines.push(routine.name.clone());

                self.run_hooks(HookPoint::AfterPhase, item).await?;

                // 决定下一步
                if routine_index + 1 < total {
                    local_bus.post(DailyStepEvent::ExecuteRoutine {
                        routine_index: routine_index + 1,
                        total,
                    }).await?;
                } else {
                    // 所有例行事项完成 → 检查是否需要记录
                    if self.needs_logging(ctx) {
                        local_bus.post(DailyStepEvent::StartLogging).await?;
                    } else if self.should_reflect(item.window) {
                        local_bus.post(DailyStepEvent::StartReflection {
                            reflection_type: self.reflection_type_for_window(item.window),
                        }).await?;
                    } else {
                        self.finish_item(ctx, local_bus, persistence).await?;
                    }
                }
            }

            DailyStepEvent::StartLogging => {
                // 记录心情、感恩、日记等
                let logs = self.prompt_and_collect_logs(ctx).await?;
                for log in &logs {
                    persistence.save_log_entry(log).await?;
                }
                ctx.pending_logs.extend(logs);
                local_bus.post(DailyStepEvent::LoggingComplete).await?;
            }

            DailyStepEvent::LoggingComplete => {
                if self.should_reflect(item.window) {
                    local_bus.post(DailyStepEvent::StartReflection {
                        reflection_type: self.reflection_type_for_window(item.window),
                    }).await?;
                } else {
                    self.finish_item(ctx, local_bus, persistence).await?;
                }
            }

            DailyStepEvent::StartReflection { reflection_type } => {
                let (reflection, insights, suggestions) = match reflection_type {
                    ReflectionType::MorningBrief => self.run_morning_brief(ctx).await?,
                    ReflectionType::EveningReview => self.run_evening_review(ctx).await?,
                    ReflectionType::WeeklyRetro => self.run_weekly_retro(ctx).await?,
                    ReflectionType::GratitudeCheckin => self.run_gratitude_checkin().await?,
                };
                persistence.save_reflection(&reflection).await?;

                // 夜间反思后生成明日计划
                if reflection_type == ReflectionType::EveningReview {
                    self.generate_tomorrow_plan(ctx).await?;
                }

                local_bus.post(DailyStepEvent::ReflectionComplete { insights, suggestions }).await?;
            }

            DailyStepEvent::ReflectionComplete { .. } => {
                self.finish_item(ctx, local_bus, persistence).await?;
            }

            _ => {}
        }
        Ok(())
    }

    async fn finish_item(
        &mut self,
        ctx: &mut DailyContext,
        local_bus: &dyn EventBus,
        persistence: &mut DailyLifeStore,
    ) -> DailyResult<()> {
        let item = ctx.current.as_ref().unwrap();

        // 持久化今日快照
        if item.window == TimeWindow::Night {
            persistence.save_daily_snapshot(ctx).await?;
        }

        self.run_hooks(HookPoint::AfterExecution, item).await?;
        self.run_hooks(HookPoint::OnSuccess, item).await?;

        let duration = item.created_at.elapsed();
        local_bus.post(DailyEvent::DailyItemCompleted {
            item_id: item.id.clone(),
            outcome: DailyItemOutcome::Completed,
            duration,
        }).await?;

        Ok(())
    }
}
```

### 4.3 Cron/Timer Source — External Time Management

v1 中 Daily System 自管理的定时器全部移除，改为外部 Cron Source 推送：

```yaml
# Cron Source 配置（在全局 source 层，非 daily 内部）
sources:
  cron:
    - schedule: "0 6 * * *"        # 每天 06:00
      action: push_daily_item
      params:
        window: morning
        trigger: morning_tick

    - schedule: "0 12 * * *"       # 每天 12:00
      action: push_daily_item
      params:
        window: midday
        trigger: midday_tick

    - schedule: "0 18 * * *"       # 每天 18:00
      action: push_daily_item
      params:
        window: evening
        trigger: evening_tick

    - schedule: "0 21 * * *"       # 每天 21:00
      action: push_daily_item
      params:
        window: night
        trigger: night_tick

    - schedule: "0 * * * *"        # 每小时
      action: push_daily_item
      params:
        window: auto               # 根据当前时间自动判定
        trigger: hourly_tick
```

`push_daily_item` 动作由 Cron Source 执行：构造 `DailyItem` + `DailyItemAssigned` 事件 → 投递到目标 Agent 的 Local Event Bus。

### 4.4 Daily Timeline (unchanged flow, new mechanism)

```
06:00 ─ Cron Source pushes DailyItemAssigned(window=Morning)
  → BUSY
    → CheckCalendar → CheckWeather → CheckHabits → DailyBrief
    → MorningReflection
  → DailyItemCompleted
  → IDLE

12:00 ─ Cron Source pushes DailyItemAssigned(window=Midday)
  → BUSY
    → CheckHabits → CheckHealth
  → DailyItemCompleted
  → IDLE

18:00 ─ Cron Source pushes DailyItemAssigned(window=Evening)
  → BUSY
    → CheckHabits → CheckHealth
  → DailyItemCompleted
  → IDLE

21:00 ─ Cron Source pushes DailyItemAssigned(window=Night)
  → BUSY
    → CheckHabits → GuideReflection
    → Logging (mood, gratitude, journal)
    → EveningReflection → generate tomorrow plan
  → DailyItemCompleted
  → IDLE (until next morning)
```

---

## 5. How External Systems Push Daily Items

### 5.1 Unified Push Interface

```rust
#[async_trait]
pub trait DailyItemPushChannel {
    async fn push(
        &self,
        agent_id: &AgentId,
        item: DailyItem,
        source: DailyItemSource,
    ) -> Result<()>;
}
```

### 5.2 Cron/Timer → Daily

```
Cron Source (全局):
  - "0 6 * * *" 到期 → push(agent_id, DailyItem { window: Morning }, TimeTrigger)
  - "0 21 * * *" 到期 → push(agent_id, DailyItem { window: Night }, TimeTrigger)
```

### 5.3 CLI/API → Daily

```
用户: aman daily query "how was my sleep this week"
  → DailyItemAssigned { item: DailyItem { routines: [LifeQuery(SleepAnalysis)] },
                         source: UserAction }
  → BUSY → 执行查询 → 返回结果 → DailyItemCompleted

用户: aman daily log "mood: feeling great today"
  → DailyItemAssigned { item: DailyItem { routines: [LogLifeEvent(Mood)] },
                         source: UserAction }
  → BUSY → 记录 → DailyItemCompleted
```

### 5.4 Health Sync → Daily

```
Health Plugin 收到 Apple Health 数据同步:
  → push(agent_id, DailyItem { routines: [CheckHealth] },
         HealthDataSync { source: AppleHealth })
  → BUSY → 合并指标 → 检测异常 → 异常提醒（如有）
  → DailyItemCompleted
```

### 5.5 Calendar → Daily

```
Calendar Plugin 检测到事件变更:
  → push(agent_id, DailyItem { routines: [CheckCalendar { days_ahead: 1 }] },
         CalendarUpdated)
  → BUSY → 获取最新日程 → 格式化简报
  → DailyItemCompleted
```

---

## 6. Habit Reminder Escalation (unchanged from v1)

柔性提醒升级保留——它是 `CheckHabits` routine 的内部行为：

```
Habit: 晨间冥想 (TimeWindow: Morning, target: Daily)

Day 1: 09:00 未完成 → HabitReminder(urgency=Gentle)
Day 2: 09:00 未完成 → HabitReminder(urgency=Friendly)
Day 3: 09:00 未完成 → HabitReminder(urgency=Firm)
Day 7: 09:00 未完成 → HabitReminder(urgency=Concerned)
        "要不要把目标降到每天 1 分钟？习惯比强度重要。"
```

`ReminderUrgency` 枚举保持不变（Gentle/Friendly/Firm/Concerned），
只是提醒的触发从 v1 的 "CHECKING_ROUTINE 状态中检查" 变为
"CheckHabits routine 内部逻辑"。

---

## 7. Hook Mechanism

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookPoint {
    BeforeExecution,
    BeforePhase,       // 每个例行事项执行前
    AfterPhase,        // 每个例行事项执行后
    AfterExecution,
    OnSuccess,
    OnFailure,
}
```

```yaml
daily_life:
  hooks:
    before_execution:
      - name: log_routine_start
        action:
          type: tool
          tool_name: trace.record
          params:
            event: "daily.item.started"

    after_phase:
      - name: update_daily_snapshot
        action:
          type: tool
          tool_name: persistence.save_snapshot

    on_success:
      - name: sync_calendar_back
        action:
          type: emit_event
          event_type: "calendar.updated"
```

---

## 8. Health & Calendar Integration

Health 和 Calendar 不再是 Daily System 的内部组件，而是独立的 Plugin/Source，
通过推送 `DailyItemAssigned` 与 Daily System 交互：

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

Health 和 Calendar 的**数据模型**保持不变（HealthMetric, HealthSnapshot, CalendarEvent 等），
因为它们属于领域模型，不是状态机。详见 v1 文档 §6–§7。

---

## 9. Configuration

```yaml
daily_life:
  timezone: "Asia/Shanghai"

  # 时间窗定义
  time_windows:
    morning_start: "06:00"
    midday_start: "12:00"
    evening_start: "18:00"
    night_start: "21:00"

  # 每个时间窗的例行事项
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
      - name: "今日运动总结"
        action: check_health
        priority: optional

    night:
      - name: "晚间习惯确认"
        action: check_habits
        priority: essential
      - name: "晚间回顾引导"
        action: guide_reflection
        params: { template: evening_review }
        priority: essential

  # 反思模板
  reflection:
    morning_brief: true
    evening_review: true
    weekly_retro: true
    weekly_retro_day: sunday
    gratitude_checkin: true

  # 习惯定义
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

    - id: "daily-walk"
      name: "每日步行"
      habit_type: count
      target: { daily: 10000 }
      trigger_window: evening
      reminder:
        enabled: true
        preferred_time: "18:00"
        re_remind_interval: 3600s
        max_reminders: 1
        escalation_days: 3

    - id: "evening-journal"
      name: "晚间日记"
      habit_type: binary
      target: { daily: 1 }
      trigger_window: night
      reminder:
        enabled: true
        preferred_time: "21:30"
        re_remind_interval: 1800s
        max_reminders: 1
        escalation_days: 3

    - id: "water-intake"
      name: "饮水目标"
      habit_type: daily
      target: { daily: 8 }
      trigger_window: midday
      reminder:
        enabled: true
        preferred_time: "12:00"
        re_remind_interval: 7200s
        max_reminders: 2
        escalation_days: 2

  hooks:
    before_execution: []
    before_phase: []
    after_phase: []
    after_execution: []
    on_success: []
    on_failure: []

  # 队列
  queue:
    max_size: 50
    priority_queue: false

  # 提醒风格
  reminder_style: gentle

  # 健康追踪（数据模型保留，由 Health Plugin 管理数据源）
  health:
    metrics: [steps, active_energy, sleep_duration, weight, mood]
    anomaly_thresholds:
      sleep_duration:
        low: 6.0
        high: 10.0
        trend_window_days: 7
        trend_threshold: 0.15

  # 数据保留
  retention:
    health_metrics: 365d
    life_logs: 90d
    daily_reflections: forever
    habit_completions: 365d
```

对比 v1：不再有 `DelayedDailyTick` 相关配置、不再有 `DailyPersonality` 中的巡检参数、
Cron schedule 移到全局 `sources.cron` 中管理。

---

## 10. Runtime Integration

```rust
impl AgentBuilder {
    pub fn build(self) -> Result<AgentRuntime> {
        let daily_sys = DailyLifeSystem::new(
            self.config.daily_life.clone(),
            local_bus.clone(),
            self.persistence.daily_life_store(),
        );
        // Calendar 和 Health 数据通过 Tool 层或 Plugin 获取，不注入到 DailySystem
        // ...
    }
}
```

关闭时：
- persist today's snapshot（日期切换时的最终快照）
- flush habit completion records
- flush pending log entries

---

## 11. Event Routing

```yaml
routes:
  - match: { event_type: "daily.item.assigned" }   → handler:daily_life
  - match: { event_type: "daily.item.completed" }  → handler:daily_life
  - match: { event_type: "daily.item.failed" }     → handler:daily_life
  - match: { event_type: "daily.interrupt" }       → handler:daily_life
```

`DailyStepEvent`（ExecuteRoutine/RoutineComplete/StartLogging/StartReflection 等）是内部事件，不走路由表。

---

## 12. Migration Path from v1

| 删除 | 替换为 |
|------|-------|
| `DailyState::CheckingRoutine` | 内部 Phase Pipeline（routine 列表遍历） |
| `DailyState::Executing` | 内部 Phase（ExecuteRoutine） |
| `DailyState::Logging` | 内部 Phase（StartLogging） |
| `DailyState::Reflecting` | 内部 Phase（StartReflection） |
| `MorningTick` / `MiddayTick` / `EveningTick` / `NightTick` / `HourlyTick` | Cron Source → `DailyItemAssigned(TimeTrigger)` |
| `DelayedDailyTick` | 删除（Cron Source 替代） |
| `StartRoutineCheck` / `RoutineCheckComplete` | 内部 `DailyStepEvent::ExecuteRoutine` |
| `ExecuteRoutine(Routine)` / `RoutineComplete` | 内部 `DailyStepEvent` |
| `StartLogging` / `LoggingComplete` | 内部 `DailyStepEvent` |
| `StartReflection` / `ReflectionComplete` | 内部 `DailyStepEvent` |
| `LifeQuery` / `LifeLog` | `DailyItemAssigned(UserAction)` |
| `HealthDataSync` | `DailyItemAssigned(HealthDataSync)` |
| `CalendarUpdated` | `DailyItemAssigned(CalendarUpdated)` |
| `DailyPersonality` (巡检参数) | Cron Source 配置 |

保留：
- `Interrupt` + checkpoint 机制
- `Habit` / `HabitType` / `HabitReminderStrategy` 数据模型
- 柔性提醒升级逻辑（挪到 CheckHabits routine 内部）
- `TimeWindow` / `RoutineAction` / `RoutinePriority`
- `ReflectionType`（MorningBrief/EveningReview/WeeklyRetro/GratitudeCheckin）
- `HealthMetric` / `HealthSnapshot` 数据模型
- `DailyLifeStore` 持久化结构
- Phase 4 初始化 / Phase 0 销毁

---

## 13. Summary

| 维度 | v1 (内部定时巡检) | v2 (被动推送) |
|------|-----------------|-------------|
| **状态** | 5 | 2 (IDLE/BUSY) |
| **事件类型** | 20+ | 3 + Interrupt |
| **定时管理** | DelayedDailyTick 自循环 | Cron/Timer Source 外部统一管理 |
| **多入口** | 7 种触发事件 | DailyItemAssigned + DailyItemSource (5 variants) |
| **例行流程** | 5 状态，每状态独立事件 | 内部 Phase Pipeline，一个 DailyStepEvent |
| **Hook** | 无 | 6 个 Hook 点 |
| **Health/Calendar** | 内部组件 | 独立 Plugin/Source，通过推送交互 |
| **Idle 协作** | 时间窗与 Idle 对齐 | 队列空时 Idle 自然运行，推送时唤醒 |
| **Cron 配置** | 散布在 daily 配置中 | 集中在 `sources.cron`，与 Daily 解耦 |

**核心原则**：
1. Daily Life System 就是一个带 Hook 的 FIFO 日常队列消费者。
2. **时间的感知不在 Daily System 内部**——Cron/Timer Source 在配置的时间点推送 `DailyItem`。
3. 用户查询、健康同步、日历更新同样通过推送入队，统一处理路径。
4. 例行执行 → 记录 → 反思的流程是内部 Phase Pipeline，不暴露为状态。
5. 柔性习惯提醒保留，是 `CheckHabits` routine 的内部行为。
6. 队列空时 Idle System 自然运行，推送时 Daily Life 自动接管。
