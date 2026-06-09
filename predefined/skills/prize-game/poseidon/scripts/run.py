#!/usr/bin/env python3
"""
Poseidon Collision Search — CLI.

Usage:
    python3 run.py floyd-rho --q 1                        # q=1 (~2 min)
    python3 run.py floyd-rho --q 2 --timeout 3600          # q=2, stop after 1h
    python3 run.py birthday --q 2 --iters 5e6 --timeout 600
    python3 run.py benchmark

Stop conditions (all commands):
    - Collision found → exit with result
    - --timeout N      → stop after N seconds
    - --iters N        → stop after N iterations (birthday) or per-phase (floyd-rho)
    - Ctrl+C           → graceful stop, save checkpoint
"""

import sys
import os
import time
import json
import argparse
import signal

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, SCRIPT_DIR)

from framework.field import P, SEED, INPUT_WIDTH
from framework.poseidon import make_default_poseidon, hash_with_seed, check_collision
from attacks.brute import (
    birthday_search,
    floyd_rho_search,
    benchmark_hash_speed,
    request_stop,
    reset_stop,
)

# ── Globals ───────────────────────────────────────────────────────────

RESULT_FILE = os.path.join(SCRIPT_DIR, "results.json")


def save_result(entry: dict) -> None:
    """Append a result entry to results.json."""
    previous = {}
    if os.path.exists(RESULT_FILE):
        try:
            with open(RESULT_FILE) as f:
                previous = json.load(f)
        except (json.JSONDecodeError, OSError):
            pass
    if "history" not in previous:
        previous["history"] = []
    previous["history"].append(entry)
    previous["last_run"] = entry
    with open(RESULT_FILE, "w") as f:
        json.dump(previous, f, indent=2, default=str)


# ── Commands ──────────────────────────────────────────────────────────


def cmd_floyd_rho(args):
    """Floyd's rho cycle detection."""
    pos = make_default_poseidon()
    bench = benchmark_hash_speed(2000)

    expected = int(3 * (P ** (args.q / 2)))
    eta = expected / bench["hashes_per_second"]
    print(f"Speed: {bench['hashes_per_second']:.0f} h/s | Expected: ~{expected:,} calls | ETA: ~{eta:.0f}s")
    if args.timeout:
        print(f"Timeout: {args.timeout}s | Max phase iters: {args.phase1_max:,}")
    print()

    result = floyd_rho_search(
        q=args.q,
        seed=args.seed,
        phase1_max_iters=args.phase1_max,
        max_restarts=args.max_restarts,
        max_seconds=args.timeout,
        checkpoint_file=args.checkpoint,
        verbose=True,
    )

    if result:
        print(f"\n✅ q={args.q} collision found")
        save_result(result)
    else:
        print(f"\n⏹ Stopped — no collision found")

    return result


def cmd_birthday(args):
    """Birthday attack with hash caching."""
    bench = benchmark_hash_speed(2000)
    eta = args.iters / bench["hashes_per_second"]
    print(f"Speed: {bench['hashes_per_second']:.0f} h/s | Max: {args.iters:,} iters | ETA: ~{eta:.0f}s")
    if args.timeout:
        print(f"Timeout: {args.timeout}s")
    print()

    def progress(i, hashes, best, rate, elapsed):
        pct = i / args.iters * 100
        print(f"\r  {i:>10,} ({pct:.1f}%) | best={best}/{args.q} | {rate:.0f} h/s | {elapsed:.0f}s",
              end="", flush=True)

    result = birthday_search(
        q=args.q,
        max_iters=args.iters,
        max_seconds=args.timeout,
        checkpoint_interval=args.checkpoint,
        checkpoint_file=args.save_checkpoint,
        progress_cb=progress,
    )
    print()

    if result["found"]:
        print(f"\n✅ q={args.q} collision found — {result['elapsed_seconds']:.0f}s")
        save_result(result)
    else:
        reason = "stopped early" if result.get("stopped_early") else "max iters reached"
        print(f"\n⏹ {reason} — best match: {result['best_match_count']}/{args.q}")

    return result


def cmd_benchmark(args):
    """Hash speed test."""
    bench = benchmark_hash_speed(args.n)
    print(f"Poseidon — KoalaBear (p=2³¹-2²⁴+1), t=16, RF=8, RP=20")
    print(f"  {bench['hashes_per_second']:.0f} hashes/sec  ({bench['elapsed_seconds']:.3f}s for {args.n})")
    # Also show time estimates
    for q in [1, 2, 3]:
        calls = int(3 * (P ** (q / 2)))
        t = calls / bench["hashes_per_second"]
        unit = "s"
        if t > 86400:
            t /= 86400; unit = "days"
        elif t > 3600:
            t /= 3600; unit = "h"
        elif t > 60:
            t /= 60; unit = "min"
        print(f"  q={q}: ~{calls:,} calls → ~{t:.1f}{unit}")
    return bench


# ── Main ──────────────────────────────────────────────────────────────


def main():
    parser = argparse.ArgumentParser(
        description="Poseidon Collision Search",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  python3 run.py floyd-rho --q 1                     # q=1, ~2 min
  python3 run.py floyd-rho --q 2 --timeout 3600       # q=2, 1h limit
  python3 run.py birthday --q 2 --iters 10000000       # 10M samples
  python3 run.py birthday --q 2 --timeout 600          # 10 min limit
  python3 run.py benchmark
        """,
    )
    sub = parser.add_subparsers(dest="cmd")

    # ── floyd-rho ────────────────────────────────────────────────────
    p_rho = sub.add_parser("floyd-rho", help="Floyd rho cycle detection (O(1) memory)")
    p_rho.add_argument("--q", type=int, default=1, help="Collision length (default: 1)")
    p_rho.add_argument("--seed", type=int, default=None, help="RNG seed")
    p_rho.add_argument("--timeout", type=int, default=0, metavar="SEC",
                       help="Wall-clock time limit (0=no limit)")
    p_rho.add_argument("--phase1-max", type=int, default=1_000_000_000, metavar="N",
                       help="Max iters in Phase 1/2/3 loops (safety valve)")
    p_rho.add_argument("--max-restarts", type=int, default=1000,
                       help="Max restart attempts")
    p_rho.add_argument("--checkpoint", type=str, default=None, metavar="PATH",
                       help="Save partial state on stop")

    # ── birthday ─────────────────────────────────────────────────────
    p_bday = sub.add_parser("birthday", help="Birthday attack with hash caching")
    p_bday.add_argument("--q", type=int, default=2, help="Collision length (default: 2)")
    p_bday.add_argument("--iters", type=int, default=1_000_000, metavar="N",
                        help="Max iterations (default: 1,000,000)")
    p_bday.add_argument("--timeout", type=int, default=0, metavar="SEC",
                        help="Wall-clock time limit (0=no limit)")
    p_bday.add_argument("--checkpoint", type=int, default=100_000,
                        help="Progress report interval")
    p_bday.add_argument("--save-checkpoint", type=str, default=None, metavar="PATH",
                        help="Save partial results on stop")

    # ── benchmark ────────────────────────────────────────────────────
    p_bench = sub.add_parser("benchmark", help="Hash speed benchmark")
    p_bench.add_argument("--n", type=int, default=10_000, help="Number of hashes")

    args = parser.parse_args()

    if args.cmd == "floyd-rho":
        cmd_floyd_rho(args)
    elif args.cmd == "birthday":
        cmd_birthday(args)
    elif args.cmd == "benchmark":
        cmd_benchmark(args)
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
