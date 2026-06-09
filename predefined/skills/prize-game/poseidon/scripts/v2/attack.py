"""
v2 attack: Newton's method over GF(P) for q=4 collision.

Given X, find Y ≠ X such that hash(Y)[0:4] = hash(X)[0:4].
Fix Y₅..Y₁₄ = X₅..X₁₄, vary only first 4 positions.
This gives 4 equations in 4 unknowns.

Use Newton's method with numerical Jacobian.
"""
import sys, os, time, json, random as pyrand
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from framework.field import P, SEED, random_element
import numpy as np
from numba import njit


# ── Build ─────────────────────────────────────────────────────────────

def build_vandermonde_mds(t=16):
    M = np.zeros((t, t), dtype=np.int64)
    for i in range(t):
        for j in range(t):
            M[i, j] = pow(j + 1, i, P)
    return M

def build_round_constants():
    from framework.poseidon import make_default_poseidon
    pos = make_default_poseidon()
    total = pos.r_f + pos.r_p
    rc = np.zeros((total, 16), dtype=np.int64)
    for r in range(total):
        for i in range(16):
            rc[r, i] = pos.round_constants[r][i]
    return rc

P_I64 = np.int64(P)

@njit(cache=True)
def modinv(x, p):
    """Modular inverse via Fermat: x^(p-2) mod p (binary exponentiation)."""
    result = np.int64(1)
    base = x % p
    exp = p - 2
    while exp > 0:
        if exp & 1:
            result = (result * base) % p
        base = (base * base) % p
        exp >>= 1
    return result

@njit(cache=True)
def cube_mod(x, p):
    return (x * x % p) * x % p

@njit(cache=True)
def mat_mul(state, M, p):
    t = len(state)
    res = np.zeros(t, dtype=np.int64)
    for i in range(t):
        s = np.int64(0)
        for j in range(t):
            s = (s + M[i, j] * state[j]) % p
        res[i] = s
    return res

@njit(cache=True)
def poseidon_permutation(state, M, rc, p):
    t = len(state)
    s = state.copy()
    r_f = 8; r_p = 20
    half_f = r_f // 2
    idx = 0
    for _ in range(half_f):
        for i in range(t):
            s[i] = (s[i] + rc[idx, i]) % p
        for i in range(t):
            s[i] = cube_mod(s[i], p)
        s = mat_mul(s, M, p)
        idx += 1
    for _ in range(r_p):
        for i in range(t):
            s[i] = (s[i] + rc[idx, i]) % p
        s[0] = cube_mod(s[0], p)
        s = mat_mul(s, M, p)
        idx += 1
    for _ in range(half_f):
        for i in range(t):
            s[i] = (s[i] + rc[idx, i]) % p
        for i in range(t):
            s[i] = cube_mod(s[i], p)
        s = mat_mul(s, M, p)
        idx += 1
    return s

@njit(cache=True)
def hash_with_seed(inputs, M, rc):
    t = 16
    p = P_I64
    state = np.zeros(t, dtype=np.int64)
    state[0] = SEED % p
    for i in range(1, t):
        state[i] = inputs[i-1] % p
    orig = state.copy()
    perm = poseidon_permutation(state, M, rc, p)
    out = np.zeros(t, dtype=np.int64)
    for i in range(t):
        out[i] = (perm[i] + orig[i]) % p
    return out

@njit(cache=True)
def make_y(fixed_part, y1, y2, y3, y4):
    """Build Y = [y1, y2, y3, y4, fixed[4..14]]."""
    Y = fixed_part.copy()
    Y[0] = y1 % P
    Y[1] = y2 % P
    Y[2] = y3 % P
    Y[3] = y4 % P
    return Y

@njit(cache=True)
def F_func(y1, y2, y3, y4, fixed_part, target, M, rc):
    """
    F(y) = hash(Y)[0:4] - target, where Y has first 4 elements = (y1,y2,y3,y4)
    and last 11 from fixed_part.
    """
    Y = make_y(fixed_part, y1, y2, y3, y4)
    h = hash_with_seed(Y, M, rc)
    return np.array([
        (h[0] - target[0]) % P,
        (h[1] - target[1]) % P,
        (h[2] - target[2]) % P,
        (h[3] - target[3]) % P,
    ], dtype=np.int64)


@njit(cache=True)
def jacobian(y1, y2, y3, y4, fixed_part, target, M, rc, eps=1):
    """Numerical Jacobian using finite differences."""
    p = P_I64
    F0 = F_func(y1, y2, y3, y4, fixed_part, target, M, rc)
    J = np.zeros((4, 4), dtype=np.int64)
    
    # Perturb each variable by eps
    for j in range(4):
        dy1, dy2, dy3, dy4 = y1, y2, y3, y4
        if j == 0: dy1 = (y1 + eps) % p
        if j == 1: dy2 = (y2 + eps) % p
        if j == 2: dy3 = (y3 + eps) % p
        if j == 3: dy4 = (y4 + eps) % p
        
        F1 = F_func(dy1, dy2, dy3, dy4, fixed_part, target, M, rc)
        eps_inv = modinv(eps % p, p)
        
        for i in range(4):
            diff = (F1[i] - F0[i]) % p
            J[i, j] = (diff * eps_inv) % p
    
    return J, F0


@njit(cache=True)
def mat_inv_4x4(A):
    """Invert 4x4 matrix mod P using Cramer's rule."""
    p = P_I64
    det = (
        A[0,0] * (A[1,1] * (A[2,2]*A[3,3] - A[2,3]*A[3,2]) - A[1,2] * (A[2,1]*A[3,3] - A[2,3]*A[3,1]) + A[1,3] * (A[2,1]*A[3,2] - A[2,2]*A[3,1])) -
        A[0,1] * (A[1,0] * (A[2,2]*A[3,3] - A[2,3]*A[3,2]) - A[1,2] * (A[2,0]*A[3,3] - A[2,3]*A[3,0]) + A[1,3] * (A[2,0]*A[3,2] - A[2,2]*A[3,0])) +
        A[0,2] * (A[1,0] * (A[2,1]*A[3,3] - A[2,3]*A[3,1]) - A[1,1] * (A[2,0]*A[3,3] - A[2,3]*A[3,0]) + A[1,3] * (A[2,0]*A[3,1] - A[2,1]*A[3,0])) -
        A[0,3] * (A[1,0] * (A[2,1]*A[3,2] - A[2,2]*A[3,1]) - A[1,1] * (A[2,0]*A[3,2] - A[2,2]*A[3,0]) + A[1,2] * (A[2,0]*A[3,1] - A[2,1]*A[3,0]))
    ) % p
    
    if det == 0:
        return None, 0  # Singular
    
    det_inv = modinv(det, p)
    
    # Compute cofactor matrix
    # This is large but straightforward
    inv = np.zeros((4,4), dtype=np.int64)
    for i in range(4):
        for j in range(4):
            # Minor (3x3 determinant)
            rows = [r for r in range(4) if r != i]
            cols = [c for c in range(4) if c != j]
            minor = (
                A[rows[0], cols[0]] * (A[rows[1], cols[1]]*A[rows[2], cols[2]] - A[rows[1], cols[2]]*A[rows[2], cols[1]]) -
                A[rows[0], cols[1]] * (A[rows[1], cols[0]]*A[rows[2], cols[2]] - A[rows[1], cols[2]]*A[rows[2], cols[0]]) +
                A[rows[0], cols[2]] * (A[rows[1], cols[0]]*A[rows[2], cols[1]] - A[rows[1], cols[1]]*A[rows[2], cols[0]])
            ) % p
            cofactor = minor if (i + j) % 2 == 0 else (p - minor) % p
            inv[j, i] = (cofactor * det_inv) % p  # Transpose
    
    return inv, det


@njit(cache=True)
def newton_step(y1, y2, y3, y4, fixed_part, target, M, rc):
    """One Newton step: y_{k+1} = y_k - J^{-1} * F(y_k)."""
    J, F = jacobian(y1, y2, y3, y4, fixed_part, target, M, rc, eps=1)
    Jinv, det = mat_inv_4x4(J)
    
    if Jinv is None:
        return (y1, y2, y3, y4, F, False, "singular")
    
    # Compute J^{-1} * F
    correction = np.zeros(4, dtype=np.int64)
    for i in range(4):
        s = np.int64(0)
        for j in range(4):
            s = (s + Jinv[i, j] * F[j]) % P
        correction[i] = s
    
    ny1 = (y1 - correction[0]) % P
    ny2 = (y2 - correction[1]) % P
    ny3 = (y3 - correction[2]) % P
    ny4 = (y4 - correction[3]) % P
    
    return (ny1, ny2, ny3, ny4, F, True, "ok")


def newton_search(X, M, rc, max_iters=50, tol=10, verbose=True):
    target = hash_with_seed(X, M, rc)[:4]
    fixed_part = X.copy()
    
    # Start from a random perturbation
    y = (
        (X[0] + random_element()) % P,
        (X[1] + random_element()) % P,
        (X[2] + random_element()) % P,
        (X[3] + random_element()) % P,
    )
    
    if verbose:
        print(f"Target: {target}")
        print(f"Starting F(y): {F_func(*y, fixed_part, target, M, rc)}")
    
    for it in range(max_iters):
        ny1, ny2, ny3, ny4, F, success, msg = newton_step(
            y[0], y[1], y[2], y[3], fixed_part, target, M, rc
        )
        
        fnorm = sum(f * f for f in F) % P
        
        if verbose:
            print(f"  iter {it}: |F|={fnorm}, {msg}")
        
        if fnorm <= tol:
            Y = make_y(fixed_part, ny1, ny2, ny3, ny4)
            h = hash_with_seed(Y, M, rc)
            match_count = 0
            for i in range(4):
                if h[i] == target[i]:
                    match_count += 1
                else:
                    break
            
            if match_count >= 4 and not np.array_equal(Y, X):
                if verbose:
                    print(f"\n✅ q=4 collision found in {it+1} iterations!")
                return {
                    "found": True,
                    "iterations": it+1,
                    "x": X.tolist(),
                    "y": Y.tolist(),
                    "hash_x": hash_with_seed(X, M, rc).tolist()[:4],
                    "hash_y": h.tolist()[:4],
                }
        
        if not success:
            if verbose:
                print(f"  Singular Jacobian — random jump")
            ny1 = (ny1 + pyrand.randint(1, P-1)) % P
            ny2 = (ny2 + pyrand.randint(1, P-1)) % P
            ny3 = (ny3 + pyrand.randint(1, P-1)) % P
            ny4 = (ny4 + pyrand.randint(1, P-1)) % P
        
        y = (ny1, ny2, ny3, ny4)
    
    return {"found": False, "iterations": max_iters}


def run_trials(num_trials=5):
    """Run multiple Newton trials from different starting points."""
    M = build_vandermonde_mds(16)
    rc = build_round_constants()
    
    rng = np.random.default_rng()
    
    for trial in range(num_trials):
        print(f"\n{'='*60}")
        print(f"Trial {trial+1}/{num_trials}")
        print(f"{'='*60}")
        
        X = rng.integers(0, P, 15, dtype=np.int64)
        
        result = newton_search(X, M, rc, max_iters=50, verbose=True)
        
        if result.get("found"):
            return result
    
    return {"found": False, "trials": num_trials}


if __name__ == "__main__":
    import random as pyrand
    result = run_trials(5)
    print(f"\nFinal: {json.dumps(result, indent=2, default=str)}")
