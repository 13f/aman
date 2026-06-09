# Poseidon Attack Versions — Registry

Each version is a self-contained attack attempt. Versions are **immutable**
once created — new attempts fork from the best version into a new directory.
The framework (`framework/`) is stable and shared across all versions.

## Version Index

| Version | Date | Strategy | RP | q | Result | Best Match | Forked From |
|---------|------|----------|----|---|--------|------------|-------------|
| [v1](v1/) | — | Birthday (random sampling) | 8 | 4 | Not run yet | — | (initial) |

## How to Create a New Version

1. Copy the best version directory:
   ```bash
   cp -r versions/vN versions/vN+1
   ```
2. Update `versions/vN+1/__init__.py` with the new strategy description.
3. Modify `versions/vN+1/attack.py` with the new algorithm.
4. Update `versions/vN+1/SKILL.md` with the strategy documentation.
5. Reset `results.json` to empty template.
6. Add a row to the version index above.
7. **Never modify old versions** — only append.

## Quick Links

- [Master Experiment Log](EXPERIMENTS.md) — all runs across all versions
- [Framework](../framework/) — stable mathematical primitives
