https://www.poseidon-initiative.info/ 

---

# Poseidon1 碰撞挑战代数攻击设计文档

## 1. 背景与挑战目标

- **哈希函数**：Poseidon1（压缩模式），基于替换-置换网络（SPN）。
- **有限域**：KoalaBear 素数  
  \[
  p = 2^{31} - 2^{24} + 1 = 2130706433
  \]
- **状态宽度**：\( t = 16 \) 个域元素。
- **S-Box**：\( x \mapsto x^3 \)（指数 \( \alpha = 3 \)）。
- **轮数配置**：
  - 全轮（RF）= 6（前 RF/2 = 3 全轮，后 RF/2 = 3 全轮）
  - 部分轮（RP）= 6, 8, 10 …（随挑战难度增加）
- **MDS 矩阵**：任意满足“无不变子空间轨迹”条件的矩阵，例如 Plonky3 循环矩阵。
- **挑战目标（碰撞奖）**：  
  找到两个 15 元素输入 \( X, Y \)（压缩模式，初始固定前缀 `0xc09de4` 作为前一个状态），使哈希输出的前 \( q \) 个元素完全相等（\( q = 3,4,5,6,7 \)）。

## 2. 核心数学基础

### 2.1 有限域 \( \mathbb{F}_p \)

- 域元素为整数 \( \{0,1,\dots,p-1\} \)。
- 加减乘除模 \( p \) 运算。
- 除法通过乘法逆元实现（费马小定理 \( a^{p-2} \)）。

### 2.2 多项式环

设 \( \mathbb{F}_p[x_1, x_2, \dots, x_n] \) 为 \( n \) 个变量的多项式环。  
一个多项式是有限个单项式的和：
\[
\sum_{e_1,\dots,e_n} c_{e_1\dots e_n} \cdot x_1^{e_1} \cdots x_n^{e_n},\quad c \in \mathbb{F}_p
\]
- **次数**：单项式中变量指数之和的最大值。
- **多元多项式方程组**：一系列多项式 \( f_1 = 0, \dots, f_m = 0 \) 定义了代数簇。

### 2.3 结式 (Resultant)

对于两个单变量多项式 \( A(x), B(x) \)，结式 \( \mathrm{Res}(A,B) \) 是关于系数的多项式，为零当且仅当 \( A \) 与 \( B \) 有公共根。  
在多元情形下，结式可用于**消去一个变量**：例如从 \( f(x,y)=0, g(x,y)=0 \) 消去 \( y \) 得到 \( R(x)=0 \)。

### 2.4 Gröbner 基

给定理想 \( I = \langle f_1,\dots,f_m \rangle \)，其 Gröbner 基是一组生成元，使得理想成员判定和方程组求解更容易。  
- **词典序 (lex)**：优先消去靠前的变量。
- **算法**：Buchberger、F4、F5（SageMath 内置实现）。
- 求解步骤：计算 Gröbner 基 → 得到三角化系统 → 回代求数值解。

### 2.5 子空间踪迹 (Subspace Trail)

定义：一个线性子空间 \( L \subset \mathbb{F}_p^t \) 在 MDS 矩阵 \( M \) 作用下**前向不变**（或经过几轮后仍落在某个子空间）。  
对于部分轮仅对第一个元素应用 S-Box 的情况，如果整个状态始终位于某个子空间中，则该子空间内 S-Box 可被线性化或简化，从而降低代数次数。

## 3. Poseidon1 详细代数描述

### 3.1 状态表示

设状态向量 \( \mathbf{s} = (s_0, s_1, \dots, s_{t-1}) \in \mathbb{F}_p^t \)。

### 3.2 单个全轮操作

1. **S-Box 层**：  
   \[
   s_i \leftarrow s_i^{\alpha},\quad \alpha = 3
   \]
   对**所有** \( i \) 同时作用。

2. **MDS 矩阵乘法**：  
   \[
   \mathbf{s} \leftarrow M \cdot \mathbf{s}
   \]
   其中 \( M \) 是 \( t \times t \) 可逆矩阵，满足 MDS 性质（任意子矩阵满秩）。

### 3.3 部分轮操作

部分轮中，S-Box **仅应用于第一个元素** \( s_0 \)：
\[
s_0 \leftarrow s_0^{\alpha}, \quad s_i \leftarrow s_i \ (i \ge 1)
\]
线性层仍是全矩阵乘法 \( \mathbf{s} \leftarrow M \cdot \mathbf{s} \)。

### 3.4 完整轮数配置

```
// 初始状态 s[0] = 0xc09de4（前缀），其余 15 个为输入 X
for i in 1..(RF/2):        // 前 RF/2 全轮
    full_round(s)
for i in 1..RP:            // 部分轮
    partial_round(s)
for i in 1..(RF/2):        // 后 RF/2 全轮
    full_round(s)
// 输出 s[0..q-1] 用于碰撞比较
```

其中 `RF = 6`，`RP` 可变（6,8,10…）。

### 3.5 参数对攻击难度的影响

| 参数 | 作用 | 攻击挑战 |
| :--- | :--- | :--- |
| \( t = 16 \) | 状态宽度 | 更多变量，约束系统规模大 |
| \( \alpha = 3 \) | 低次 S-Box | 代数攻击有利（次数≤3） |
| \( RF = 6 \) | 少量全轮 | 可完全展开多项式 |
| \( RP \) 增加 | 更多部分轮 | 增加总代数次数，复杂性指数增长 |
| MDS 无不变子空间 | 消除特殊弱矩阵 | 但可能存在**有限长度**的子空间踪迹 |

## 4. 攻击建模总体策略

我们的目标是找到两个不同的输入 \( X \) 和 \( Y \)（均为 \( t-1 = 15 \) 个域元素）使得哈希输出的前 \( q \) 项相等。

**建模为方程组**：

设 \( F \) 表示完整的 Poseidon1 压缩函数（含固定前缀 `0xc09de4`）。  
则要求：
\[
F(X)_j = F(Y)_j, \quad j = 0,1,\dots,q-1
\]
这是一个**相等约束**系统。

为了方便代数处理，我们可以引入**中间状态变量**，将 \( F \) 的整个计算过程展开为一系列多项式方程。

### 4.1 变量定义

- 输入变量：\( x_0, x_1, \dots, x_{14} \)（对应 \( X \)），\( y_0, \dots, y_{14} \)（对应 \( Y \)）。
- 中间状态变量：对每一轮、每一元素，定义符号变量。例如：
  - \( a^{(r)}_i \) 表示第 \( r \) 轮 S-Box 前的状态。
  - \( b^{(r)}_i \) 表示第 \( r \) 轮线性层后的状态。
  但为减少变量，可以只保留每轮输入 / 输出关系。

### 4.2 约束方程类型

1. **S-Box 约束**：
   \[
   b = a^{\alpha} \quad\Rightarrow\quad b - a^{\alpha} = 0
   \]
   或者用变量表示 \( a \) 和 \( b \) 的关系。

2. **线性层约束**：
   \[
   a^{(r+1)}_i = \sum_{j=0}^{t-1} M_{ij} \cdot b^{(r)}_j
   \]
   其中 \( b^{(r)} \) 是第 \( r \) 轮 S-Box 输出（或部分轮中只对 \( s_0 \) 应用）。

3. **碰撞约束**：
   \[
   \text{out}_i(X) - \text{out}_i(Y) = 0,\quad i=0,\dots,q-1
   \]

### 4.3 整体多项式系统

设总变量数为 \( N \)（输入变量 + 中间变量），总方程数为 \( M \)。  
我们得到一个多项式方程组：
\[
\begin{cases}
f_1(v_1,\dots,v_N) = 0 \\
\vdots \\
f_M(v_1,\dots,v_N) = 0
\end{cases}
\]
目标是找到 \( \mathbb{F}_p \) 上的一个解（或全部解）。

## 5. 攻击路线：代数消元与求解

### 5.1 路线一：结式攻击（适用于较小轮数）

适用于 \( RP \) 较小（如 6）的情况。思路是手工或自动消去大部分中间变量，最终推导出关于输入变量的低次方程。

**步骤**：
1. 将 Poseidon 最后几轮（如最后 3 轮）用符号变量表示。
2. 利用碰撞条件 \( \text{out}_i(X) = \text{out}_i(Y) \) 构建方程。
3. 从后向前代入 MDS 和 S-Box 关系，逐步消去状态变量。
4. 最终获得关于 \( X \) 和 \( Y \) 的少量多项式方程。
5. 计算结式消去 \( Y \)，得到单变量高次方程。
6. 解方程得到候选 \( X \)，再回代求 \( Y \)。

**适用性**：对 \( q \) 较小（3,4）且 \( RP \) 固定时可手动推导。自动化需要符号计算软件（SageMath）。

### 5.2 路线二：Gröbner 基攻击（通用方法）

这是目前最系统的代数攻击工具，适用于任意 \( RP, q \)。

**步骤**：
1. **系统构建**：用 Python 脚本（调用 SageMath）自动为给定的 \( RF, RP, q \) 生成多项式方程组。包括所有轮运算和碰撞条件。
2. **变量排序**：选择词典序（lex）使输出变量优先，或使输入变量最后消去。
3. **计算 Gröbner 基**：
   ```sage
   I = ideal(equations)
   G = I.groebner_basis()
   ```
4. **分析基**：
   - 如果基中包含单变量多项式 \( h(x_i) \)，求解该多项式在 \( \mathbb{F}_p \) 中的根。
   - 依次代入得到其他变量。
5. **验证解**：用 Python 的 poseidon-tools 验证找到的 \( X, Y \) 是否满足碰撞。

**性能考虑**：
- 变量数和方程数随轮数指数增长，对 \( RP \ge 8 \) 时直接计算可能不可行。
- 需要利用**子空间踪迹**或**固定某些输入模式**来降低复杂度（见下文）。

### 5.3 路线三：子空间踪迹攻击（高级）

利用 Poseidon 部分轮的弱点：如果前 \( r \) 轮全轮后的状态落入某个子空间，则后续部分轮的 S-Box 仅作用于一个元素，若该元素在子空间中线性化，则整个部分轮段可变成**线性映射**。

**攻击流程**：
1. 寻找一个子空间 \( L \) 使得 \( M \cdot L \subset L \) 且 \( L \) 包含向量 \( (1,0,\dots,0) \) 方向的某个倍数。
2. 构造特殊输入，使哈希在进入部分轮之前的状态位于 \( L \) 中。
3. 在 \( L \) 中，部分轮操作退化为线性变换，从而可快速传播碰撞。
4. 利用该线性性，将碰撞条件转化为求解线性方程组。

**当前状态**：已知 Poseidon1 的 MDS 矩阵（如 Plonky3 循环矩阵）不存在**无限**子空间踪迹，但可能具有**有限长度**踪迹（例如经过 3 轮后离开）。攻击者需精确控制长度。

## 6. 实现框架（Python + SageMath）

尽管不具体编码，这里给出一个可实现的模块结构。

### 6.1 模块划分

```
poseidon_attack/
├── config.py            # 参数 p, t, alpha, RF, RP, q
├── poseidon_model.py    # 生成多项式方程组的函数
├── groebner_attack.py   # 调用Sage计算Gröbner基
├── resultant_attack.py  # 手工结式消元（可借助Sage符号运算）
├── subspace_attack.py   # 子空间构造与线性化
└── verify.py            # 用poseidon-tools验证解
```

### 6.2 关键函数伪代码（描述）

```python
# config.py
p = 2130706433
t = 16
alpha = 3
RF = 6
RP = 8   # 待破解目标
q = 4    # 碰撞长度

# poseidon_model.py (Sage)
def build_full_round(s_vars, M):
    """输入s_vars列表，返回经过一个全轮后的变量列表"""
    # sbox
    s_cubed = [v^alpha for v in s_vars]
    # linear
    new = [sum(M[i][j]*s_cubed[j] for j in range(t)) for i in range(t)]
    return new

def build_partial_round(s_vars, M):
    """部分轮：仅对第一个变量立方"""
    s_cubed = [s_vars[0]^alpha] + s_vars[1:]
    new = [sum(M[i][j]*s_cubed[j] for j in range(t)) for i in range(t)]
    return new

def generate_collision_equations(X_list, Y_list):
    """
    X_list, Y_list: 长度为t-1的符号变量列表
    返回方程组（列表）和所有中间变量
    """
    # 初始状态带前缀 0xc09de4
    prefix = GF(p)(0xc09de4)
    sX = [prefix] + X_list
    sY = [prefix] + Y_list
    
    # 前RF/2全轮
    for _ in range(RF//2):
        sX = build_full_round(sX, M)
        sY = build_full_round(sY, M)
    # RP部分轮
    for _ in range(RP):
        sX = build_partial_round(sX, M)
        sY = build_partial_round(sY, M)
    # 后RF/2全轮
    for _ in range(RF//2):
        sX = build_full_round(sX, M)
        sY = build_full_round(sY, M)
    
    # 碰撞条件：前q个输出相等
    eqs = [sX[i] - sY[i] for i in range(q)]
    return eqs, [sX, sY]  # 加上中间变量可返回全部用于调试
```

### 6.3 使用 SageMath 求解

```python
# groebner_attack.py (Sage)
from sage.all import *
from poseidon_model import generate_collision_equations

X = [var(f'x{i}') for i in range(15)]
Y = [var(f'y{i}') for i in range(15)]
eqs, _ = generate_collision_equations(X, Y)

I = ideal(eqs)
G = I.groebner_basis()
print("Gröbner basis length:", len(G))

# 提取单变量多项式
for g in G:
    if g.is_univariate():
        print("Univariate:", g)
        roots = g.roots(multiplicities=False)
        print("Roots:", roots)
```

### 6.4 验证

使用官方 `poseidon-tools` 中的 `partial_collision_verifier.py` 验证输出结果。确保你的解满足：
- 输入为 15 个整数 ∈ [0, p-1]
- 压缩函数输出前 q 个相等。

## 7. 预期难点与优化方向

| 难点 | 可能的解决方案 |
| :--- | :--- |
| 变量数爆炸（~ \( O(RP \cdot t) \)） | 利用**子空间踪迹**减少中间变量；采用**局部消元**（如仅展开最后几轮）。 |
| Gröbner 基计算过慢 | 使用 F4/F5 算法（Sage 调用 `groebner_basis(algorithm='f4')`）；尝试不同变量序。 |
| 无单变量多项式出现 | 说明解空间高维，需要添加更多约束（如固定部分输入为常数）。 |
| 需要求解高次单变量方程（次数>50） | 在有限域上可使用 Cantor-Zassenhaus 算法（Sage 内置 `roots()` 支持）。 |

## 8. 总结

这份文档提供了从数学基础到攻击建模的完整理论框架。实现时建议：

1. 从最小的 RP=6、q=3 开始尝试，验证你的代数系统构建正确。
2. 逐步增加 RP 和 q，观察 Gröbner 基计算的可行性极限。
3. 若直接计算失败，尝试**混合攻击**：先暴力枚举输入的前几个字节或利用子空间约束简化系统。

通过不断迭代，你将能开发出针对 Poseidon1 碰撞挑战的有效攻击程序。祝成功！