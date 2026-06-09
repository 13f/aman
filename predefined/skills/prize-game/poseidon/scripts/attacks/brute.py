"""
Poseidon collision search — Floyd rho + Birthday attacks.

Official parameters: p = 2130706433, α = 3, t = 16, RF = 8, RP = 20
"""

import time
import signal
import hashlib
import math
import json
import os
from typing import Callable, Optional

from framework.field import random_element, P, INPUT_WIDTH
from framework.poseidon import hash_with_seed, count_matching_outputs, check_collision
from framework.poseidon import make_default_poseidon


# ── Global stop flag (set by SIGINT / timeout) ───────────────────────

_stop_requested: bool = False


def request_stop(signum=None, frame=None) -> None:
    global _stop_requested
    _stop_requested = True


def is_stopped() -> bool:
    return _stop_requested


def reset_stop() -> None:
    global _stop_requested
    _stop_requested = False


# ── Helpers ───────────────────────────────────────────────────────────


def random_input() -> list[int]:
    return [random_element() for _ in range(INPUT_WIDTH)]


def _output_key(outputs: list[int]) -> str:
    data = ",".join(str(x) for x in outputs).encode()
    return hashlib.sha256(data).hexdigest()


# ── Timeout context manager ───────────────────────────────────────────


class Timeout:
    """Raise TimeoutError after `seconds` (uses SIGALRM, Unix only)."""

    def __init__(self, seconds: int):
        self.seconds = seconds

    def __enter__(self):
        if self.seconds > 0 and hasattr(signal, "SIGALRM"):
            signal.signal(signal.SIGALRM, self._handler)
            signal.alarm(self.seconds)
        return self

    def __exit__(self, *args):
        if self.seconds > 0 and hasattr(signal, "SIGALRM"):
            signal.alarm(0)
        return False

    @staticmethod
    def _handler(signum, frame):
        raise TimeoutError("time limit reached")


# ── Birthday Attack ───────────────────────────────────────────────────


def birthday_search(
    q: int,
    max_iters: int = 10_000_000,
    max_seconds: int = 0,
    checkpoint_interval: int = 100_000,
    checkpoint_file: str | None = None,
    progress_cb: Optional[Callable] = None,
) -> dict:
    """Birthday collision search.

    Stops when: collision found, max_iters reached, max_seconds elapsed,
    or SIGINT received (Ctrl+C).

    Args:
        q: Leading output elements to match (1..16).
        max_iters: Max random samples.
        max_seconds: Wall-clock time limit (0 = no limit).
        checkpoint_interval: Report progress every N iterations.
        checkpoint_file: Path to save partial results on interrupt.
        progress_cb: callback(iteration, hashes, best_match, rate, elapsed).

    Returns:
        dict with: found, iterations, hashes_computed, elapsed_seconds,
                   hashes_per_second, best_match_count, stopped_early.
    """
    reset_stop()

    # Register SIGINT handler
    prev_sigint = signal.signal(signal.SIGINT, request_stop)

    seen: dict[str, list[int]] = {}
    best_match: int = 0
    hashes_computed: int = 0
    start_time: float = time.time()
    stopped_early: bool = False

    try:
        for i in range(max_iters):
            # ── Stop checks ──────────────────────────────────────────
            if is_stopped():
                stopped_early = True
                break

            if max_seconds > 0 and (time.time() - start_time) > max_seconds:
                stopped_early = True
                break

            # ── Hash ─────────────────────────────────────────────────
            x = random_input()
            out = hash_with_seed(x, out_length=q)
            key = _output_key(out)
            hashes_computed += 1

            if key in seen:
                y = seen[key]
                elapsed = time.time() - start_time
                return {
                    "found": True,
                    "iterations": i + 1,
                    "hashes_computed": hashes_computed,
                    "elapsed_seconds": elapsed,
                    "hashes_per_second": hashes_computed / elapsed if elapsed > 0 else 0,
                    "best_match_count": q,
                    "collision": {"x": x, "y": y, "matching_outputs": q},
                    "stopped_early": False,
                }

            seen[key] = x

            # Track best partial match (sample every 1000)
            if i % 1000 == 0 and len(seen) > 1:
                prev_items = list(seen.items())
                idx = i % len(prev_items)
                prev_input = prev_items[idx][1]
                match_count = count_matching_outputs(x, prev_input)
                if match_count > best_match:
                    best_match = match_count

            # Progress
            if i > 0 and i % checkpoint_interval == 0:
                elapsed = time.time() - start_time
                hps = hashes_computed / elapsed if elapsed > 0 else 0
                if progress_cb:
                    progress_cb(i, hashes_computed, best_match, hps, elapsed)

    finally:
        signal.signal(signal.SIGINT, prev_sigint)

    elapsed = time.time() - start_time

    # Save checkpoint if requested
    if checkpoint_file:
        _save_checkpoint(checkpoint_file, {
            "q": q, "iterations": hashes_computed, "best_match": best_match,
            "elapsed_seconds": elapsed, "stopped_early": stopped_early,
        })

    return {
        "found": False,
        "iterations": hashes_computed,
        "hashes_computed": hashes_computed,
        "elapsed_seconds": elapsed,
        "hashes_per_second": hashes_computed / elapsed if elapsed > 0 else 0,
        "best_match_count": best_match,
        "stopped_early": stopped_early,
    }


# ── Floyd Rho ─────────────────────────────────────────────────────────


def floyd_rho_search(
    q: int = 1,
    seed: int | None = None,
    phase1_max_iters: int = 1_000_000_000,
    max_restarts: int = 1000,
    max_seconds: int = 0,
    checkpoint_file: str | None = None,
    verbose: bool = True,
) -> dict | None:
    """Floyd's rho cycle detection — O(sqrt(p^q)) hashes, O(1) memory.

    Stops when: collision found, phase1_max_iters exceeded in any phase,
    max_seconds elapsed, max_restarts exhausted, or SIGINT (Ctrl+C).

    Args:
        q: Leading output elements to match.
        seed: RNG seed.
        phase1_max_iters: Max iterations in Phase 1/2/3 loops (safety valve).
        max_restarts: Max restart attempts.
        max_seconds: Wall-clock time limit (0 = no limit).
        checkpoint_file: Path to save partial state on interrupt.
        verbose: Print progress.

    Returns:
        dict with collision info, or None.
    """
    import random as _random

    reset_stop()
    prev_sigint = signal.signal(signal.SIGINT, request_stop)

    rng = _random.Random(seed)
    t0 = time.perf_counter()
    hash_calls = 0

    # Cached Poseidon and f(v)
    pos = make_default_poseidon()

    def f(v: int) -> int:
        inp = [v] + [0] * (INPUT_WIDTH - 1)
        return hash_with_seed(inp, pos, out_length=1)[0]

    expected = int(3 * math.isqrt(P ** q))
    if verbose:
        print(f"Floyd rho — q={q}")
        print(f"  Expected: ~{expected:,} calls | ~{expected/1900:.0f}s")
        print(f"  Stop: Ctrl+C | timeout after {max_seconds}s" if max_seconds else "  Stop: Ctrl+C")
        print()

    result = None

    try:
        for restart in range(1, max_restarts + 1):
            # ── Stop checks ──────────────────────────────────────────
            if is_stopped():
                if verbose:
                    print("\n[stopped by user]")
                break
            if max_seconds > 0 and (time.perf_counter() - t0) > max_seconds:
                if verbose:
                    print(f"\n[timeout after {max_seconds}s]")
                break

            v0 = rng.randint(0, P - 1)

            # ── Phase 1: cycle detection ─────────────────────────────
            tortoise = f(v0); hash_calls += 1
            hare = f(tortoise); hash_calls += 1
            p1_iters = 1

            while tortoise != hare:
                if is_stopped():
                    break
                if p1_iters > phase1_max_iters:
                    if verbose:
                        print(f"  restart {restart}: Phase 1 exceeded {phase1_max_iters} iters, retrying...")
                    break
                tortoise = f(tortoise); hash_calls += 1
                hare = f(f(hare)); hash_calls += 2
                p1_iters += 1

                if verbose and hash_calls % 100_000 == 0:
                    elapsed = time.perf_counter() - t0
                    print(f"  {hash_calls:>10,} calls | {elapsed:.0f}s | ~{hash_calls/elapsed:.0f} h/s")

            if is_stopped() or tortoise != hare:
                continue

            # ── Phase 2: find cycle entry mu ─────────────────────────
            tortoise2 = v0
            mu = 0
            while tortoise2 != hare:
                if is_stopped() or mu > phase1_max_iters:
                    break
                tortoise2 = f(tortoise2); hash_calls += 1
                hare = f(hare); hash_calls += 1
                mu += 1

            if is_stopped():
                break

            if mu == 0:
                if verbose:
                    print(f"  restart {restart}: mu=0, retrying...")
                continue

            # ── Phase 3: extract collision ───────────────────────────
            tail_pred = v0
            for _ in range(mu - 1):
                if is_stopped():
                    break
                tail_pred = f(tail_pred); hash_calls += 1

            if is_stopped():
                break

            x_mu = tortoise2
            cycle_pred = x_mu
            p3_iters = 0
            while True:
                if is_stopped() or p3_iters > phase1_max_iters:
                    break
                nxt = f(cycle_pred); hash_calls += 1
                p3_iters += 1
                if nxt == x_mu:
                    break
                cycle_pred = nxt

            if is_stopped() or p3_iters > phase1_max_iters:
                continue

            if tail_pred == cycle_pred:
                if verbose:
                    print(f"  restart {restart}: degenerate (tail=cycle), retrying...")
                continue

            # ── Verify ───────────────────────────────────────────────
            x = [tail_pred] + [0] * (INPUT_WIDTH - 1)
            y = [cycle_pred] + [0] * (INPUT_WIDTH - 1)

            if not check_collision(x, y, q, pos):
                if verbose:
                    print(f"  restart {restart}: verification failed, retrying...")
                continue

            elapsed = time.perf_counter() - t0
            if verbose:
                hx = hash_with_seed(x, pos, out_length=q)
                hy = hash_with_seed(y, pos, out_length=q)
                print(f"\n✅ COLLISION FOUND — {hash_calls:,} calls, {elapsed:.0f}s, restart {restart}")
                print(f"  tail_pred  = {tail_pred}")
                print(f"  cycle_pred = {cycle_pred}")
                print(f"  H(x)[:{q}]  = {hx}")
                print(f"  H(y)[:{q}]  = {hy}")

            result = {
                "found": True,
                "strategy": "floyd_rho",
                "hash_calls": hash_calls,
                "elapsed_seconds": elapsed,
                "restarts": restart,
                "collision": {"x": x, "y": y, "matching_outputs": q},
            }
            break

    finally:
        signal.signal(signal.SIGINT, prev_sigint)

    if result is None and checkpoint_file:
        _save_checkpoint(checkpoint_file, {
            "q": q, "hash_calls": hash_calls,
            "elapsed_seconds": time.perf_counter() - t0,
            "stopped_by_user": is_stopped(),
        })

    return result


# ── Checkpoint ────────────────────────────────────────────────────────


def _save_checkpoint(path: str, data: dict) -> None:
    """Save checkpoint data to JSON file."""
    data["timestamp"] = time.strftime("%Y-%m-%dT%H:%M:%S")
    try:
        os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
        with open(path, "w") as f:
            json.dump(data, f, indent=2)
    except OSError:
        pass


# ── Benchmark ─────────────────────────────────────────────────────────


def benchmark_hash_speed(num_hashes: int = 10_000) -> dict:
    pos = make_default_poseidon()
    start = time.time()
    for _ in range(num_hashes):
        x = random_input()
        hash_with_seed(x, pos, out_length=16)
    elapsed = time.time() - start
    return {
        "num_hashes": num_hashes,
        "elapsed_seconds": elapsed,
        "hashes_per_second": num_hashes / elapsed if elapsed > 0 else 0,
    }
