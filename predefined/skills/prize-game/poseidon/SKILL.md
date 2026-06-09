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

**🔥 MDS 矩阵可以任选！** 只需满足 "无不变子空间轨迹" 条件。
Plonky3 循环矩阵只是示例。这意味着可以**选择一个结构更弱、更容易攻击的 MDS**——
这是整个挑战中最大的自由度，可能是突破口。

## 2. Current State — READ THIS FIRST

<!--
  STATE 取值:
    "thinking"     → 需要构思新策略，进入 §7 Thinking Log
    "running-vN"   → 检查 attacks/vN/checkpoint.json，尝试断点续传
    "give-up-vN"   → 放弃 vN，进入 thinking 模式
-->

**STATE: `thinking`**

> v1 (Floyd rho q=1) 已完成验证。q≥4 暴力不可行，需要代数攻击。
> 进入深度思考模式。关键发现: cube root 在 𝔽_p 上是双射 (gcd(3,p-1)=1)，
> 可以利用此性质逆向全轮。正在构思后向-前向结合 Newton 迭代法。

## 3. Decision Tree

```
读取 §2 STATE
    │
    ├─ "running-vN"
    │       │
    │       ├─ attacks/vN/checkpoint.json 存在且 state="running"
    │       │       └─ 断点续传: 读 checkpoint，从上次位置继续
    │       │
    │       └─ checkpoint 不存在或 state="done"
    │               └─ 检查实验结果 → 更新 STATE
    │
    ├─ "give-up-vN" 或 STATE 不存在
    │       │
    │       └─ 进入 THINKING 模式
    │               │
    │               ├─ 阅读 §8 Dead Ends，排除已失败方案
    │               ├─ 阅读 §9 Active Strategies，选择/构思策略
    │               ├─ 在 §7 Thinking Log 中记录分析
    │               ├─ 如果新策略可行 → STATE="running-vN+1", 实现 vN+1/attack.py
    │               └─ 如果新策略也不可行 → 追加 Dead Ends, STATE 保持 "give-up-..."
    │
    └─ "thinking"
            └─ 同上，进入 THINKING 模式
```

**关键**: 每次切换状态时更新 §2 的 STATE 标记。不要覆盖旧版本代码。

## 4. How to Run

```bash
cd ~/.aman/skills/prize-game/poseidon/scripts

# 基准测试
python3 run.py benchmark

# 运行某个版本的攻击
python3 attacks/v1/attack.py       # Floyd rho (已完成)
python3 attacks/v2/attack.py       # 新策略
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
阶段 2: ANALYZE  粗略计算复杂度/可行性，在 Thinking Log 中记录分析
        ↓        如果明显不可行 → 直接追加 Dead Ends，跳过后面的阶段
        ↓
阶段 3: DESIGN   设计具体方案（变量选择、MDS 构造、消元顺序等）
        ↓
阶段 4: CODE     在 attacks/vN/attack.py 中实现（仅当阶段 2-3 认为可行时）
        ↓
阶段 5: TEST     运行、计时、验证
        ↓
阶段 6: RECORD   更新 SKILL.md: 实验日志 + 结果 + (如果失败) Dead Ends
```

**关键原则**: 阶段 1-3 不写代码。在 Thinking Log 中完成数学分析，
确认方向可行后才进入代码阶段。这样可以快速排除大量死胡同。

## 7. 🧠 Thinking Log

**AI 在此记录数学分析和策略推演。不写代码，纯思考。**

| # | Date | Hypothesis | Quick Analysis | Verdict | → Action |
|---|------|-----------|----------------|---------|----------|
| 1 | 2026-06-09 | "Cube root 是双射: gcd(3,p-1)=1" | p=2130706433, p%3=2, gcd(3,p-1)=1. 立方根指数 e=(2p-1)/3=1420470955. x→x³ 是排列！全轮可完全逆向。 | ✅ 已验证 | 利用此性质做后向攻击 |
| 2 | 2026-06-09 | "Newton 迭代法求解 4×4 方程组" | 固定 Y=0, 固定 X[4..14]=0, 4变量 X[0..3]. 误差函数 E(X)=H(X)[:4]-T. Jacobian 4×4 有限差分. 每步~5次hash. 收敛性不确定但代价低。 | ⏳ **选定方案** | 实现 v2/attack.py: Newton 迭代

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

## 9. ✅ Active Strategies — TRY THESE

### C: Gröbner Basis (SageMath) ← **首选**
- 构建多项式方程组 → 计算 lex Gröbner 基 → 提取单变量多项式 → 在 𝔽_p 求根
- **变体**:
  - C1: 完整系统（30 个变量） → 最通用，最贵
  - C2: 部分系统（只展开最后 N 轮） → 变量少，可能找到 q=4
  - C3: 不同变量排序 → 极大影响运行时间
  - C4: Block ordering (degrevlex → lex) → 更快消元
- 复杂度: 最坏双指数，但 α=3 低次 + 部分轮结构可能使 F4 可行

### D: Resultant Elimination (SageMath)
- 只对最后几轮建方程，用结式消去中间变量
- 比完整 Gröbner 快，适合小系统

### E: 选择弱 MDS 矩阵 🔥
- **MDS 可以任选！** 只需满足无不变子空间条件
- 寻找结构简单、代数度低、或有小不变子空间的 MDS
- 验证器接受任意 MDS（通过 `mds` 参数传入）
- 可能的方向: 低次多项式系数、稀疏矩阵、特殊结构

### F: Subspace Trail
- 寻找 MDS 矩阵的不变子空间 L
- 如果状态在部分轮前落入 L → 部分轮退化为线性 → 线性方程组求解

### G: Differential / Linear Cryptanalysis
- 分析 S-Box (x→x³) 的差分/线性特性
- 寻找高概率差分路径

### H: Hybrid
- 代数方法固定部分变量 + 小规模暴力
- 例如: 用 Gröbner 确定 10 个变量关系，暴力搜剩余 5 个

### I: Agent-Invented — Backward-Forward Newton with Cube Root Bijection 🔥
- **关键洞察**: gcd(3,p-1)=1 → x→x³ 在 𝔽_p 上是双射 → 全轮可完全逆向
- 使用 Newton 迭代法: 固定11个输入变量，对剩余4个变量做数值寻根
- 误差函数 E(X[0..3]) = H([X₀,X₁,X₂,X₃,0,...,0])[:4] - T
- Jacobian 通过有限差分计算（每步5次hash）
- 随机重启处理不收敛情况
- **现代码实现**: attacks/v2/attack.py

## 10. Experiment Protocol

### 每次实验

1. 在 `attacks/` 下新建 `vN/` 目录（`cp -r attacks/v$(($N-1)) attacks/v$N`，不修改旧代码）
2. 写 attack 脚本，导入 `framework/`
3. 运行、计时、记录结果

### 🔄 必须更新 SKILL.md

每次实验结束后，**必须用 Edit 工具更新本文件**：
- 实验日志表 → 追加一行
- 发现死胡同 → 追加到 §8 Dead Ends
- 发现新策略 → 追加到 §9 Active Strategies
- 当前最佳 → 更新 §12 Current Target
- 有进展/洞察 → 更新实验日志的 Insight 列

这是闭环：AI 读 SKILL.md → 选策略 → 运行 → 写回 SKILL.md → 下次读最新版。

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
| 1 | 2026-06-09 | Floyd rho | 1 | 100s | ✅ | 官方验证器通过，框架验证完成 |

## 12. Current Target

- **目标**: q=4 ($64,000)
- **Deadline**: Jan 1, 2029（还有约 2.5 年）
- **生日界**: ~4.5×10^18 — 暴力出局
- **最大自由度**: MDS 矩阵可以任选 ← 🔥 突破口
- **策略**: 选择一个弱 MDS + Gröbner basis / subspace trail
- **关键参数**: 20 个部分轮中只有 s[0] 非线性 → 代数度增长缓慢

## 13. Code Map

```
~/.aman/skills/prize-game/poseidon/scripts/
├── run.py                  ← CLI (benchmark)
├── framework/              ← ❌ 禁止修改
│   ├── field.py            ← 𝔽_p 运算
│   ├── grain_lfsr.py       ← Grain LFSR 轮常数
│   ├── mds.py              ← Cauchy / Plonky3 MDS
│   └── poseidon.py         ← Poseidon 哈希 + 碰撞检测
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
- Official tools: https://github.com/khovratovich/poseidon-tools
- Poseidon paper: https://eprint.iacr.org/2019/458
- Gröbner basis: https://doc.sagemath.org/html/en/reference/polynomial_rings/
