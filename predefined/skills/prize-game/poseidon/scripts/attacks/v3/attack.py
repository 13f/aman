#!/usr/bin/env python3
"""
v3 — Inverse Permutation Multi-Start Newton for q=4

Strategy:
  Since cube root is a bijection (gcd(3, p-1) = 1), the full Poseidon
  permutation is completely invertible. We exploit this to formulate
  the collision problem as a fixed-point system:

  Let Q = P(0_padded) — the permutation output for zero input.
  For collision with Y=0, we need X such that:
    P(X_padded)[0] = Q[0]
    P(X_padded)[1] + X[0] = Q[1]
    P(X_padded)[2] + X[1] = Q[2]
    P(X_padded)[3] + X[2] = Q[3]

  Let S = P(X_padded) be the state after permutation. Then X_padded = P⁻¹(S).
  We parameterize S = [Q[0], s1, s2, s3, s4..s15] and solve for S
  such that P⁻¹(S)[0] = SEED and s_i + P⁻¹(S)[i] = Q[i] for i=1,2,3.

  With 15 unknowns (s1..s15) and 4 constraints, we have 11 DOF.
  We use Newton's method on the first 12 variables, fixing the rest.

Usage:
    python3 attacks/v3/attack.py [--outer N] [--seed S]
"""

import sys, os, time, json, argparse, random as _random

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
ROOT_DIR = os.path.dirname(os.path.dirname(SCRIPT_DIR))
sys.path.insert(0, ROOT_DIR)

from framework.field import P, INPUT_WIDTH, SEED
from framework.poseidon import (
    make_default_poseidon, make_plonky3_poseidon,
    hash_with_seed, check_collision
)
from framework.mds import apply_mds, generate_circulant_mds_matrix, PLONKY3_MDS_FIRST_ROW_16

# ── Cube root (bijection since gcd(3, p-1) = 1) ──────────────────────
CBRT_EXP = (2 * P - 1) // 3  # = (2p-1)/3

def cbrt(x):
    if x == 0: return 0
    return pow(x, CBRT_EXP, P)

# ── MDS Inverse ───────────────────────────────────────────────────────
def matrix_inverse(m):
    """Compute inverse of t×t matrix over F_p using Gauss-Jordan."""
    t = len(m)
    aug = [m[i][:] + [1 if i == j else 0 for j in range(t)] for i in range(t)]
    for col in range(t):
        pivot = None
        for row in range(col, t):
            if aug[row][col] != 0:
                pivot = row; break
        if pivot is None:
            raise ValueError("Matrix not invertible")
        aug[col], aug[pivot] = aug[pivot], aug[col]
        piv_inv = pow(aug[col][col], P - 2, P)
        for j in range(2 * t):
            aug[col][j] = (aug[col][j] * piv_inv) % P
        for row in range(t):
            if row != col and aug[row][col] != 0:
                factor = aug[row][col]
                for j in range(2 * t):
                    aug[row][j] = (aug[row][j] - factor * aug[col][j]) % P
    return [aug[i][t:] for i in range(t)]

# ── Inverse Permutation ───────────────────────────────────────────────
def invert_permutation(pos, state_in):
    """Fully invert the Poseidon permutation (all 28 rounds)."""
    t, r_f, r_p, prime = pos.t, pos.r_f, pos.r_p, pos.prime
    half_f = r_f // 2
    rc = pos.round_constants

    if not hasattr(pos, '_mds_inv'):
        pos._mds_inv = matrix_inverse(pos.mds)
    mds_inv = pos._mds_inv

    state = list(state_in)

    # Invert back full rounds (4 rounds)
    for i in range(half_f - 1, -1, -1):
        rc_idx = half_f + r_p + i
        s = apply_mds(state, mds_inv, prime)     # undo MDS
        s = [cbrt(x) for x in s]                  # undo S-Box (cube root)
        s = [(s[j] - rc[rc_idx][j]) % prime for j in range(t)]  # undo round constants
        state = s

    # Invert partial rounds (20 rounds)
    for i in range(r_p - 1, -1, -1):
        rc_idx = half_f + i
        s = apply_mds(state, mds_inv, prime)      # undo MDS
        s[0] = cbrt(s[0])                          # undo S-Box on s[0] only
        s = [(s[j] - rc[rc_idx][j]) % prime for j in range(t)]  # undo round constants
        state = s

    # Invert front full rounds (4 rounds)
    for i in range(half_f - 1, -1, -1):
        rc_idx = i
        s = apply_mds(state, mds_inv, prime)
        s = [cbrt(x) for x in s]
        s = [(s[j] - rc[rc_idx][j]) % prime for j in range(t)]
        state = s

    return state


# ── Collision Solver: Inverse + Fixed-Point ───────────────────────────
def solve_collision(pos, Q, max_restarts=500, max_newton=20, seed=None):
    """
    Find X such that H(X) collides with H(0) on first 4 elements.

    Uses inverse permutation: parameterize permutation output S,
    invert to get X_padded = P⁻¹(S), then optimize S to satisfy
    the collision constraints.

    Returns (X, stats) or (None, stats).
    """
    rng = _random.Random(seed)
    t = pos.t

    best_error = float('inf')
    calls = 0

    for restart in range(max_restarts):
        # Random initialization of S (permutation output state)
        S = [0] * t
        S[0] = Q[0]  # fixed: P(X)[0] = Q[0]

        # Initialize S[1..3] as random (will be adjusted by Newton)
        for i in range(1, 4):
            S[i] = rng.randint(0, P - 1)

        # Initialize S[4..15] as random free parameters
        for i in range(4, t):
            S[i] = rng.randint(0, P - 1)

        for niter in range(max_newton):
            # Compute X_padded = P⁻¹(S)
            X_padded = invert_permutation(pos, S)
            calls += 1

            # Errors:
            # E0: X_padded[0] - SEED (must be 0)
            # Ei: S[i] + X_padded[i] - Q[i] for i=1,2,3
            E0 = (X_padded[0] - SEED) % P
            E = [E0]
            for i in range(1, 4):
                E.append((S[i] + X_padded[i] - Q[i]) % P)

            total_err = sum(e * e for e in E)
            if total_err < best_error:
                best_error = total_err

            if total_err == 0:
                # Extract X from X_padded
                X = [X_padded[i] % P for i in range(1, t)]
                # Verify
                if check_collision(X, [0]*INPUT_WIDTH, 4, pos):
                    return X, {"restarts": restart+1, "calls": calls, "error": 0}
                # Otherwise false positive, continue

            # Compute Jacobian of E w.r.t. S[1..15] (15 vars, 4 eqs)
            # Underdetermined: use first 4 vars for Newton step,
            # fix remaining 11 (or use pseudo-inverse / damped)

            # We compute J[i][j] = ∂E[i]/∂S[j] for i=0..3, j=1..3
            # (only vary S[1..3]; S[4..15] are free parameters kept fixed for now)
            J = [[0]*3 for _ in range(4)]
            eps = 1
            for j in range(3):
                S_j_orig = S[j+1]
                S[j+1] = (S_j_orig + eps) % P
                Xp = invert_permutation(pos, S)
                calls += 1

                # E0: Xp[0] - SEED
                dE0 = (Xp[0] - SEED - E0) % P
                J[0][j] = (dE0 * pow(eps, P-2, P)) % P

                for i in range(1, 4):
                    dEi = (S[i] + Xp[i] - Q[i] - E[i]) % P
                    J[i][j] = (dEi * pow(eps, P-2, P)) % P

                S[j+1] = S_j_orig  # restore

            # Solve J × Δ = -E for Δ (4×3 system, underdetermined)
            # Use least squares or fix one variable
            # Simplify: solve 3×3 sub-system for S[1..3] using E[1..3],
            # then check if E[0] improves

            # Extract 3×3 sub-Jacobian for S[1..3] → E[1..3]
            J33 = [J[i][:] for i in range(1, 4)]  # rows 1-3, cols 0-2

            # Solve J33 × Δ = -E[1..3]
            rhs = [(-E[i]) % P for i in range(1, 4)]

            # Gaussian elimination
            M = [J33[i][:] + [rhs[i]] for i in range(3)]
            singular = False
            for col in range(3):
                pivot = None
                for row in range(col, 3):
                    if M[row][col] != 0:
                        pivot = row; break
                if pivot is None:
                    singular = True; break
                M[col], M[pivot] = M[pivot], M[col]
                piv_inv = pow(M[col][col], P-2, P)
                for j in range(4):
                    M[col][j] = (M[col][j] * piv_inv) % P
                for row in range(3):
                    if row != col and M[row][col] != 0:
                        factor = M[row][col]
                        for j in range(4):
                            M[row][j] = (M[row][j] - factor * M[col][j]) % P

            if singular:
                # Perturb and retry
                for i in range(1, 4):
                    S[i] = (S[i] + rng.randint(1, 100)) % P
                continue

            delta = [M[i][3] for i in range(3)]

            # Update S[1..3] with damping
            damp = 1.0
            for i in range(3):
                S[i+1] = (S[i+1] + int(delta[i] * damp)) % P

            # After updating S[1..3], also perturb S[4..15] slightly
            # to escape local minima
            if niter % 5 == 4:
                for i in range(4, t):
                    S[i] = (S[i] + rng.randint(-50, 50)) % P

    return None, {"restarts": max_restarts, "calls": calls, "best_error": best_error}


# ── Main ──────────────────────────────────────────────────────────────
def main():
    parser = argparse.ArgumentParser(description="v3 — Inverse Permutation Newton q=4")
    parser.add_argument("--outer", type=int, default=500, help="Max restarts")
    parser.add_argument("--newton", type=int, default=20, help="Max Newton iters per restart")
    parser.add_argument("--seed", type=int, default=None)
    parser.add_argument("--mds", choices=["default", "plonky3"], default="default")
    args = parser.parse_args()

    # Build Poseidon with chosen MDS
    if args.mds == "plonky3":
        pos = make_plonky3_poseidon()
    else:
        pos = make_default_poseidon()

    print(f"v3 — Inverse Permutation Newton | MDS: {args.mds}")
    print(f"  Restarts: {args.outer}, Newton iters: {args.newton}")

    # Compute target Q = P(0_padded)
    pad_zero = [SEED] + [0] * INPUT_WIDTH
    Q = pos.permutation(pad_zero)
    print(f"  Q[0..3] = {Q[:4]}")
    print()

    t0 = time.time()
    X, stats = solve_collision(
        pos, Q,
        max_restarts=args.outer,
        max_newton=args.newton,
        seed=args.seed
    )
    elapsed = time.time() - t0

    if X is not None:
        print(f"\n✅ q=4 COLLISION FOUND!")
        print(f"  X = {X}")
        hx = hash_with_seed(X, pos, 4)
        print(f"  H(X)[:4] = {hx}")
        result = {
            "version": "v3",
            "strategy": "inverse_permutation_newton",
            "q": 4,
            "found": True,
            "collision": {"x": X, "y": [0]*INPUT_WIDTH, "hx": hx},
            "elapsed_seconds": elapsed,
            **stats
        }
        with open(os.path.join(SCRIPT_DIR, "checkpoint.json"), "w") as f:
            json.dump({"state": "done", **result}, f, indent=2)
    else:
        print(f"\n❌ No collision found")
        print(f"  Restarts: {stats['restarts']}, Inverse calls: {stats['calls']}")
        print(f"  Best error: {stats['best_error']}")
        print(f"  Elapsed: {elapsed:.1f}s")
        result = {
            "version": "v3",
            "strategy": "inverse_permutation_newton",
            "q": 4,
            "found": False,
            "elapsed_seconds": elapsed,
            **stats
        }
        # Save checkpoint
        with open(os.path.join(SCRIPT_DIR, "checkpoint.json"), "w") as f:
            json.dump({
                "version": "v3",
                "state": "done",
                "found": False,
                "best_error": stats['best_error'],
                "last_saved": time.strftime("%Y-%m-%dT%H:%M:%S")
            }, f, indent=2)

    # Append to results.json
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
