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
                         ┌─────────────────────────┐
                         │  Weighted Random Pick    │
                         │  ┌───────┬─────────────┐│
                         │  │ work  │ study │ fun  ││
                         │  │ 40%   │ 30%   │ 20%  ││
                         │  └───────┴──────┴──────┘│
                         │  (idle=10% → no-op)      │
                         └────────────┬────────────┘
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
