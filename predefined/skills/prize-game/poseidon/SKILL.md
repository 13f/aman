---
name: poseidon-collision
description: >
  Iteratively search for Poseidon hash collisions to claim bounties
  from the Poseidon Initiative. Design strategies, run experiments,
  record results, and refine approaches.
category: research
tags:
  - idle_run
  - fun
  - prize
  - crypto
  - hash-collision
  - algebraic-attack
  - bounty
  - poseidon
  - gröbner
triggers:
  - "挑战"
  - "challenge"
  - "prize"
  - "game"
  - "游戏"
idle_prompt:
  - "挑战"
  - "challenge"
  - "prize"
  - "game"
  - "游戏"
---

# Poseidon Collision Search

## 1. Challenge Overview

**Prize pool**: $992,000 | **Deadline**: Jan 1, 2029
**Submit to**: dmitry.khovratovich@ethereum.org
**Rules**: First come first win. 1 month after submission → publish attack report & code.

Find two distinct 15-element inputs `X ≠ Y` such that Poseidon compression
(with prefix `0xc09de4`) produces outputs where the first `q` elements match.

| q | Award | Status |
|----|-------|--------|
| 1 | — | ✅ |
| 2 | — | ✅ (claimed) |
| 3 | $32K | ✅ (claimed Apr 6, 2026) |
| 4 | **$64K** | 🎯 **我们的目标** |
| 5 | $128K | — |
| 6 | $256K | — |
| 7 | $512K | — |

**Hash function**: Poseidon SPN over KoalaBear prime field
- p = 2³¹ − 2²⁴ + 1 = 2130706433
- t = 16, α = 3 (cube), RF = 8, RP = 20
- 压缩模式: `[SEED=0xc09de4, 15 inputs] → permutation → +feedforward → output`
- 官方验证器: https://github.com/khovratovich/poseidon-tools
- **本地副本**: `scripts/poseidon-tools/` (vendored, 2026-06-10 main branch)

**🔥 MDS 矩阵可以任选！** 只需满足 "无不变子空间轨迹" 条件。
Plonky3 循环矩阵只是示例。这意味着可以**选择一个结构更弱、更容易攻击的 MDS**——
这是整个挑战中最大的自由度，可能是突破口。

## 2. Current State — READ THIS FIRST

<!--
  STATE 取值:
    “thinking”     → 需要构思新策略，进入 §7 Thinking Log
    “running-vN”   → 检查 attacks/vN/checkpoint.json，尝试断点续传
    “give-up-vN”   → 放弃 vN，进入 thinking 模式
    “done”         → 🛑 停止！代码可运行 → 提示用户自行运行
-->

**STATE: `thinking`**

> v1 (Floyd rho q=1) 已完成验证。q≥4 暴力不可行，需要代数攻击。
> 进入深度思考模式，探索新策略。关键发现: cube root 在 𝔽_p 上是双射 (gcd(3,p-1)=1)。
> **🔥 MDS 矩阵可以任选！** 可以选弱 MDS 降低攻击难度。

## 🛑 CRITICAL: Termination Rule

**此技能以探索新策略为主。LLM 的职责是设计策略和编写代码——不是运行实验或调参。**

### 硬性停止条件（满足任一立即停止，更新 STATE=”done”）:

1. 🔴 **找到有效碰撞** → 立即停止。不要 “再找一个更好的”。
2. 🔴 **代码语法正确 + 导入正确 + 逻辑完整** → 停下来，提示用户运行。
3. 🔴 **同一策略已修改 >2 次** → 不管是否收敛，停止并记录结果。
4. 🔴 **同一个 bug/错误重复 >3 次** → 停止，描述问题让用户解决。

### 变体预算（超预算立即停止）:

| 限制项 | 预算 | 说明 |
|--------|------|------|
| 每个策略的变体数 | ≤ 2 | 试 2 个方向后还没收敛 → 放弃 |
| 每个会话的 attack.py 写入次数 | ≤ 5 | 超过 5 次写入 → 停止 |
| MDS 矩阵测试数 | ≤ 3 | 测试 >3 个 MDS 不会增加洞察 |
| 每个实验的运行次数 | ≤ 2 | 运行 >2 次同一脚本只说明你在调参 |

## 3. 🧱 Design Constraints — Read Before Designing Any Strategy

**新策略必须通过以下检查后才允许进入 CODE 阶段。**

### 规则 1: 先验证 MDS，再设计攻击 🔴

**M=I 禁止使用。** 官方 `verify_mds_matrix(I)` 返回 False（A1–A4 四项检查全部失败）。
`verify_collision_solution` 在检查碰撞之前（第 148 行）先调用 `verify_mds_matrix`，
M=I 碰撞将被直接拒绝。

| 要求 | 说明 |
|------|------|
| **M=I 禁止使用** | A1–A4 全部失败；位置独立 = 不安全的 MDS |
| **新 MDS 必须预检** | 实现策略前先用 `scripts/poseidon-tools/` 验证 MDS 通过 |
| **通过 MDS 清单** | Cauchy, Plonky3, companion, Fibonacci, Vandermonde, I+random-small |

### 规则 2: 不要用数值方法在 GF(p) 上寻根 🔴

Poseidon 输出在 GF(p) 上是高度非连续阶跃函数，有限差分 Jacobian 无意义。
Newton、固定点迭代、坐标下降、Z3 SMT 均不适用于此问题。

| 禁止 | 替代方案 |
|------|----------|
| Newton 迭代 / 固定点 / 坐标下降 | Gröbner basis (SageMath) |
| Z3 SMT over GF(p) | Resultant elimination (论文 2026/150) |

### 规则 3: 不要机械扫描 MDS 矩阵 🔴

没有数学理由地测试 20+ 个 MDS 变体不会带来突破。
每个 MDS 测试前必须在 Thinking Log 记录选择理由。最多测试 3 个 MDS。

### 规则 4: q=1 方法不适用于 q=4 🔴

Floyd rho / birthday 对 q=1 有效（退化为单元素检测），q=4 需要 4 个输出同时匹配。
必须使用代数攻击（Gröbner / resultant）。

### 规则 5: 纯 Python 无法做代数攻击 🔴

SymPy Gröbner 太慢，Z3 在 GF(p) 上编码 28 轮导致变量爆炸。
必须使用 CAS（SageMath / Magma / Maple）。

### 新策略检查清单

在进入 CODE 阶段前，必须在 Thinking Log 中确认：

```
□ 1. 目标 MDS 已通过 verify_mds_matrix? (不能是 M=I)
□ 2. 攻击方法是代数的 (Gröbner/resultant)，不是数值的 (Newton)?
□ 3. 选这个 MDS 有数学理由? (不是机械扫描)
□ 4. 方法可扩展到 q=4? (不是 q=1 特例)
□ 5. 有 CAS 工具支持? (SageMath/Magma，不是纯 Python)
```

## 3.1. Decision Tree

```
读取 §2 STATE + 检查变体预算
    │
    ├─ STATE="done" 或 变体预算超标？
    │       └─ 🛑 立即停止。提示用户自行运行代码。
    │
    ├─ 同一错误已出现 >3 次？
    │       └─ 🛑 停止。描述问题，让用户介入解决。
    │
    ├─ "running-vN"
    │       │
    │       ├─ attacks/vN/checkpoint.json 存在且 state="running"
    │       │       └─ 断点续传: 读 checkpoint，从上次位置继续
    │       │
    │       └─ checkpoint="done" 或无 checkpoint
    │               └─ 检查实验结果 → 找到碰撞？ → STATE="done" → 🛑
    │                   → 没找到，第 1 次尝试 → 允许微调 1 次
    │                   → 第 2 次仍失败 → STATE="give-up-vN"
    │
    ├─ "give-up-vN" 或 STATE 不存在
    │       │
    │       └─ 进入 THINKING 模式
    │               │
    │               ├─ ⚠️ 先检查 §3 规则 1: MDS 是否通过 verify_mds_matrix?
    │               │   → M=I 禁止！选通过验证的 MDS（Cauchy/companion/...）
    │               │
    │               ├─ 阅读 §8 Dead Ends，排除已失败方案
    │               ├─ 阅读 §9 Active Strategies，选择 1 个策略
    │               ├─ 在 §7 Thinking Log 中记录分析
    │               │   → 必须完成 §3 检查清单的 5 项
    │               ├─ 分析通过 → 实现，最多 2 个变体
    │               │   → 变体 1 失败 → 变体 2（修正后）失败 → 放弃
    │               ├─ 分析不通过 → 追加 Dead Ends，换策略
    │               └─ 策略耗尽 → STATE="done"，报告结论
    │
    └─ "thinking"
            └─ 同上，进入 THINKING 模式
```

**关键规则**:
- 每次切换状态时更新 §2 的 STATE 标记
- 找到碰撞 = 游戏结束。不要继续探索
- 不要覆盖旧版本代码

## 4. How to Run

```bash
cd ~/.aman/skills/prize-game/poseidon/scripts

# 基准测试
python3 run.py benchmark

# 运行某个版本的攻击
python3 attacks/v1/attack.py       # Floyd rho (已完成)
# python3 attacks/vN/attack.py     ← AI 创建新策略后运行
```

```python
# attacks/vN/attack.py 模板
import sys, os, time, json
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from framework.field import P, random_element, SEED
from framework.poseidon import make_default_poseidon, hash_with_seed, check_collision
from framework.mds import generate_mds_matrix

# ... 策略实现 ...
```

## 4.1. 代码修改规则

| 区域 | 规则 | 说明 |
|------|------|------|
| `framework/` | ❌ **禁止修改** | 哈希函数定义，改了就全错 |
| `attacks/brute.py` | ❌ **禁止修改** | 已出局，保留作参考 |
| `run.py` | ⚠️ 谨慎修改 | 可加新命令，不要删已有功能 |
| `attacks/vN/attack.py` | ✅ **自由修改** | 这是 AI 写策略代码的地方 |
| `SKILL.md` | ✅ **每轮更新** | 记录结果、更新策略、追加 Dead Ends |

**工作流 (Thinking-First)**:
```
                     ┌─ 可行 → attacks/vN/attack.py → 运行 → 记录结果
                     │
读 SKILL.md → 思考 → 假设 → 数学分析 → 判定
                     │
                     └─ 不可行 → 追加 Dead Ends，不写代码
```

**大部分工作应该是思考**，代码是最后一步。不要一上来就写代码——先做数学分析，
确认方向可行后再实现。分析过程记录在 §7 Thinking Log。

## 5. Mathematical Landscape

### Round Structure
```
Input: 15 elements + SEED=0xc09de4 → state[0..15]

Front: 4 full rounds   (S-Box x→x³ on ALL 16 → MDS)
Mid:   20 partial rounds (S-Box ONLY on s[0] → MDS)
Back:  4 full rounds   (S-Box on ALL 16 → MDS)

Output: permutation → +feedforward → first q elements
```

### Key Structural Weakness
**Partial rounds only apply S-Box to s[0]**. The other 15 state elements
undergo purely linear (MDS) transformations across all 20 partial rounds.
This dramatically limits the algebraic degree growth and is THE attack surface.

### Algebraic Degree Analysis
- Each full round: degree × 3 (cube on all 16) → after 4 front rounds: 3⁴ = 81
- Each partial round: degree grows only on s[0] path (×3), rest stays linear
- Total polynomial degree after all rounds is bounded

## 6. Strategy Lifecycle

每个策略必须经过以下阶段，**思考优先，代码最后**：

```
阶段 1: THINK    阅读数学背景，分析攻击面，提出假设
        ↓
阶段 2: ANALYZE  计算复杂度/可行性，记录到 Thinking Log
        ↓        如果明显不可行 → 追加 Dead Ends，跳过 CODE
        ↓
阶段 3: DESIGN   设计具体方案（变量选择、MDS 构造、消元顺序等）
        ↓
阶段 4: CODE     在 attacks/vN/attack.py 中实现
        ↓        📏 最多尝试 2 个变体
        ↓
阶段 5: TEST     语法检查、dry-run、验证代码可运行
        ↓        如果找到碰撞 → 🛑 立即停止，STATE="done"
        ↓
阶段 6: RECORD   更新 SKILL.md: 实验日志 + STATE="done" + 提示用户运行
        ↓
阶段 7: 🛑 STOP  代码可正常运行 → 停止。不要继续微调。
```

**关键原则**: 阶段 1-3 不写代码。在 Thinking Log 中完成数学分析，
确认方向可行后才进入代码阶段。**找到碰撞 = 游戏结束。**

## 7. 🧠 Thinking Log

**AI 在此记录数学分析和策略推演。不写代码，纯思考。**

| # | Date | Hypothesis | Quick Analysis | Verdict | → Action |
|---|------|-----------|----------------|---------|----------|
| 1 | — | "Cube root 是双射: gcd(3,p-1)=1" | p=2130706433, p%3=2, gcd(3,p-1)=1. 立方根指数存在，x→x³ 是排列！全轮可完全逆向。 | ✅ 已验证 | 利用此性质做后向攻击 |

*示例:*
- *Hypothesis: "选择对角 MDS 矩阵 → 部分轮退化为 16 条独立路径 → 每个 s[i] 可独立求解"*
- *Quick Analysis: "对角阵不满足 MDS 性质，验证器的 verify_mds_matrix 会拒绝"*
- *Verdict: ❌ Dead — 直接追加到 §8 Dead Ends，不写代码*

## 8. ❌ Dead Ends — DO NOT RETRY

These approaches are proven infeasible for q=4. Skip them.

| Approach | Why Dead | Proof |
|----------|----------|-------|
| Floyd rho (cycle detection) | 仅适用于 q=1，f 的输出是单个域元素，无法扩展 | q=1 已验证，q>1 不可行 |
| Birthday / random sampling | O(√(p^q)) — q=4 需要 ~4.5×10^18 次 ≈ 7400万年 | 基准测试已确认 |
| Meet-in-the-middle (naive) | O(p^(q/2)) = O(p^2) ≈ 4.5×10^18 — 同上 | 生日界限制 |
| 纯暴力枚举 | p^15 搜索空间，无意义 | 天文数字 |
| 固定部分输入 + 暴力剩余 | p^k 搜索代价，k 不够大 | 只是常数级加速 |
| Inverse Permutation Newton | 只优化 S[1..3], E0=SEED 约束无法满足 | 数值实验不收敛 |
| 纯 Python Gröbner/Z3 | SymPy 太慢，Z3 不适用于 GF(p) 上的多项式系统 | 需要 SageMath/Magma |
| **M=I identity MDS** | 🛑 `verify_mds_matrix(I)` = False（A1–A4 全部失败）。官方 `verify_collision_solution` 第 148 行拒绝。位置独立 = 不安全的 MDS — 永远不要使用 | 即使找到了数学上有效的碰撞，官方验证器也会拒绝 |

## 9. ✅ Active Strategies — TRY THESE

> **🛑 M=I (identity MDS) 已关闭。** 不要使用 M=I。`verify_mds_matrix(I)` = False，
> 官方验证器第 148 行在检查碰撞之前先拒绝。详见 §8 Dead Ends 和 §3 规则 1。
> 选择通过验证的 MDS（Cauchy / companion / Fibonacci / Vandermonde）。

### C: Gröbner Basis (SageMath) ← **首选**
- 构建多项式方程组 → 计算 lex Gröbner 基 → 提取单变量多项式 → 在 𝔽_p 求根
- **变体**: C1(完整30变量), C2(部分最后N轮), C3(变量排序), C4(Block ordering)
- 复杂度: 最坏双指数，但 α=3 低次 + 部分轮结构可能使 F4 可行

### D: Resultant Elimination (SageMath)
- 只对最后几轮建方程，用结式消去中间变量
- 比完整 Gröbner 快，适合小系统

### E: 选择弱 MDS 矩阵 🔥
- **MDS 可以任选！** 只需满足无不变子空间条件
- 寻找结构简单、代数度低、或有小不变子空间的 MDS
- 验证器接受任意 MDS（通过 `mds` 参数传入）
- 可能的方向: 低次多项式系数、稀疏矩阵、特殊结构
- **⚠️ 不要机械扫描** — 每次测试前必须有数学理由

### F: Subspace Trail
- 寻找 MDS 矩阵的不变子空间 L
- 如果状态在部分轮前落入 L → 部分轮退化为线性 → 线性方程组求解

### G: Differential / Linear Cryptanalysis
- 分析 S-Box (x→x³) 的差分/线性特性
- 寻找高概率差分路径

### H: Hybrid
- 代数方法固定部分变量 + 小规模暴力
- 例如: 用 Gröbner 确定 10 个变量关系，暴力搜剩余 5 个

### M: Resultant-Based Elimination 🔥🔥 — 论文 2026/150 (Bak et al.)
- **已成功破解 CICO-1/2 bounty（2025 年）**
- 构建多项式系统 → 用 resultant 消去变量 → 求解单变量多项式
- 对部分轮结构特别有效（只 s[0] 非线性）
- 需要 CAS (SageMath/Magma), 纯 Python 不可行

### N: Zero s[0] Partial Round Linearization 🔥🔥🔥
- **核心洞察**: 如果部分轮前 s[0]=0, 则 S-Box 恒等 (0³=0), 全部 20 个部分轮变为纯线性!
- 仅剩 8 个全轮的非线性 (RF=8), 碰撞搜索大幅简化
- 需要 20 个约束: ∀i ∈ [0..19]: s_before_partial_round_i[0] = 0
- 自由度: 15 个输入 X[0..14] + MDS 自由选择 → 可调参数充足
- **关键**: 选 MDS 使部分轮 s[0] 路径可控, 约束相关化

### O: 利用 q=3 已公开的攻击报告
- q=3 claim 于 2026-04-06, 攻击报告应在 2026-05-06 前公开
- 查找并复用 q=3 的攻击方法扩展到 q=4
- 搜索关键词: "Poseidon q=3 partial collision KoalaBear"

## 10. Experiment Protocol

### 每次实验

1. 在 `attacks/` 下新建 `vN/` 目录（`cp -r attacks/v$(($N-1)) attacks/v$N`，不修改旧代码）
2. 写 attack 脚本，导入 `framework/`
3. 运行、计时、记录结果

### 🔄 必须更新的文件

每次实验结束后，**必须用 Edit 工具更新以下文件**：

**SKILL.md**:
- 实验日志表 → 追加一行
- 发现死胡同 → 追加到 §8 Dead Ends
- 发现新策略 → 追加到 §9 Active Strategies
- 当前最佳 → 更新 §12 Current Target
- 有进展/洞察 → 更新实验日志的 Insight 列

**VERSIONS.md** (`scripts/VERSIONS.md`):
- 新建版本 → 在 Version Index 追加一行
- 版本状态变化（running → done / give-up）→ 更新该版本的 Status 列
- 发现碰撞 → 记录碰撞向量 (X, Y) 和 hash 值
- verify_mds_matrix 结果 → 更新该版本的验证状态表
- 每个版本必须包含：文件清单、策略描述、验证状态矩阵、运行命令

这是闭环：AI 读 SKILL.md → 选策略 → 运行 → 写回 SKILL.md + VERSIONS.md → 下次读最新版。

### 💾 断点续算

长时间计算必须支持中断后恢复。在每个 `attacks/vN/` 目录下维护 `checkpoint.json`:

```json
{
  "version": "v3",
  "strategy": "groebner-partial-6-rounds",
  "parameters": {"q": 4, "rp": 20, "mds": "weak-circulant", "variable_order": "lex"},
  "state": "running",
  "abandoned": false,
  "iteration": 0,
  "elapsed_seconds": 0,
  "last_saved": "2026-06-09T12:00:00Z"
}
```

`state` 取值: `"running"` | `"done"` | `"paused"`
`abandoned`: `true` 表示此策略已放弃，对应 §2 STATE=`give-up-vN`

attack 脚本在每次 checkpoint 时更新此文件。恢复时读取 `state`、`iteration` 等字段继续计算。
`run.py` 的 `--timeout` + `--checkpoint` 参数已支持此机制。

## 11. Experiment Log

| # | Date | Strategy | q | Time | Result | Insight |
|---|------|----------|---|------|--------|---------|
| 1 | — | Floyd rho | 1 | ~100s | ✅ | 官方验证器通过，框架验证完成 |

## 12. Current Target

- **目标**: q=4 ($64,000)
- **Deadline**: Jan 1, 2029
- **生日界**: ~4.5×10^18 — 暴力出局
- **最大自由度**: MDS 矩阵可以任选 ← 🔥 突破口
- **策略**: 选择一个弱 MDS + Gröbner basis / subspace trail
- **关键参数**: 20 个部分轮中只有 s[0] 非线性 → 代数度增长缓慢
- **最新文献**: 
  - 2026/150: resultant 攻击成功破解 CICO bounty
  - 2026/306: Skipping Class, 弱矩阵跳过轮, 碰撞攻击 2^106 加速
  - 2025/954: Gröbner basis + subspace trail

## 13. Code Map

```
~/.aman/skills/prize-game/poseidon/scripts/
├── run.py                  ← CLI (benchmark)
├── VERSIONS.md             ← 版本注册表（每个版本的验证状态）
├── framework/              ← ❌ 禁止修改
│   ├── field.py            ← 𝔽_p 运算
│   ├── grain_lfsr.py       ← Grain LFSR 轮常数
│   ├── mds.py              ← Cauchy / Plonky3 MDS
│   └── poseidon.py         ← Poseidon 哈希 + 碰撞检测
│
├── poseidon-tools/         ← 官方验证器 (vendored)
│   ├── poseidon/mds_matrix.py    ← verify_mds_matrix()
│   └── bounties/partial_collision_verifier.py ← verify_collision_solution()
│
├── attacks/
│   ├── brute.py            ← ❌ 已出局，保留参考
│   ├── v1/                 ← ✅ Floyd rho (已完成)
│   │   ├── attack.py
│   │   └── checkpoint.json
│   └── vN/                 ← ✅ AI 创建 (N=2,3,...)
│       ├── attack.py        ← 策略实现 (AI 写)
│       ├── checkpoint.json  ← 断点续算
│       └── notes.md         ← (可选) 策略思路
```

**AI 写代码的范围**: 在 `attacks/vN/` 下创建 attack.py。从 `framework/` 导入原语，
实现 SKILL.md 中选定的策略。不要改 `framework/` 或 `attacks/brute.py`。

## 14. References

- Challenge spec: `docs/game/Poseidon/Poseidon.md`
- Official tools: https://github.com/khovratovich/poseidon-tools (vendored at `scripts/poseidon-tools/`)
- Poseidon paper: https://eprint.iacr.org/2019/458
- Gröbner basis: https://doc.sagemath.org/html/en/reference/polynomial_rings/
