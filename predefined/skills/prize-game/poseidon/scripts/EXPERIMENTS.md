# Poseidon — Master Experiment Log

All experiment runs across all versions. Append-only — never delete rows.

## Experiment Table

| # | Date | Version | Strategy | q | Iterations | Runtime | Hashes/s | Result | Insight |
|---|------|---------|----------|---|------------|---------|----------|--------|---------|
| 1 | 2026-06-09 | v1 | Floyd rho | 1 | 191,046 | 100s | 1,903 | ✅ q=1 | Official verifier passed. Framework verified. |

## Quick Stats

- **Total experiments**: 1
- **Total hashes computed**: 191,046
- **Total runtime**: 100s
- **Collisions found**: 1 (q=1)
- **Best collision**: tail_pred=403130345, cycle_pred=696607256, H[0]=74288258
- **Official verifier**: PASSED ✓

## Strategy Evolution

```
v1 (Floyd rho q=1) ✅
  │
  └── q=1 验证框架正确。q=2,3 已被他人领取。
      目标 q=4 ($64K)。暴力 7400 万年 — 必须用代数攻击。
```

## Key Insights

1. **Hash speed**: ~1,900 hashes/sec (Python, Cauchy MDS)
2. **Framework verified**: 100 random cross-checks match official poseidon-tools
3. **q=1 solved**: Floyd rho, 191K calls, 100s
4. **q=2,3 claimed by others** → target is q=4 ($64K)
5. **Brute force dead**: sqrt(p^4) ≈ 4.5×10^18 ≈ 7400万年
6. **MDS freedom**: MDS 矩阵可任选 ← 🔥 最大突破口
7. **Next**: algebraic attacks — Gröbner basis / subspace trail with chosen weak MDS
