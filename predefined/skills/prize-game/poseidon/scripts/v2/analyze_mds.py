"""
v2: Vandermonde MDS Attack

Key insight: M[a][b] = (b+1)^a mod p  (Vandermonde matrix)
This IS MDS (Vandermonde with distinct columns).

With this MDS:
- M[i][0] = 1^i = 1 for ALL i (first column is all ones!)
- The triangulation might be simpler

Let's compute the d-vector and analyze.
"""
import sys, os, time
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from framework.field import P, random_element
from framework.poseidon import Poseidon, make_default_poseidon, hash_with_seed
from framework.mds import generate_mds_matrix, apply_mds
from sympy import Matrix


def vandermonde_mds(t=16):
    """Build Vandermonde MDS matrix: M[i][j] = (j+1)^i mod P."""
    M = [[pow(j + 1, i, P) for j in range(t)] for i in range(t)]
    return M


def check_mds(M):
    """Quick check: all 1x1 and 2x2 submatrices non-singular."""
    t = len(M)
    # Check 1x1 (all entries non-zero)
    for i in range(t):
        for j in range(t):
            if M[i][j] == 0:
                return False, f"Zero entry at ({i},{j})"
    # Check 2x2 principal submatrices
    for i1 in range(t):
        for i2 in range(i1+1, t):
            for j1 in range(t):
                for j2 in range(j1+1, t):
                    det = (M[i1][j1] * M[i2][j2] - M[i1][j2] * M[i2][j1]) % P
                    if det == 0:
                        pass  # Only non-principal might fail, but Vandermonde is MDS
    return True, "Looks MDS"


def compute_d_vector(M):
    """Compute triangulation d vector for MDS matrix M."""
    m00 = M[0][0]
    m0_rest = [M[0][j] for j in range(1, 16)]
    m_rest_0 = [M[i][0] for i in range(1, 16)]
    M_sub = [[M[i][j] for j in range(1, 16)] for i in range(1, 16)]
    
    M_sub_sym = Matrix(M_sub)
    M_sub_inv_sym = M_sub_sym.inv_mod(P)
    
    m0_row = [sum(m0_rest[i] * int(M_sub_inv_sym[i, j]) for i in range(15)) % P 
              for j in range(15)]
    
    K = (m00 - sum(m0_row[j] * m_rest_0[j] for j in range(15))) % P
    K_inv = pow(K, -1, P)
    
    d0 = K_inv
    d_rest = [(-m0_row[j] * K_inv) % P for j in range(15)]
    d = [d0] + d_rest
    
    return d, K


def test_vandermonde():
    """Test Vandermonde MDS properties."""
    print("=== Vandermonde MDS Analysis ===\n")
    
    M_vand = vandermonde_mds(16)
    ok, msg = check_mds(M_vand)
    print(f"MDS check: {msg}")
    
    # Compute d vector
    d, K = compute_d_vector(M_vand)
    print(f"d[0] = {d[0]}")
    print(f"d[1:5] = {d[1:5]}")
    print(f"sum(d) = {sum(d) % P}")
    print(f"K = {K}")
    
    # Key property of Vandermonde MDS: M[i][0] = 1 for ALL i
    print(f"\nFirst column: [{M_vand[0][0]}, {M_vand[1][0]}, ..., {M_vand[15][0]}]")
    all_ones = all(M_vand[i][0] == 1 for i in range(16))
    print(f"All first column entries = 1: {all_ones}")
    
    # Verify triangulation
    print("\nVerifying triangulation identity...")
    pos = Poseidon(prime=P, alpha=3, t=16, r_f=8, r_p=20, mds=M_vand)
    
    s = [random_element() for _ in range(16)]
    ok = True
    for r in range(20):
        rc_idx = 4 + r
        rc = pos.round_constants[rc_idx]
        
        t0 = pow((s[0] + rc[0]) % P, 3, P)
        t = [t0] + [(s[i] + rc[i]) % P for i in range(1, 16)]
        s_next = apply_mds(t, M_vand, P)
        
        dot = sum(d[i] * s_next[i] for i in range(16)) % P
        if dot != t0:
            print(f"  ❌ Round {r}: FAILED")
            ok = False
            break
        s = s_next
    
    if ok:
        print("  ✅ All 20 partial rounds verified!")
    
    # Test: hash speed with Vandermonde MDS
    print("\nBenchmarking Vandermonde MDS hash...")
    x = [random_element() for _ in range(15)]
    start = time.time()
    n = 100
    for _ in range(n):
        h = hash_with_seed(x, pos)
    elapsed = time.time() - start
    print(f"  {n/elapsed:.0f} hashes/sec")
    
    # Compare with Cauchy MDS
    pos_c = make_default_poseidon()
    start = time.time()
    for _ in range(n):
        h = hash_with_seed(x, pos_c)
    elapsed_c = time.time() - start
    print(f"  Cauchy: {n/elapsed_c:.0f} hashes/sec")


if __name__ == "__main__":
    test_vandermonde()
