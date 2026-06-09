#!/usr/bin/env python3
"""
v1 — Floyd rho q=1 (COMPLETED ✅)

Template for AI to fork into v2, v3, ... when trying new strategies.
Each version is self-contained: imports framework, implements strategy,
saves checkpoints for resume.

Usage:
    python3 attacks/v1/attack.py               # Run
    python3 attacks/v1/attack.py --resume      # Resume from checkpoint
"""

import sys
import os
import time
import json
import signal
import argparse

# ── Setup: make framework importable ──────────────────────────────────
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
ROOT_DIR = os.path.dirname(os.path.dirname(SCRIPT_DIR))  # scripts/ (up from attacks/v1/)
sys.path.insert(0, ROOT_DIR)

from framework.field import P, INPUT_WIDTH, random_element
from framework.poseidon import (
    make_default_poseidon, hash_with_seed, check_collision
)

# ── Checkpoint ────────────────────────────────────────────────────────
CHECKPOINT_FILE = os.path.join(SCRIPT_DIR, "checkpoint.json")

def load_checkpoint():
    if os.path.exists(CHECKPOINT_FILE):
        with open(CHECKPOINT_FILE) as f:
            return json.load(f)
    return {}

def save_checkpoint(state: dict):
    state["last_saved"] = time.strftime("%Y-%m-%dT%H:%M:%S")
    with open(CHECKPOINT_FILE, "w") as f:
        json.dump(state, f, indent=2)

# ── Strategy implementation ───────────────────────────────────────────
# Replace this function when forking to v2, v3, ...

def run_strategy(args) -> dict:
    """Floyd rho for q=1. Returns result dict."""
    import random as _random
    import math

    pos = make_default_poseidon()
    rng = _random.Random(args.seed)

    def f(v: int) -> int:
        inp = [v] + [0] * (INPUT_WIDTH - 1)
        return hash_with_seed(inp, pos, out_length=1)[0]

    expected = int(3 * math.isqrt(P))
    print(f"v1 — Floyd rho q=1 | expected ~{expected:,} calls | ~{expected/1900:.0f}s")
    print()

    # Resume from checkpoint if requested
    ck = load_checkpoint()
    hash_calls = ck.get("iteration", 0) if args.resume else 0
    start_time = time.time() - ck.get("elapsed_seconds", 0) if args.resume else time.time()

    for restart in range(1, 1001):
        v0 = rng.randint(0, P - 1)

        tortoise = f(v0); hash_calls += 1
        hare = f(tortoise); hash_calls += 1

        while tortoise != hare:
            tortoise = f(tortoise); hash_calls += 1
            hare = f(f(hare)); hash_calls += 2

        tortoise2 = v0; mu = 0
        while tortoise2 != hare:
            tortoise2 = f(tortoise2); hash_calls += 1
            hare = f(hare); hash_calls += 1
            mu += 1

        if mu == 0:
            print(f"  restart {restart}: mu=0, retrying...")
            continue

        tail_pred = v0
        for _ in range(mu - 1):
            tail_pred = f(tail_pred); hash_calls += 1

        x_mu = tortoise2; cycle_pred = x_mu
        while True:
            nxt = f(cycle_pred); hash_calls += 1
            if nxt == x_mu: break
            cycle_pred = nxt

        if tail_pred == cycle_pred:
            continue

        x = [tail_pred] + [0] * (INPUT_WIDTH - 1)
        y = [cycle_pred] + [0] * (INPUT_WIDTH - 1)

        if not check_collision(x, y, 1, pos):
            continue

        elapsed = time.time() - start_time
        result = {
            "version": "v1", "strategy": "floyd_rho", "q": 1,
            "found": True, "hash_calls": hash_calls,
            "elapsed_seconds": elapsed,
            "collision": {"x": x, "y": y, "hash_output": hash_with_seed(x, pos, 1)[0]},
        }
        save_checkpoint({"state": "done", "iteration": hash_calls, "elapsed_seconds": elapsed})
        return result

    elapsed = time.time() - start_time
    return {"version": "v1", "found": False, "hash_calls": hash_calls, "elapsed_seconds": elapsed}


# ── Main ──────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="v1 — Floyd rho q=1")
    parser.add_argument("--seed", type=int, default=None)
    parser.add_argument("--resume", action="store_true")
    args = parser.parse_args()

    result = run_strategy(args)

    if result.get("found"):
        c = result["collision"]
        print(f"\n✅ q=1 collision: x[0]={c['x'][0]}, y[0]={c['y'][0]}, H[0]={c['hash_output']}")
    else:
        print(f"\n⏹ No collision: {result['hash_calls']:,} calls, {result['elapsed_seconds']:.0f}s")

    # Save result to scripts/results.json
    results_path = os.path.join(ROOT_DIR, "results.json")
    prev = {}
    if os.path.exists(results_path):
        with open(results_path) as f:
            try: prev = json.load(f)
            except: pass
    prev.setdefault("history", []).append(result)
    prev["last_run"] = result
    with open(results_path, "w") as f:
        json.dump(prev, f, indent=2, default=str)


if __name__ == "__main__":
    main()
