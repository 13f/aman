"""
Test the key mathematical insight:
- Linear elimination through partial rounds
- Verify the triangulation identity: (s0+rc0)³ = d · s'(next)
"""
import sys, os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from framework.field import P, random_element
from framework.mds import generate_mds_matrix
from framework.poseidon import make_default_poseidon
from sympy import Matrix

import numpy as np


def compute_d_vector(M):
    """
    Compute vector d such that for any partial round:
    (s0 + rc0)³ ≡ Σ d_j * s'_j  (mod P)
    
    where s' = M × t,  t = [(s0+rc0)³; s1+rc1; ...; s15+rc15]
    """
    m00 = M[0][0]
    m0_rest = [M[0][j] for j in range(1, 16)]
    m_rest_0 = [M[i][0] for i in range(1, 16)]
    M_sub = [[M[i][j] for j in range(1, 16)] for i in range(1, 16)]
    
    # Compute M_sub_inv mod P
    M_sub_sym = Matrix(M_sub)
    M_sub_inv_sym = M_sub_sym.inv_mod(P)
    M_sub_inv = [[M_sub_inv_sym[i, j] for j in range(15)] for i in range(15)]
    
    # K = m00 - m0_rest @ M_sub_inv @ m_rest_0
    # m0_rest @ M_sub_inv = 1×15 row vector
    m0_row = [sum(m0_rest[i] * M_sub_inv[i][j] for i in range(15)) % P for j in range(15)]
    
    # m0_row @ m_rest_0 = scalar
    m0_dot = sum(m0_row[j] * m_rest_0[j] for j in range(15)) % P
    K = (m00 - m0_dot) % P
    
    K_inv = pow(K, -1, P)
    d0 = K_inv
    d_rest = [(-m0_row[j] * K_inv) % P for j in range(15)]
    d = [d0] + d_rest
    
    return d, K


def test_identity():
    """Verify the identity (s0+rc0)³ = d · s'(next) for Cauchy MDS."""
    pos = make_default_poseidon()
    M = pos.mds
    
    print("=== Partial Round Triangulation Test ===\n")
    d, K = compute_d_vector(M)
    print(f"d vector (first 5): {d[:5]}...")
    print(f"K = {K}")
    print(f"sum(d) = {sum(d) % P}")
    
    # Test with multiple random states
    print("\nVerifying identity on 100 random states...")
    for trial in range(100):
        s = [random_element() for _ in range(16)]
        rc = [random_element() for _ in range(16)]
        
        # Forward partial round
        t0 = pow((s[0] + rc[0]) % P, 3, P)
        t = [t0] + [(s[i] + rc[i]) % P for i in range(1, 16)]
        s_next = [sum(M[i][j] * t[j] for j in range(16)) % P for i in range(16)]
        
        # d · s_next
        dot = sum(d[i] * s_next[i] for i in range(16)) % P
        
        if dot != t0:
            print(f"  ❌ Trial {trial}: dot={dot} ≠ t0={t0}")
            return False
    
    print("  ✅ All 100 trials passed!")
    
    # Also test: can we recover s from s_next?
    print("\nTesting inversion (M_sub invertibility)...")
    M_sub = [[M[i][j] for j in range(1, 16)] for i in range(1, 16)]
    M_sub_sym = Matrix(M_sub)
    det = M_sub_sym.det() % P
    print(f"  det(M_sub) = {det}  (non-zero ✓)" if det != 0 else f"  det(M_sub) = 0  ❌")
    
    return True


if __name__ == "__main__":
    test_identity()
