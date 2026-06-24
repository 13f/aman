# Aman Agent Idle → Boredom Flow

```
                         ┌─────────────────────────┐
                         │    Agent is IDLE         │
                         │  (no user messages,       │
                         │   local bus empty)        │
                         └────────────┬────────────┘
                                      │
                              idle depth++
                                      │
                         ┌────────────▼────────────┐
                         │   Depth 5+: BOREDOM      │
                         │   poll_count++ each tick  │
                         └────────────┬────────────┘
                                      │
                              poll_count == trigger_poll?
                                      │ yes
                                      ▼
                         ┌─────────────────────────────────┐
                         │  Weighted Random Pick            │
                         │  ┌───────┬────────┬──────┬─────┐│
                         │  │ work  │ study  │ fun  │idle ││
                         │  │ w=1.0 │ w=0.5  │w=0.3 │w=7.5││
                         │  └───────┴────────┴──────┴─────┘│
                         │                                  │
                         │  work_pressure (if configured):  │
                         │    queue_depth ↑ → work weight ↑ │
                         │    e.g. depth=10: work w=4.0     │
                         │          depth=30: work w=10.0   │
                         │    (idle tag → no-op)            │
                         └────────────┬────────────────────┘
                                      │ e.g. "work"
                                      ▼
                         ┌─────────────────────────┐
                         │  Filter Skills           │
                         │  tag == "work"           │
                         │  AND tag == "idle_run"   │
                         │                          │
                         │  ┌────────────────────┐  │
                         │  │ kanban-worker      │  │
                         │  │ btc-bottom-model   │  │
                         │  │ ...                │  │
                         │  └────────────────────┘  │
                         └────────────┬────────────┘
                                      │
                              pick random skill
                                      │
                                      ▼
                         ┌─────────────────────────┐
                         │  Pick idle_prompt        │
                         │  from SKILL.md frontmatter│
                         │                          │
                         │  "{agent_id}, check      │
                         │   your kanban for work"  │
                         └────────────┬────────────┘
                                      │
                                      ▼
                         ┌─────────────────────────┐
                         │  Publish MessageReceived  │
                         │  session_id: {agent}:idle │
                         │  session_type: background │
                         │  background: true         │
                         └────────────┬────────────┘
                                      │
                         ┌────────────▼────────────┐
                         │  MessageReceivedHandler   │
                         │  → ensure_session()       │
                         │  → spawn_process_message  │
                         └────────────┬────────────┘
                                      │
                         ┌────────────▼────────────┐
                         │  ReAct Loop (background) │
                         │                          │
                         │  LLM Call (retry 3x,     │
                         │    backoff 1s→2s→3s)     │
                         │      │                   │
                         │      ▼                   │
                         │  Tool Calls (retry 3x,   │
                         │    1s interval, skip     │
                         │    permanent errors)     │
                         │      │                   │
                         │      ▼                   │
                         │  LLM thinks...           │
                         │  (up to max_turns)       │
                         └────────────┬────────────┘
                                      │
                         ┌────────────▼────────────┐
                         │  Skill Execution         │
                         │                          │
                         │  ┌─ kanban-worker ─────┐ │
                         │  │ GET /projects        │ │
                         │  │ GET /works           │ │
                         │  │                      │ │
                         │  │ work found?          │ │
                         │  │  ├─ yes → execute    │ │
                         │  │  │  read context     │ │
                         │  │  │  run steps        │ │
                         │  │  │  update progress  │ │
                         │  │  │                   │ │
                         │  │  └─ no → "idle" exit │ │
                         │  └─────────────────────┘ │
                         │                          │
                         │  ┌─ btc-bottom-model ───┐│
                         │  │ check on-chain data  ││
                         │  │ analyze indicators   ││
                         │  │ write report         ││
                         │  └──────────────────────┘│
                         │                          │
                         │  ┌─ luck ───────────────┐│
                         │  │ generate secp256k1   ││
                         │  │ derive addresses     ││
                         │  │ check dormant list   ││
                         │  └──────────────────────┘│
                         └────────────┬────────────┘
                                      │
                         ┌────────────▼────────────┐
                         │  agent:reply_ready        │
                         │  (background: true,       │
                         │   skill_name, turn count) │
                         └────────────┬────────────┘
                                      │
                    ┌─────────────────┼─────────────────┐
                    ▼                 ▼                  ▼
            ┌──────────────┐  ┌──────────────┐  ┌──────────────┐
            │ Notification │  │ SessionStore  │  │ Agent State  │
            │ Subscriber   │  │ upsert +      │  │ Update       │
            │              │  │ JSONL persist │  │              │
            │ "minmax      │  │               │  │ Idle→Working │
            │  finished    │  │ session_id:   │  │ or Studying  │
            │  kanban-     │  │ {agent}:idle: │  │ or DailyLife │
            │  worker"     │  │ {random}      │  │              │
            │              │  │               │  │              │
            │ toast 3s     │  │ survives      │  │ UI reflects  │
            │ auto-dismiss │  │ restart       │  │ new state    │
            └──────────────┘  └──────────────┘  └──────────────┘
                                      │
                                      ▼
                         ┌─────────────────────────┐
                         │  Agent returns to IDLE    │
                         │  idle depth resets to 0   │
                         │  boredom_poll_count = 0   │
                         │  waits for next activity  │
                         └─────────────────────────┘
```

## WakeUp After Deep States (R10)

After any deep state completes (Sleep, Exploration, Meditation, Incubation), the WakeUp
Ouroboros cycle ensures the agent doesn't get stuck in deep idle:

```
Deep State Complete → ⏸ Quiet Period (60s) → 🌅 WakeUp (N poll steps)
                                                  ├─ depth → 0 (linear interpolate)
                                                  ├─ arousal → 1.0 (linear interpolate)
                                                  └─ Arrive at Active state
```

Sleep additionally has a cooldown (default 3600s) preventing immediate re-entry.

See `docs/idle-design.md` §4.1 and §14.10 for full details.

## Key Design Decisions

| Decision | Rationale |
|---|---|
| `session_type: "background"` | Separate from user chat sessions; no UI navigation |
| `session_id: {agent}:idle:{random}` | Each run isolated; no cross-run history collision |
| `ensure_session()` idempotent | Safe to call before every message; survives restart |
| LLM retry 3x (1s→2s→3s) | Handles transient API failures |
| Tool retry 3x (1s fixed) | Only on transient errors; skips unrecoverable/not-found |
| `idle_run` tag gate | Only skills explicitly opted-in to background execution |
| Work item `{agent}:work:{proj}:{work}` | Deterministic ID enables resume (断点续传) |
| Notification toast (3s auto-close) | Non-intrusive; agent activity visible but not disruptive |
| WakeUp Ouroboros after deep states | Prevents infinite sleep loop; progressive depth/arousal recovery |

## System State Mapping

After BoredomActor selects a tag and picks a skill, the agent's `AgentSystemState` is updated for UI visibility:

| Tag Selected | AgentSystemState | UI Display |
|---|---|---|
| `"work"` | `Working` | Working emoji / status |
| `"study"` | `Studying` | Studying emoji / status |
| `"internet"` \| `"entertainment"` \| `"fun"` | `DailyLife` | Daily life emoji / status |
| `"idle"` or any other | `Idle` | Idle (bored) emoji |

> **fix (68ac0a5)**: `"fun"` tag was previously missing from the DailyLife mapping and fell through to Idle — the UI showed bored emoji while the agent was playing.

## Work Pressure Configuration (R9)

When `work_pressure` is configured, the `BoredomActor` dynamically scales the weight of
the target tag based on the current queue depth. A growing work backlog increases the
probability of selecting work-related skills during idle periods.

### Pressure Curves

| Curve | Formula | Use Case |
|---|---|---|
| **Linear** | `multiplier = clamp(1.0 + slope × depth, 1.0, max)` | Steady ramp-up as backlog grows |
| **Sigmoid** | `multiplier = 1.0 + (max-1.0) / (1 + exp(-steepness × (depth-midpoint)))` | Sharp transition after backlog crosses a threshold |

### YAML Configuration

```yaml
idle:
  personality:
    boredom:
      trigger_poll: 3
      activities:
        - { tag: "idle", weight: 7.5 }
        - { tag: "work", weight: 1.0 }
        - { tag: "study", weight: 0.5 }
        - { tag: "fun", weight: 0.3 }
      # Optional: dynamic weight scaling based on work queue depth
      work_pressure:
        target_tag: "work"
        curve: "linear"        # or "sigmoid"
        slope: 0.3             # per queued item
        max_multiplier: 10.0   # cap at 10× base weight
```

### Effect at Various Depths (Linear, slope=0.3, max=10)

| Queue Depth | Multiplier | Effective work Weight | P(work) vs idle=7.5 |
|---|---|---|---|
| 0 | 1.0× | 1.0 | 11.8% |
| 5 | 2.5× | 2.5 | 25.0% |
| 10 | 4.0× | 4.0 | 34.8% |
| 20 | 7.0× | 7.0 | 48.3% |
| 30+ | 10.0× | 10.0 | 57.1% |

> **Design rationale**: Without work_pressure, the probability of doing work stays at ~12%
> regardless of backlog. With a linear slope of 0.3, a backlog of 30 items pushes the
> work probability above 50%, creating a natural backpressure loop: more pending work →
> agent more likely to work during idle → backlog shrinks → pressure eases.

### Per-Agent Idle-Run Availability Endpoint

`GET /agents/idle-availability` returns per-agent button availability using a three-step check:

1. **Skill tags**: any skill with `idle_run` + requested tag? (global)
2. **Team plugin**: is team plugin loaded and running? (global, work only)
3. **Work items**: does the agent have pending work items? (per-agent, work only)

Frontend fetches availability on mount and agent switch, disabling dropdown buttons
when the corresponding action is unavailable.
