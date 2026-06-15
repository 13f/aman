# Configuration Reference

aman uses a layered configuration system with YAML files (`~/.aman/config.yaml`) and
environment variable overrides.

The top-level config struct is `AmanConfig` (`kernel/config/src/lib.rs`), which
flattens the `AgentConfig` runtime config alongside multi-agent, provider, memory,
evaluation, and hook sections.

## Configuration File (`~/.aman/config.yaml`)

```yaml
# ── Runtime ──────────────────────────────────────────────────────
runtime:
  drain_timeout_sec: 30           # Graceful shutdown drain timeout
  tool_timeout_sec: 60            # Per-tool execution timeout

gateway:
  port: 9999                      # Gateway HTTP listen port

# ── Event Bus ────────────────────────────────────────────────────
event_bus:
  mode: persistent                # "in_memory" | "persistent"
  max_queue_size: 10000           # Max events in queue before backpressure
  persistence:                    # Only valid when mode=persistent
    wal_sync: fsync               # "fsync" | "batch"
    checkpoint_interval: 500      # WAL checkpoint every N events

# ── Plugin ───────────────────────────────────────────────────────
plugin:
  enforce_dependency_check: true  # Validate plugin dependency graph

# ── Source ───────────────────────────────────────────────────────
source:
  notify_on_complete: false       # Emit notification on source completion
  watch_patterns: []              # File watch globs (mutually exclusive with notify_on_complete)

# ── Workflow Definitions ─────────────────────────────────────────
workflow:
  definitions:                    # List of named workflow state machines
    - name: approval
      states: [PENDING, APPROVED, REJECTED]
      initial_state: PENDING

# ── Security ─────────────────────────────────────────────────────
security:
  secrets_mode: env               # "env" | "keyring" | "1password"
  risky_capabilities_enabled: false

# ── Idle System ──────────────────────────────────────────────────
idle:
  enabled: true
  reflection:
    enabled: true
    timeout_secs: 30
    check_items: [chain_tasks, immediate_errors, lessons_learned]
  personality:
    enabled_kinds: [daze, boredom, sleep, exploration, meditation, incubation]
    depth_schedule:
      - [0, daze]
      - [5, boredom]
      - [20, sleep]
      - [50, exploration]
      - [100, meditation]
      - [200, incubation]
    poll_interval:
      interval_secs: 5.0          # Fixed interval; also supports Linear { base, multiplier }
    poll_relaxation: none         # "none" | Linear { slope } | Exponential { factor }
    chat_mode:
      allowed_kinds: [daze, boredom]
      grace_period_secs: 60
      poll_interval:
        interval_secs: 2.0
    reflection_breaker:
      max_consecutive: 5
      cooldown_secs: 300
    context_isolation:
      pollute_chat_history: false
      suspend_on_user_input: true
  boredom:                        # Optional boredom random-action config
    trigger_poll: 5
    activities:
      - tag: idle
        weight: 0.5
      - tag: work
        weight: 0.3
    work_pressure:                # Optional dynamic weight scaling
      target_tag: work            # Which activity tag gets pressure boost
      curve: linear               # "linear" | "sigmoid"
      slope: 0.3
      max_multiplier: 10.0
  arousal:
    initial_value: 1.0
    half_life_secs: 900
  context:
    max_output_buffer: 10
  sleep:
    short_term_retention_days: 7
    cache_expiry_days: 30
    max_cpu_seconds: 300
  exploration:
    api_rate_per_minute: 10
    on_quota_exhausted: fallback
    cooldown_secs: 3600
  incubation:
    max_concurrent: 1
    enabled: true
    cooldown_secs: 10800
    incubation_threshold: 0.7
    high_value_threshold: 0.85
    cancel_timeout_secs: 5
  meditation:
    cooldown_secs: 7200
    min_interval_ticks: 20
    review_depth: 20

# ── Work System ──────────────────────────────────────────────────
work:
  execution:
    auto_decompose: true
    step_timeout: 120             # seconds
    inter_item_cooldown: 0        # seconds
  hooks:
    before_execution: []
    before_step: []
    after_step: []
    after_execution: []
    on_success: []
    on_failure: []
  queue:
    max_size: 100
    priority_queue: false
  retry:
    max_step_retries: 3
    retry_delay: 5                # seconds

# ── Study System ─────────────────────────────────────────────────
study:
  execution:
    auto_decompose: true
    step_timeout: 120
    inter_item_cooldown: 0
  hooks:
    before_execution: []
    before_step: []
    after_step: []
    after_execution: []
    on_success: []
    on_failure: []
  queue:
    max_size: 100
    priority_queue: false
  retry:
    max_step_retries: 3
    retry_delay: 5
  default_depth: read             # read | practice | teach | master
  phase_timeout: 600              # seconds
  materials:
    auto_gather: true
    search_sources: [arxiv, web_search, local_knowledge_graph]
    max_candidates: 10
    min_relevance: 0.6
  learning:
    max_module_duration: 600      # seconds
    min_comprehension: 0.7
    auto_practice: true
  spaced_repetition:
    intervals_days: [1, 3, 7, 14, 30, 60, 120]
    max_review_rounds: 7
    ease_factor: 2.5
    min_interval_on_fail: 1
  knowledge_graph:
    min_connections: 2
    auto_connect: true
    similarity_threshold: 0.6

# ── Daily Life System ────────────────────────────────────────────
daily_life:
  execution:
    auto_decompose: true
    step_timeout: 120
    inter_item_cooldown: 0
  hooks:
    before_execution: []
    before_step: []
    after_step: []
    after_execution: []
    on_success: []
    on_failure: []
  queue:
    max_size: 100
    priority_queue: false
  retry:
    max_step_retries: 3
    retry_delay: 5
  timezone: Asia/Shanghai
  routines:
    morning: []
    midday: []
    afternoon: []
    evening: []
    night: []

# ── Context Compression ──────────────────────────────────────────
compression:
  threshold: 0.80                 # Fraction of context window to trigger compression (80%)
  tail_budget_ratio: 0.20         # Fraction of tokens reserved for TAIL (20%)
  protect_head_messages: 2        # Messages at start always kept
  min_tail_messages: 3            # Messages at end always kept
  anti_thrashing: true            # Pause if 2 consecutive runs save < min_savings_pct
  min_savings_pct: 10.0           # Minimum savings percentage for effective compression
  max_tool_args_chars: 500
  dedup_tool_outputs: true
  summarize_tool_results: true
  truncate_tool_args: true

# ── Self Module (Python bridge) ──────────────────────────────────
self_module:
  enabled: true
  python: python3
  bridge_script: self/bridge.py

# ── Multi-Agent: LLM Providers ───────────────────────────────────
llm:
  api_type: openai                # "openai" | "claude" (falls back per-provider)

providers:
  openai:
    display_name: OpenAI
    base_url: https://api.openai.com/v1
    api_type: openai              # Optional per-provider override
    api_key: null                 # Optional inline key (use $KEYCHAIN:... or $ENV:...)
    models:
      - id: gpt-5
        model_id: gpt-5-turbo
      - id: gpt-4o
        model_id: gpt-4o

model:                            # Default LLM model selection
  default: gpt-5
  provider: openai
  base_url: https://api.openai.com/v1

models:                           # Global model parameter definitions
  gpt-5:
    max_context_tokens: 128000
    max_output_tokens: 16384
  gpt-4o:
    max_context_tokens: 128000
    max_output_tokens: 16384

# ── Multi-Agent: Agent Definitions ───────────────────────────────
agents:
  cortana:
    display_name: Cortana
    provider: openai
    model: gpt-5
    system_prompt_override: null  # Optional per-agent prompt
    enabled: true
    tools:                        # Optional per-agent tool access
      allow: ["*"]                # ["*"] = all tools; or a specific list
      deny: []
    skills: null                  # null = all skills; or a list of skill names
    event_bus:                    # Optional per-agent bus overrides
      max_queue_size: 1000
      mode: persistent

# ── Memory Subsystem ─────────────────────────────────────────────
memory:
  provider: yantrikdb
  embedding:
    embedder: potion-multilingual-128M       # Download mode (local ONNX, zero network)
    # provider: ollama                       # Remote mode (OpenAI-compatible /v1/embeddings)
    # model: qwen3-embedding-8b              #   works: Ollama, oMLX, LM Studio, OpenAI
  llm:                                  # Memory extraction LLM
    provider: deepseek
    model: deepseek-v4-flash

# ── Script Hooks ─────────────────────────────────────────────────
hooks:
  - name: webhook-alert
    on: agent:busy                # Single event or list: [session:started, agent:busy]
    script: ./hooks/alert.py
    runtime: python3
    min_version: ">=3.8"          # Optional

# ── Evaluation System ────────────────────────────────────────────
eval:
  enabled: true
  default_threshold: 0.7
  auto_evaluate: true
  persist_results: true
  max_results: 1000
  sample_rate: 1.0
  llm:                            # Optional LLM-as-judge
    provider: deepseek
    model: deepseek-v4-flash
    temperature: 0.3
  rules: []                       # Custom evaluation rules

# ── Messaging Channels (raw JSON, plugin-deserialized) ───────────
channels: null

# ── Info-Hub Plugin Config (raw JSON, plugin-deserialized) ──────
info_hub: null
```

## Environment Variables

A limited set of config fields can be overridden via `AMAN_*` environment variables.
Single underscores (`_`) separate nested keys — **not** double underscores.

```bash
export AMAN_EVENT_BUS_MODE=persistent
export AMAN_EVENT_BUS_MAX_QUEUE_SIZE=50000
export AMAN_RUNTIME_DRAIN_TIMEOUT_SEC=60
export AMAN_RUNTIME_TOOL_TIMEOUT_SEC=120
export AMAN_SOURCE_NOTIFY_ON_COMPLETE=true
export AMAN_SOURCE_WATCH_PATTERNS=*.log,*.txt
export AMAN_SECURITY_RISKY_CAPABILITIES_ENABLED=false
```

Environment variables take precedence over YAML config but are overridden by runtime
override files. Only the 7 variables listed above are supported — not all config fields
are env-overridable.

## Config Layers

| Layer | Source | Priority |
|---|---|---|
| 1 | Hardcoded defaults (`Default` impls) | Lowest |
| 2 | `~/.aman/config.yaml` file | Medium |
| 3 | `AMAN_*` environment variables | High |
| 4 | Runtime override YAML file | Highest |

## Secret Injection Convention

API keys and other secrets are **not** stored in config files. Use one of:

- **Environment variables**: `export AMAN_PROVIDER_OPENAI_API_KEY=sk-...`
- **OS keyring**: `$KEYCHAIN:aman.providers.openai.api_key`
- **1Password CLI**: configured via `security.secrets_mode: 1password`

The `ProviderConfig.api_key` inline field exists but should only be used for
development; secrets in YAML are redacted from logs by `kernel::redactor`.
