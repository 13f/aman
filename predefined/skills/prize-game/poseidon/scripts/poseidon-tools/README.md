# Poseidon Tools (vendored)

- **Original repository**: https://github.com/khovratovich/poseidon-tools
- **Download date**: 2026-06-10
- **Branch**: main
- **Download method**: direct file extraction (not git clone)

## Why vendored

The official `verify_mds_matrix()` from this repository is needed by the
prize-game attack scripts (v4/test_mds.py, etc.) to check whether a
custom MDS matrix passes the bounty challenge's validity criteria.

Vendoring avoids a runtime dependency on `/tmp/poseidon-tools` and
ensures reproducibility.
