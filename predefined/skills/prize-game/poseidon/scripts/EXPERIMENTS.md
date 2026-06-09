# Poseidon — Master Experiment Log

All experiment runs across all versions. Append-only — never delete rows.

## Experiment Table

| # | Date | Version | Strategy | RP | q | Iterations | Runtime | Hashes/s | Result | Best Match | Insights |
|---|------|---------|----------|----|---|------------|---------|----------|--------|------------|----------|
| 1 | 2026-06-09 | v1 | Floyd rho | 20 | 1 | 191,046 | 100.4s | 1,903 | **COLLISION FOUND** | 1/1 | Floyd rho q=1 works, ~2·sqrt(p) calls. Verified with official verifier. Next: try q=2. |

## Quick Stats

- **Total experiments**: 1
- **Total hashes computed**: 191,046
- **Total runtime**: 100.4s
- **Collisions found**: 1 (q=1, t=1)
- **Best collision**: tail_pred=403130345, cycle_pred=696607256, H[0]=74288258
- **Official verifier**: PASSED ✓

## Strategy Evolution

```
v1 (Floyd rho q=1) ──► v2 (Floyd rho q=2 or algebraic attack)
  │
  └── q=1 collision found (100s, 191K calls). Birthday bound for q=2
      is sqrt(p^2) ≈ p ≈ 2.13e9 samples (infeasible). Need algebraic approach.
```

## Key Insights Learned

1. **Hash speed**: ~1,900 hashes/sec (Python, single-threaded, Cauchy MDS)
2. **q=1 collision**: Floyd rho succeeds in ~100s with ~191K hash calls
3. **Official verifier match**: Framework produces identical output to poseidon-tools
4. **Birthday bound check**: For q=2, birthday bound is sqrt(p^2) ≈ 2.13e9 (~13 days continuous)
5. **Next target**: Algebraic attacks for q≥2 — Gröbner basis with SageMath
