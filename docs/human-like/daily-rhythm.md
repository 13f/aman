# 日常节律层 — 身体时钟与习惯

> Agent 如果每次醒来都是"永恒的当下"，就无法理解：
> "今天是周一早晨，该做每周计划"、
> "已经连续 3 天没冥想，该提醒一下"。
>
> Aman 通过 **TimeWindow**（时间窗）+ **Cron Source**（外部时钟）+ **Habit Escalation**（习惯升级提醒），
> 为 Agent 建立**类人类的日常节律系统**。

---

## 1. 设计哲学

```
Agent 的时间感知不在 Daily System 内部。
Cron/Timer Source 在配置的时间点推送 DailyItem——
时间的"滴答"来自外部，Agent 只响应：
  "到了早晨，该做例行了"
  "到了晚上，该回顾了"
```

**核心原则：Daily Life System 是 `LifecycleEngine<DailySpec>` 的薄封装。**
时间的感知、任务的触发、步骤的执行全部由引擎提供，
Daily Life 只做领域特有的逻辑（查日历、看天气、检习惯、引导反思）。

---

## 2. 时间窗（TimeWindow）

```rust
pub enum TimeWindow {
    Morning,     // 06:00–11:59
    Midday,      // 12:00–13:59
    Afternoon,   // 14:00–17:59
    Evening,     // 18:00–20:59
    Night,       // 21:00–05:59
}
```

每个 TimeWindow 对应一套**例行事项（Routines）**——
不同时间段 Agent 做不同的事：

| 时间窗 | 例行事项 | 拟人化 |
|---|---|---|
| **Morning** | CheckCalendar → CheckWeather → CheckHabits → DailyBrief → MorningReflection | "起床了，看看今天有什么安排" |
| **Midday** | CheckHabits → CheckHealth | "该活动一下了" |
| **Evening** | CheckHabits → CheckHealth | "今天过得怎么样" |
| **Night** | CheckHabits → GuideReflection → GenerateTomorrowPlan | "睡前回顾，准备明天" |

---

## 3. Cron Source — 外部时钟管理

Daily System **不管理时钟**。时间触发由全局 Cron Source 统一管理：

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

### 3.1 日常时间线（Daily Timeline）

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

## 4. Routine 类型

```rust
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

### 4.1 CheckCalendar — 看日程

```rust
async fn check_calendar(&self, days_ahead: u32) -> Result<StepOutput> {
    let events = self.calendar.get_events(today, today + days_ahead).await?;
    // 格式化输出：今天下午 3 点有个会、明天上午代码审查...
}
```

### 4.2 CheckWeather — 天气播报

```rust
async fn check_weather(&self) -> Result<StepOutput> {
    let forecast = self.fetch_weather().await?;
    // "今天晴，最高 28°C，建议穿薄外套"
}
```

### 4.3 CheckHabits — 习惯检查 + Escalation

```rust
async fn check_habits(&self, window: TimeWindow) -> Result<StepOutput> {
    let (completed, reminders) = self.check_habits(window).await?;
    for reminder in reminders {
        self.deliver_reminder(&reminder).await?;
    }
}
```

### 4.4 CheckHealth — 健康监测

```rust
async fn check_health(&self) -> Result<StepOutput> {
    let snapshot = self.health_data.fetch_snapshot().await?;
    // 异常检测：睡眠 < 6h / 步数 < 3000 / 连续 3 天 mood < 3
}
```

### 4.5 GuideReflection — 反思引导

```rust
async fn guide_reflection(&self, template: &str) -> Result<StepOutput> {
    let (reflection, insights) = self.run_reflection(template).await?;
    self.persistence.save_reflection(&reflection).await?;
    // "今天最大的收获是什么？明天最想改进的一点？"
}
```

---

## 5. Habit Escalation — 习惯提醒升级

连续未完成习惯时，提醒语气逐步升级：

```
Habit: 晨间冥想 (TimeWindow: Morning, target: Daily)

Day 1: 09:00 未完成 → HabitReminder(urgency=Gentle)  "记得冥想哦~"
Day 2: 09:00 未完成 → HabitReminder(urgency=Friendly) "今天也别忘了冥想"
Day 3: 09:00 未完成 → HabitReminder(urgency=Firm)    "连续 3 天了，该冥想了"
Day 7: 09:00 未完成 → HabitReminder(urgency=Concerned)
        "要不要把目标降到每天 1 分钟？习惯比强度重要。"
```

```yaml
habits:
  - id: "morning-meditation"
    name: "晨间冥想"
    habit_type: duration
    target: { daily: 10 }        # 10 分钟
    trigger_window: morning
    reminder:
      enabled: true
      preferred_time: "07:00"
      re_remind_interval: 1800s
      max_reminders: 2
      escalation_days: 3         # 连续 3 天开始升级
```

**拟人化含义**：不是冷冰冰的定时提醒，
而是"温和但坚持"的同伴——时间久了会"担忧"你的状态。

---

## 6. Health Integration

```
┌──────────────┐     HealthDataSync      ┌─────────────────┐
│ Health Plugin│ ───────────────────────→│ Daily System     │
│ (Apple Health│    DailyItemAssigned    │ (被动消费)       │
│  Fitbit, etc)│                         │                  │
└──────────────┘                         └─────────────────┘
```

健康异常触发：
- 睡眠 < 6h → `HealthAnomaly` 事件 → Notification 推送
- 连续 3 天 mood < 3 → 干预建议
- 步数 < 3000 → 活动提醒

---

## 7. Lifecycle Engine — 通用引擎

Daily Life System 与 Work/Study System 共用 `LifecycleEngine<S>`：

```
LifecycleEngine<DailySpec>  ← 通用引擎（lifecycle crate）
  └─ DailySpec              ← 领域适配（daily-life crate）
       ├─ Item  = DailyItem
       ├─ Step  = Routine
       ├─ decompose()       → 根据 TimeWindow 确定例行事项列表
       ├─ execute_step_impl() → 执行单个例行
       └─ collect_result()  → 收集日志 + 健康快照 + 习惯完成记录
```

**引擎在状态切换时自动更新 `AgentSystemState`**：
- `Idle` → `AgentSystemState::Idle`
- `Busy` → `AgentSystemState::DailyLife`

---

## 8. 配置示例

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

  health:
    metrics: [steps, active_energy, sleep_duration, weight, mood]
    anomaly_thresholds:
      sleep_duration:
        low: 6.0
        high: 10.0

  queue:
    max_size: 50
```

---

## 9. Event Routing

```yaml
routes:
  - match: { event_type: "daily.item.assigned" }   → handler:daily_life
  - match: { event_type: "daily.item.completed" }  → handler:daily_life
  - match: { event_type: "daily.item.failed" }     → handler:daily_life
  - match: { event_type: "daily.interrupt" }       → handler:daily_life
```

---

## 10. 实现位置

| 组件 | 文件 | 职责 |
|---|---|---|
| DailyLifeSystem | `kernel/daily-life/src/lib.rs` | 薄 wrapper |
| DailySpec | `kernel/daily-life/src/spec.rs` | SystemSpec trait 实现 |
| DailyItem / TimeWindow | `kernel/daily-life/src/types.rs` | 领域类型 |
| Cron Source | `kernel/source/src/cron.rs` | 外部时钟推送 |
| LifecycleEngine | `kernel/lifecycle/src/engine.rs` | 通用状态机引擎 |
| Health Integration | `kernel/plugins/health/` | 健康数据 + 异常检测 |

---

> **参考：**
> - [Daily Life 设计文档](../daily-life-design.md)
> - [Maslow 需求层次](../maslow-hierarchy.md) — 生理需求映射
> - [Daily Life 代码](../../kernel/daily-life/)
