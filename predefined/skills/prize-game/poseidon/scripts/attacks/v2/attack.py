#!/usr/bin/env python3
"""
v2 — Combined Fixed-Point + Newton for q=4

Strategy:
  Phase 1: Fixed-point iteration on X[0..3] (with Z=0 fixed).
           This quickly finds a point X* where X ≈ G(X*).
           
  Phase 2: For each X*, use Newton on Z[0..11] to satisfy the 
           additional constraints (input[0]=SEED, input[5..15]=0).
  
  The key insight: Phase 1 gives us X values that are "close" to
  solving the full system. Phase 2 tries to fix Z to close the gap.

Usage:
    python3 attacks/v2/attack.py [--outer N] [--inner N] [--resume]
"""

import sys, os, time, json, signal, argparse, random as _random

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
ROOT_DIR = os.path.dirname(os.path.dirname(SCRIPT_DIR))
sys.path.insert(0, ROOT_DIR)

from framework.field import P, INPUT_WIDTH, SEED
from framework.poseidon import make_default_poseidon, hash_with_seed, check_collision
from framework.mds import apply_mds

# ── Globals ───────────────────────────────────────────────────────────
CHECKPOINT_FILE = os.path.join(SCRIPT_DIR, "checkpoint.json")
_stop_requested = False
def request_stop(signum=None, frame=None):
    global _stop_requested
    _stop_requested = True
def is_stopped():
    return _stop_requested
def load_checkpoint():
    if os.path.exists(CHECKPOINT_FILE):
        with open(CHECKPOINT_FILE) as f:
            return json.load(f)
    return {}
def save_checkpoint(state):
    state["last_saved"] = time.strftime("%Y-%m-%dT%H:%M:%S")
    with open(CHECKPOINT_FILE, "w") as f:
        json.dump(state, f, indent=2)

# ── Cube Root ─────────────────────────────────────────────────────────
CUBE_ROOT_EXP = (2 * P - 1) // 3
def cbrt(x):
    if x == 0: return 0
    return pow(x, CUBE_ROOT_EXP, P)

CBRT_CACHE = {}

def cbrt_cached(x):
    if x not in CBRT_CACHE:
        CBRT_CACHE[x] = cbrt(x)
    return CBRT_CACHE[x]

# ── MDS Inverse ───────────────────────────────────────────────────────
def mds_inverse(mds, prime=None):
    if prime is None: prime = P
    t = len(mds)
    aug = [mds[i][:] + [1 if i == j else 0 for j in range(t)] for i in range(t)]
    for col in range(t):
        pivot = None
        for row in range(col, t):
            if aug[row][col] != 0:
                pivot = row; break
        if pivot is None: raise ValueError("Not invertible")
        aug[col], aug[pivot] = aug[pivot], aug[col]
        piv_inv = pow(aug[col][col], prime-2, prime)
        for j in range(2*t): aug[col][j] = (aug[col][j] * piv_inv) % prime
        for row in range(t):
            if row != col and aug[row][col] != 0:
                factor = aug[row][col]
                for j in range(2*t): aug[row][j] = (aug[row][j] - factor * aug[col][j]) % prime
    return [aug[i][t:] for i in range(t)]

# ── Inverse Permutation ───────────────────────────────────────────────
def invert_permutation(pos, state_in):
    t, r_f, r_p, prime = pos.t, pos.r_f, pos.r_p, pos.prime
    half_f = r_f // 2
    rc = pos.round_constants
    if not hasattr(pos, '_mds_inv'):
        pos._mds_inv = mds_inverse(pos.mds, prime)
    mds_inv = pos._mds_inv
    state = list(state_in)
    for i in range(half_f - 1, -1, -1):
        rc_idx = half_f + r_p + i
        s = apply_mds(state, mds_inv, prime)
        s = [cbrt_cached(x) for x in s]
        s = [(s[j] - rc[rc_idx][j]) % prime for j in range(t)]
        state = s
    for i in range(r_p - 1, -1, -1):
        rc_idx = half_f + i
        s = apply_mds(state, mds_inv, prime)
        s[0] = cbrt_cached(s[0])
        s = [(s[j] - rc[rc_idx][j]) % prime for j in range(t)]
        state = s
    for i in range(half_f - 1, -1, -1):
        rc_idx = i
        s = apply_mds(state, mds_inv, prime)
        s = [cbrt_cached(x) for x in s]
        s = [(s[j] - rc[rc_idx][j]) % prime for j in range(t)]
        state = s
    return state

# ── Solver ────────────────────────────────────────────────────────────

def solve_nxn(A, b, n):
    """Solve n×n linear system over F_p."""
    M = [A[i][:] + [b[i]] for i in range(n)]
    for col in range(n):
        pivot = None
        for row in range(col, n):
            if M[row][col] != 0: pivot = row; break
        if pivot is None: return None
        M[col], M[pivot] = M[pivot], M[col]
        piv_inv = pow(M[col][col], P-2, P)
        for j in range(col, n+1): M[col][j] = (M[col][j] * piv_inv) % P
        for row in range(n):
            if row != col and M[row][col] != 0:
                factor = M[row][col]
                for j in range(col, n+1): M[row][j] = (M[row][j] - factor * M[col][j]) % P
    return [M[i][n] for i in range(n)]

def build_target_perm(K, X, Z):
    """Build permutation target from X and Z."""
    tp = [0]*16
    tp[0] = K
    tp[1] = (args.target[1] - X[0]) % P
    tp[2] = (args.target[2] - X[1]) % P
    tp[3] = (args.target[3] - X[2]) % P
    for k in range(12):
        tp[4+k] = Z[k]
    return tp

class Args:
    pass

args = Args()

def run_strategy(outer_count, inner_steps, seed):
    global args
    rng = _random.Random(seed)
    signal.signal(signal.SIGINT, request_stop)
    
    pos = make_default_poseidon()
    CBRT_CACHE.clear()
    
    Y = [0] * INPUT_WIDTH
    target = hash_with_seed(Y, pos, out_length=4)
    K = (target[0] - SEED) % P
    args.target = target
    
    print(f"v2 — Combined Fixed-Point + Newton")
    print(f"  Target = {target}")
    print(f"  Outer: {outer_count}, Inner steps: {inner_steps}")
    print()
    
    ck = load_checkpoint()
    start_outer = ck.get("outer", 0) if False else 0  # No resume for simplicity
    start_time = time.time()
    best_total = float('inf')
    
    sqrt_damp = int(P ** 0.5)
    
    for outer in range(outer_count):
        if is_stopped():
            print(f"\n[stopped]"); break
        
        X = [rng.randint(0, P-1) for _ in range(4)]
        Z = [0] * 12
        
        # Phase 1: Fixed-point on X (15 iterations)
        for fp_step in range(15):
            tp = build_target_perm(K, X, Z)
            inp = invert_permutation(pos, tp)
            new_X = [inp[1], inp[2], inp[3], inp[4]]
            
            # Check constraints
            res_seed = (inp[0] - SEED) % P
            res_tail = sum(inp[i]*inp[i] for i in range(5,16)) % P
            total_res = (res_seed * res_seed + res_tail) % P
            
            if total_res == 0:
                full_X = new_X + [0]*(INPUT_WIDTH-4)
                if check_collision(full_X, Y, 4, pos):
                    elapsed = time.time() - start_time
                    print(f"\n✅ q=4 COLLISION (Phase 1)!")
                    hx = hash_with_seed(full_X, pos, 4)
                    return {"found": True, "collision": {"x": full_X, "y": Y, "hx": hx, "hy": target},
                            "elapsed": elapsed}
            
            if abs(res_seed) < best_total: best_total = abs(res_seed)
            X = new_X
        
        # Phase 2: Newton on Z (fix X, vary Z)
        best_z_res = float('inf')
        for z_step in range(inner_steps):
            tp = build_target_perm(K, X, Z)
            inp = invert_permutation(pos, tp)
            
            E = [inp[i] for i in range(5, 16)]  # 11 elements
            res_seed = (inp[0] - SEED) % P
            
            z_res = sum(e*e for e in E) % P
            if z_res < best_z_res: best_z_res = z_res
            
            if z_res == 0 and res_seed == 0:
                new_X = [inp[1], inp[2], inp[3], inp[4]]
                full_X = new_X + [0]*(INPUT_WIDTH-4)
                if check_collision(full_X, Y, 4, pos):
                    elapsed = time.time() - start_time
                    print(f"\n✅ q=4 COLLISION (Phase 2)!")
                    hx = hash_with_seed(full_X, pos, 4)
                    return {"found": True, "collision": {"x": full_X, "y": Y, "hx": hx, "hy": target},
                            "elapsed": elapsed}
            
            # Compute Jacobian of Z error w.r.t. Z
            J = [[0]*12 for _ in range(11)]
            for j in range(12):
                Zt = list(Z)
                Zt[j] = (Zt[j] + 1) % P
                tpt = build_target_perm(K, X, Zt)
                inpt = invert_permutation(pos, tpt)
                for i in range(11):
                    J[i][j] = (inpt[5+i] - E[i]) % P
            
            # Underdetermined: 11 eqs, 12 vars. Fix Z[0] and solve for Z[1..11]
            J_red = [J[i][1:] for i in range(11)]
            delta_red = solve_nxn(J_red, E, 11)
            if delta_red is None: break
            
            new_Z = [Z[0]] + [(Z[j+1] - delta_red[j]) % P for j in range(11)]
            Z = new_Z
        
        if (outer + 1) % 5 == 0:
            elapsed = time.time() - start_time
            print(f"  [{outer+1}/{outer_count}] best={best_total}, z_best={best_z_res}, {elapsed:.0f}s")
    
    elapsed = time.time() - start_time
    return {"found": False, "best_residual": best_total, "elapsed": elapsed}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--outer", type=int, default=100)
    parser.add_argument("--inner", type=int, default=10)
    parser.add_argument("--seed", type=int, default=None)
    parser.add_argument("--resume", action="store_true")
    p_args = parser.parse_args()
    
    result = run_strategy(p_args.outer, p_args.inner, p_args.seed)
    
    if result.get("found"):
        c = result["collision"]
        print(f"\n✅ X={c['x']}")
    else:
        print(f"\n⏹ No collision: {result.get('best_residual')}")
    
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
