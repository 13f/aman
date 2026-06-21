从源码 /Users/jerin/.hermes/hermes-agent/agent/system_prompt.py 看，完整组装流程一目了然。
    
    
    
    核心设计：三层结构 + 一次缓存
    
    Hermes 的 system prompt 在会话开始时一次性构建，之后缓存不动（除非触发 context compression）。这是为了保住上游 LLM 的 prefix cache。
    
    三层按 stable → context → volatile 顺序拼装，每层内部用 \n\n 连接各块。
    
    
    
    STABLE 层（构建一次，全程不变）
    
    1. Agent 身份
    - 先尝试加载 ~/.hermes/SOUL.md，有内容就替代默认身份
    - 没有 SOUL.md 则用硬编码的 DEFAULT_AGENT_IDENTITY："You are Hermes Agent, an intelligent AI assistant created by Nous Research..."
    
    2. Hermes 自身帮助指引
    - 告诉模型遇到 Hermes 配置/使用问题要去看 docs + 加载 hermes-agent skill
    
    3. 任务完成纪律（Finishing the job）
    - 要求模型不要停在 stub/计划，必须真正跑出结果
    - 由 agent.task_completion_guidance 控制（默认 true），仅在有工具时才注入
    
    4. 并行工具调用指引
    - 要求模型把互不依赖的工具调用 batch 到一起发，减少 round-trip
    - 由 agent.parallel_tool_call_guidance 控制（默认 true）
    
    5. 工具感知行为引导（仅在对应工具加载时注入）
    - memory 工具 → 持久记忆的使用规则（存什么、不存什么）
    - session_search 工具 → 会话搜索的使用规则
    - skill_manage / skill_view → 技能系统的使用规则
    - kanban_show → Kanban 工作流指引（仅在 dispatcher 派生的 worker 进程中）
    
    6. 中途转向提示（Steer Channel）
    - 告诉模型 [OUT-OF-BAND USER MESSAGE] 包裹的文本是用户的实时指令而非工具输出
    
    7. Computer-Use 指引（仅 macOS 且加载了 computer_use 工具集时）
    
    8. Nous 订阅提示（仅通过 Nous Portal 订阅时）
    
    9. 工具使用强制执行（Tool-Use Enforcement）
    - 告诉模型"你必须实际调用工具，不要只描述你打算做什么"
    - 注入策略：auto（默认，按模型黑白名单） / true（始终注入） / false（不注入） / list（自定义模型名列表）
    - 当前模型的匹配：deepseek 系不在此名单，所以你不必看到这段
    
    10. 模型族专属操作指引
    - Google Gemini/Gemma → 简洁性、绝对路径、并行调用
    - OpenAI GPT/Codex / xAI Grok → 工具持久性、前置检查、反幻觉
    
    11. Skills Index（技能索引）
    - 列出所有 available_skills 的树形结构
    - 包含"Before replying, scan the skills below..."的强制性指令
    - coding focus 模式下非代码类技能会折叠为名称列表
    
    12. Alibaba API 模型名修正（仅 alibaba provider）
    
    13. 环境提示（Environment Hints）
    - WSL、Termux、macOS 等平台的路径翻译和适配提示
    - 当前 shell、用户 home、cwd、terminal backend 类型
    
    14. Coding Posture（代码工作区姿态）
    - 当前工作目录的 git 状态、分支信息
    - 仅在交互式 coding 界面 + 代码工作区时注入
    
    15. Python 工具链探测
    - 检测 python/pip/uv 的版本和 PEP 668 状态，仅非默认时输出一行
    - 由 agent.environment_probe 控制（默认 true），仅在 local backend 有效
    
    16. Active Profile 提示
    - 告诉当前在哪个 profile 下，以及禁止跨 profile 修改的规则
    - "Active Hermes profile: default. Other profiles... Do not modify another profile's..."
    
    17. Platform Hint（平台提示）
    - CLI: "You are a CLI AI Agent. Try not to use markdown..."
    - Telegram: "Prefer shorter messages; split long answers..."
    - 可由 config.yaml 的 platform_hints.<platform> 做 append/replace
    
    
    
    CONTEXT 层（会话级稳定，依赖 cwd）
    
    18. Caller-supplied System Message
    - 来自 API 调用或网关传入的 system_message（可选）
    
    19. 项目上下文文件
    - 从 TERMINAL_CWD（或启动目录）发现并注入，优先级链（第一个命中即用）：
      1. .hermes.md / HERMES.md（沿目录树向 git root 搜索）
      2. AGENTS.md（仅 cwd）
      3. CLAUDE.md（仅 cwd）
      4. .cursorrules / .cursor/rules/*.mdc
    - 如果 SOUL.md 已加载则 skip_soul=True 避免重复
    
    
    
    VOLATILE 层（每会话可能不同，但仍然缓存）
    
    20. MEMORY.md 快照（Agent 笔记）
    - 从 ~/.hermes/memories/MEMORY.md 冻结读取
    - 容量上限 2,200 chars，显示百分比 + § 分隔
    - 由 memory.memory_enabled 控制
    
    21. USER.md 快照（用户画像）
    - 从 ~/.hermes/memories/USER.md 冻结读取
    - 容量上限 1,375 chars
    - 由 memory.user_profile_enabled 控制
    
    22. External Memory Provider Block
    - 如果配置了外部 memory provider（Mem0、Honcho 等），注入其 system prompt 块
    
    23. 时间戳 / Session / Model / Provider 行
    - 日期精度（非分钟），保持整天的 byte-stable
    - 格式："Conversation started: Sunday, June 21, 2026" + Model: deepseek-v4-pro + Provider: deepseek
    - 如果 pass_session_id 为 true，追加 Session ID: xxx
    
    
    
    流程图
    
    
    ┌─────────────────────────────────────────────────────┐
    │ SYSTEM PROMPT (一次构建，会话级缓存)                    │
    ├─────────────────────────────────────────────────────┤
    │ STABLE                                               │
    │  1. SOUL.md / 默认身份                                │
    │  2. Hermes 帮助指引                                   │
    │  3. 任务完成纪律 (finishing the job)                   │
    │  4. 并行工具调用指引                                   │
    │  5. 工具感知行为引导 (memory/session_search/skills)      │
    │  6. Steer channel 提示                               │
    │  7. Computer-use 指引 (macOS only)                    │
    │  8. Nous 订阅提示                                     │
    │  9. 工具使用强制执行 (按模型注入)                        │
    │ 10. 模型族专属指引 (Gemini/GPT/Grok)                   │
    │ 11. 技能索引 (available_skills)                       │
    │ 12. Alibaba 模型名修正                                │
    │ 13. 环境提示 (OS/shell/cwd)                           │
    │ 14. Coding posture (git状态)                          │
    │ 15. Python 工具链探测                                 │
    │ 16. Active profile 提示                               │
    │ 17. Platform hint (CLI/Telegram...)                   │
    ├─────────────────────────────────────────────────────┤
    │ CONTEXT                                              │
    │ 18. Caller system_message (optional)                  │
    │ 19. 项目上下文文件 (AGENTS.md / .cursorrules 等)       │
    ├─────────────────────────────────────────────────────┤
    │ VOLATILE (快照)                                       │
    │ 20. MEMORY.md 快照                                   │
    │ 21. USER.md 快照                                     │
    │ 22. External memory provider                         │
    │ 23. 时间戳 + Model + Provider                        │
    └─────────────────────────────────────────────────────┘
                                ↕ 两次构建之间不变（保护 prefix cache）
    
    
    
    
    关键不变量
    
    1. 唯一重建时机：context compression 触发后才 invalidate_system_prompt() 然后重建
    2. 子进程继承：delegate_task 的子 agent 以及 background review fork 都继承父级的缓存 prompt，不做重复构建
    3. skip_context_files：cron / 某些 subagent 模式下不加载 SOUL.md 和项目上下文，改用硬编码 identity
    4. Frozen snapshot：MEMORY.md 和 USER.md 在会话开始时冻结，中途 memory 工具写入磁盘但当前会话的 prompt 不变，下次会话才生效