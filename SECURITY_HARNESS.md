# Security Harness (安全防护)

Aman 的四层安全沙箱系统，保护用户本地运行的插件/hooks 免受恶意脚本攻击，
同时允许沙箱中的插件通过 HookContext 向事件总线推送事件。

## 架构概览

```
┌─────────────────────────────────────────────────────────┐
│ Layer 3: 能力权限模型 (Capability-Based Access Control) │
│  • 首次加载审批，后续自动放行                              │
│  • ~/.aman/plugins/{name}/.approved-caps.yaml 持久化     │
├─────────────────────────────────────────────────────────┤
│         ┌───────────────┬──────────────────┐            │
│         │               │                  │            │
│  Layer 1: WASM 加固     │  Layer 2: 子进程沙箱         │
│  • Fuel metering (100M) │  • Landlock (Linux 5.13+) │
│  • Epoch interruption   │  • Seatbelt (macOS)       │
│  • 500MB 内存限制        │  • 500MB 内存限制           │
│  • 1MB 栈深度限制       │  • 网络/进程生成控制         │
└─────────────────────────────────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────┐
│ Layer 4: 事件总线侧防护 (Event Bus-Side Protection)      │
│  • Rate limiting (token bucket per source)                │
│  • Sandboxed → sensitive event type rejection             │
│  • TrustLevel enforcement (Trusted > Untrusted > Sandboxed)│
└─────────────────────────────────────────────────────────┘
```

## Layer 1: WASM 加固

WASM 插件通过 wasmtime 的 fuel metering 和 epoch interruption 机制，
确保 CPU 和内存使用不会超过限制。

### 配置参数 (`WasmSecurityConfig`)

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `max_memory_bytes` | 524,288,000 (500 MB) | 最大线性内存 |
| `max_table_elements` | 10,000 | 最大间接调用表项数 |
| `max_fuel` | 100,000,000 | 总 fuel 单元 (≈1亿条WASM指令) |
| `epoch_interruption_ticks` | 1,000,000 | Epoch 计数器阈值 |

### 工作原理

1. **Fuel Metering**: 每条 WASM 指令消耗 1 个 fuel 单元。Fuel 耗尽后模块被 trap。
2. **Epoch Interruption**: 宿主定期递增 epoch 计数器，达到阈值时中断 WASM 执行。
3. **Stack Limit**: WASM 栈限制为 1MB，防止栈溢出攻击。
4. 每次函数调用前重置 epoch deadline，确保单次调用不会无限执行。

### 代码位置

- `crates/plugin/src/lib.rs` — `WasmSecurityConfig`, `WasmPluginRuntime`

---

## Layer 2: 子进程沙箱

子进程插件通过操作系统级沙箱机制限制文件系统、网络和进程权限。

### Linux: Landlock (kernel 5.13+)

Landlock 是 Linux 安全模块，允许非特权进程限制自身的文件系统访问。
通过 `prctl(PR_SET_NO_NEW_PRIVS)` + Landlock ruleset 实现不可逆的沙箱。

**要求**: Linux kernel 5.13+，`CONFIG_SECURITY_LANDLOCK=y`

**沙箱行为**:
- 默认拒绝所有文件系统访问
- 仅允许插件声明中的 `allowed_read_paths` 和 `allowed_write_paths`
- 禁止网络访问 (kernel 6.7+ 的 Landlock 网络规则暂未启用)
- 禁止生成子进程

### macOS: Seatbelt (sandbox-exec)

macOS 使用 `sandbox-exec` 命令行工具生成动态 Seatbelt profile。
程序化沙箱应用需要 `com.apple.security.temporary-exception.sandbox` 授权，
因此在 `pre_exec` 中通过环境变量 `AMAN_SANDBOX_PROFILE` 传递 profile。

**注意**: macOS 沙箱依赖 `sandbox-exec` 命令可用，当不可用时以 fail-open 模式运行。

### SandboxConfig

```rust
pub struct SandboxConfig {
    pub allowed_read_dirs: Vec<PathBuf>,    // 允许读取的目录
    pub allowed_write_dirs: Vec<PathBuf>,   // 允许读写的目录
    pub network_allowed: bool,              // 是否允许网络 (默认 false)
    pub process_spawn_allowed: bool,        // 是否允许子进程 (默认 false)
    pub max_memory_mb: u64,                 // 最大内存 MB (默认 500)
}
```

### 代码位置

- `crates/sandbox/src/lib.rs` — `SandboxConfig`, `apply_sandbox()`
- `crates/sandbox/src/linux.rs` — Landlock 实现
- `crates/sandbox/src/macos.rs` — Seatbelt profile 生成器
- `crates/plugin/src/bridge.rs` — `SubprocessPluginBridge::spawn()` 接收 sandbox_config

### 平台支持

| 平台 | 沙箱机制 | 状态 |
|------|---------|------|
| Linux (x86_64, aarch64, kernel 5.13+) | Landlock | ✅ 完整 |
| Linux (kernel < 5.13) | 无 | ⚠️ 无沙箱，日志警告 |
| macOS | Seatbelt (sandbox-exec) | ✅ 最佳努力 |
| 其他 | 无 | ⚠️ 无沙箱，日志警告 |

---

## Layer 3: 能力权限模型

每个插件在 `plugin.yaml` 中声明其所需的能力。首次加载时，
操作员需要审批这些能力；审批后持久化到本地文件。

### plugin.yaml 中的 security 声明

```yaml
name: my-plugin
version: 1.0.0
isolation: subprocess
runtime: python3
entrypoint: main.py

security:
  requested_capabilities:
    can_publish_events: true
    can_network: false
    allowed_read_paths:
      - /tmp/my-plugin
      - ~/.aman/data
    allowed_write_paths:
      - /tmp/my-plugin
    max_memory_mb: 500
    max_cpu_seconds: 300
    max_events_per_second: 50.0
```

### 能力字段说明 (`CapabilitySet`)

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `can_publish_events` | bool | false | 是否可向事件总线推送事件 |
| `can_subscribe_events` | bool | false | 是否可订阅事件 |
| `allowed_read_paths` | [Path] | [] | 允许读取的文件路径 |
| `allowed_write_paths` | [Path] | [] | 允许读写的文件路径 |
| `can_network` | bool | false | 是否允许网络连接 |
| `can_spawn_processes` | bool | false | 是否允许生成子进程 |
| `max_memory_mb` | u64 | 500 | 最大内存 (MB) |
| `max_cpu_seconds` | u64 | 300 | 最大 CPU 时间 (秒) |
| `max_events_per_second` | f64 | 50.0 | 每秒最多发布事件数 |

### 审批流程

1. **首次加载**: 插件声明 `security.requested_capabilities` → CLI 打印请求的能力列表 → 用户输入 y/N → 批准的能力写入 `~/.aman/plugins/{name}/.approved-caps.yaml`
2. **后续加载**: 检查已批准的能力是否覆盖当前请求 → 若无新增能力，自动放行 → 若有新增能力，重新提示审批
3. **版本变更**: 插件版本号变化时需要重新审批所有能力
4. **自动审批**: 配置 `security.auto_approve_plugins: true` 可跳过交互式审批

### 审批文件格式 (`.approved-caps.yaml`)

```yaml
plugin_version: "1.0.0"
capabilities:
  can_publish_events: true
  can_network: false
  allowed_read_paths:
    - /tmp/my-plugin
  allowed_write_paths:
    - /tmp/my-plugin
  max_memory_mb: 500
  max_cpu_seconds: 300
  max_events_per_second: 50.0
approved_at_ms: 1717171200000
approved_by: user
```

### 审批 CLI

加载插件时，如果检测到未审批的能力，会在 stderr 输出：

```
  Plugin "my-plugin v1.0.0" requests the following capabilities:

    - publish_events
    - read paths: ["/tmp/my-plugin", "~/.aman/data"]
    - write paths: ["/tmp/my-plugin"]
    - max memory: 500 MB
    - max CPU time: 300 seconds
    - max events/sec: 50

  Approve? [y/N]
```

### 配置开关

`config.yaml` 中 `security` 段的新增字段：

```yaml
security:
  sandbox_enabled: true           # 是否启用沙箱 (默认 true)
  sandbox_fail_open: false        # 沙箱错误是否放行 (默认 false)
  max_plugin_memory_mb: 500       # 默认插件最大内存 (MB)
  max_plugin_cpu_seconds: 300     # 默认插件最大 CPU 时间 (秒)
  auto_approve_plugins: false     # 是否自动审批所有能力 (默认 false)
```

### 代码位置

- `crates/core/src/security.rs` — `CapabilitySet`, `ApprovalCache`, `ApprovedCapabilities`
- `crates/plugin/src/lib.rs` — `PluginSecurityManifest`, `PluginLoader` 能力检查
- `crates/config/src/lib.rs` — `SecurityConfig` 新字段

---

## Layer 4: 事件总线侧防护

事件总线对所有来源实施速率限制和信任级别过滤。
即使沙箱插件有能力推送事件，事件总线也会限制其速率和事件类型。

### TrustLevel 执行

`TrustLevel` 从 `crates/source/src/registry.rs` 移至 `crates/core/src/types.rs`，
成为事件核心类型的一部分。每个事件携带 `trust_level` 字段：

```rust
pub enum TrustLevel {
    Trusted,    // 内部系统组件，无限制
    Untrusted,  // 用户提供的但已审核，中等限制
    Sandboxed,  // 隔离的插件/hook，严格限制
}
```

### 敏感事件类型拒绝

Sandboxed 来源不能发布以下事件类型：
- `ConfigChanged` — 配置变更
- `SecretRotated` — 密钥轮换
- `InjectionDetected` — 注入检测

默认启用，可通过 `InMemoryBusConfig.reject_sandboxed_sensitive_events = false` 关闭。

### Rate Limiting

基于 Token Bucket 算法，每个来源独立限制：

```rust
pub struct RateLimiterConfig {
    pub max_per_second: f64,  // 每秒最大事件数 (默认 100)
    pub burst: u32,           // 突发容量 (默认 200)
}
```

第一次超限返回 `Error::RateLimited` 并提示重试等待时间。

### 代码位置

- `crates/core/src/types.rs` — `TrustLevel` 枚举
- `crates/core/src/event.rs` — `Event.trust_level` 字段, `EventType::is_sensitive()`
- `crates/event-bus/src/lib.rs` — `InMemoryBus` trust-level 和 rate-limit 检查
- `crates/event-bus/src/rate_limiter.rs` — Token bucket 实现
- `crates/source/src/registry.rs` — `attach_trust_level()` 设置事件字段

---

## Hook 与事件总线

沙箱中的插件/hook 仍然可以通过 `HookContext.event_bus` 向事件总线推送事件。
事件总线侧的防护 (Layer 4) 会自动应用：

1. Hook 调用 `ctx.event_bus.publish(event)` → 
2. 事件到达 `InMemoryBus::publish()` → 
3. 检查 `event.trust_level` (如果是 Sandboxed) → 
4. 拒绝敏感事件类型 → 
5. Rate limiting 检查 → 
6. 正常入队 delivery

插件开发者不需要做任何额外工作 — 事件总线会自动处理所有的信任级别检查和速率限制。

---

## 新增 Crate

```
crates/sandbox/                     # 操作系统级沙箱
├── Cargo.toml
├── src/
│   ├── lib.rs                      # SandboxConfig, apply_sandbox()
│   ├── linux.rs                    # Landlock 实现 (Linux 5.13+)
│   └── macos.rs                    # Seatbelt 实现 (macOS)
```

---

## 新增/修改的文件

### 核心类型
- `crates/core/src/types.rs` — 新增 `TrustLevel` 枚举
- `crates/core/src/event.rs` — `Event` 新增 `trust_level` 字段；`EventType` 新增 `is_sensitive()` 方法
- `crates/core/src/security.rs` — **新文件**: `CapabilitySet`, `ApprovalCache`, `ApprovedCapabilities`
- `crates/core/src/error.rs` — 新增 `RateLimited`, `SecurityViolation`, `SandboxError` 错误变体
- `crates/core/src/lib.rs` — 导出 `security` 模块

### 沙箱
- `crates/sandbox/Cargo.toml` — **新 crate**
- `crates/sandbox/src/lib.rs` — **新文件**: 平台无关的沙箱接口
- `crates/sandbox/src/linux.rs` — **新文件**: Linux Landlock 实现
- `crates/sandbox/src/macos.rs` — **新文件**: macOS Seatbelt 实现

### 插件加载
- `crates/plugin/src/lib.rs` — `WasmSecurityConfig`, `PluginSecurityManifest`, `PluginLoader` 能力检查, `PluginManifest` 新增 `security` 字段
- `crates/plugin/src/bridge.rs` — `SubprocessPluginBridge::spawn()` 新增 `sandbox_config` 参数
- `crates/plugin/Cargo.toml` — 新增 `sandbox` 依赖

### 事件总线
- `crates/event-bus/src/lib.rs` — `InMemoryBus` 新增 rate limiter 和 trust-level 执行
- `crates/event-bus/src/rate_limiter.rs` — **新文件**: Token bucket 速率限制器

### 配置
- `crates/config/src/lib.rs` — `SecurityConfig` 新增 sandbox_enabled, sandbox_fail_open, max_plugin_memory_mb, max_plugin_cpu_seconds, auto_approve_plugins 字段

### 来源注册
- `crates/source/src/registry.rs` — `TrustLevel` 移至 core；`attach_trust_level()` 更新为新机制

### 工作空间
- `Cargo.toml` — 新增 `crates/sandbox`; `unsafe_code` 改为 `deny` (非 `forbid`)

---

## 运行测试

```bash
# 构建全部
cargo build --workspace

# 运行所有测试
cargo test --workspace

# 针对安全模块运行测试
cargo test -p kernel --lib security
cargo test -p sandbox
cargo test -p event-bus --lib rate_limiter

# 检查 lint
cargo clippy --workspace -- -D warnings

# 文档
cargo doc --workspace --no-deps
```

---

## 故障排除

### Landlock 不可用

```
Landlock not supported (kernel < 5.13 or not enabled)
```

**解决方案**:
- 确认内核 >= 5.13: `uname -r`
- 确认 Landlock 已启用: `cat /boot/config-$(uname -r) | grep LANDSOCK`
- 如果不可用，插件将以无沙箱模式运行 (日志警告)

### macOS sandbox-exec 报错

```
sandbox-exec: command not found
```

**解决方案**:
- `sandbox-exec` 已包含在 macOS 中 (无需额外安装)
- 如果确实缺失，插件将以无沙箱模式运行

### 能力审批提示意外出现

```
Plugin "xxx" requests capabilities but no approval cache configured
```

**解决方案**:
- 确认 `~/.aman/plugins/` 目录存在且可写
- 确认 `ApprovalCache` 已正确传入 `PluginLoader`

### 速率限制错误

```
[AmanExistence] rate limited: source xxx, retry after 100ms
```

**原因**: 插件发布事件速度超过配置的速率限制。
**解决方案**: 在插件的 `security.requested_capabilities` 中提高 `max_events_per_second`，或在事件总线配置中提高 `rate_limiter.max_per_second`。

---

## 未来增强

1. **gVisor/Firecracker 集成**: 对高风险插件使用微虚拟机级隔离
2. **Landlock 网络规则**: 当 kernel 6.7+ 普及后，添加 `LANDLOCK_ACCESS_NET_BIND_TCP` 等网络限制
3. **Plugin Catalog**: 集中管理的已审核插件目录，预审批已知插件的能力
4. **Hook 脚本沙箱**: 将 `ScriptHook` (`crates/hook/src/lib.rs`) 也纳入沙箱保护
5. **Seccomp-BPF**: 对子进程插件添加系统调用过滤
