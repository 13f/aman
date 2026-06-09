"""
v2 attack: Z3 SMT solver for q=4 collision.

Encode the Poseidon hash as Z3 constraints and use SAT solving
to find two distinct inputs with matching first 4 outputs.

Strategy:
1. Encode the full hash with Z3 bit-vectors (31-bit for p)
2. Add constraint: X ≠ Y, hash(X)[0:4] = hash(Y)[0:4]
3. Use Z3's SAT/SMT engine to find a model

Since Z3 handles bit-vector arithmetic efficiently, this might
find collisions faster than brute force, especially for structured
problems with low-degree equations.
"""
import sys, os, time, json
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from framework.field import P, SEED
from framework.poseidon import make_default_poseidon
from framework.mds import generate_mds_matrix, apply_mds
import z3


def build_z3_model():
    """
    Build Z3 model for q=4 collision problem.
    
    Approach: encode ALL round variables as Z3 bit-vectors,
    add constraints, and let Z3 solve.
    
    Bit width: 31 bits (2^31 > p = 2130706433)
    """
    from framework.poseidon import make_default_poseidon
    pos = make_default_poseidon()
    M = pos.mds
    rc_all = pos.round_constants
    t = 16
    r_f = 8
    r_p = 20
    
    bw = 31  # Bit width for field elements
    
    print("Building Z3 model for Poseidon collision...")
    print(f"  Field: GF({P}), bit width: {bw}")
    print(f"  Rounds: {r_f}f + {r_p}p = {r_f + r_p} total")
    print(f"  State width: {t}")
    
    # Create X and Y inputs (15 elements each after SEED)
    X = [z3.BitVec(f'X_{i}', bw) for i in range(15)]
    Y = [z3.BitVec(f'Y_{i}', bw) for i in range(15)]
    
    # We'll use a simpler approach: encode just ONE hash evaluation
    # and add constraints for both X and Y
    
    # Instead of full bit-vector encoding, let's use a different approach:
    # We'll encode the system using Z3 but with a REDUCED number of rounds.
    # Full 28 rounds would be too slow.
    
    # Let's just verify Z3 works first with a simple test
    solver = z3.Solver()
    
    # Add constraint: X ≠ Y (at least one position differs)
    solver.add(z3.Or([X[i] != Y[i] for i in range(15)]))
    
    # For now, just set up variables and return
    print(f"  Created {30} bit-vector variables")
    
    return solver, X, Y


def verify_z3_basics():
    """Test Z3's finite field arithmetic capabilities."""
    bw = 31
    
    print("Testing Z3 bit-vector arithmetic over GF(P)...\n")
    
    # Test 1: simple cube equation
    x = z3.BitVec('x', bw)
    s = z3.Solver()
    
    # Solve: x³ = 27 mod p
    # x * x * x = 27 (mod p, but Z3 BVMul is unsigned mod 2^bw)
    # For a non-power-of-2 modulus, we need to encode reduction explicitly
    
    # Using Python integers for exact arithmetic
    # We'll use Z3's integer theory instead
    
    print("Test 1: Integer theory for field arithmetic")
    xi = z3.Int('xi')
    si = z3.Solver()
    
    # x³ ≡ 27 (mod p)
    si.add(xi * xi * xi == 27 + z3.IntVal(P) * z3.Int('k'))
    si.add(xi >= 0, xi < P)
    
    if si.check() == z3.sat:
        m = si.model()
        val = m[xi].as_long()
        print(f"  ✅ x³ ≡ 27 (mod {P}): x = {val}")
        print(f"     Verify: {val}³ mod {P} = {pow(val, 3, P)}")
    else:
        print("  ❌ No solution")
    
    # Test 2: More complex system
    print("\nTest 2: Find a, b such that (a+1)³ ≡ (b+2)³ (mod p)")
    a, b = z3.Int('a'), z3.Int('b')
    s2 = z3.Solver()
    
    # (a+1)³ ≡ (b+2)³ (mod p)
    s2.add((a + 1) ** 3 == (b + 2) ** 3 + z3.IntVal(P) * z3.Int('k2'))
    s2.add(a >= 0, a < P, b >= 0, b < P)
    s2.add(a != b)  # Distinct solutions
    
    if s2.check() == z3.sat:
        m = s2.model()
        va = m[a].as_long()
        vb = m[b].as_long()
        print(f"  ✅ a = {va}, b = {vb}")
        print(f"     (a+1)³ mod P = {pow(va+1, 3, P)}")
        print(f"     (b+2)³ mod P = {pow(vb+2, 3, P)}")
    else:
        print("  ❌ No solution")
    
    # Test 3: Simple cubic system from partial rounds
    print("\nTest 3: One partial round equation")
    # (s0 + rc)³ = d · s'  (4 equations for q=4 partial collision)
    # Try to find s0, s1, s2, s3 satisfying some simple constraint
    
    return True


def build_simplified_hash_system():
    """
    Build a simplified system: only the last N partial rounds + 4 back full rounds.
    See if Z3 can solve for collisions.
    """
    from framework.poseidon import make_default_poseidon
    pos = make_default_poseidon()
    M = pos.mds
    rc_all = pos.round_constants
    
    # Get d vector for triangulation
    from sympy import Matrix
    m00 = M[0][0]
    m0_rest = [M[0][j] for j in range(1, 16)]
    m_rest_0_vec = [M[i][0] for i in range(1, 16)]
    M_sub_m = [[M[i][j] for j in range(1, 16)] for i in range(1, 16)]
    M_sub_sym = Matrix(M_sub_m)
    M_sub_inv_sym = M_sub_sym.inv_mod(P)
    
    m0_row = [sum(m0_rest[i] * int(M_sub_inv_sym[i, j]) for i in range(15)) % P for j in range(15)]
    K = (m00 - sum(m0_row[j] * m_rest_0_vec[j] for j in range(15))) % P
    K_inv = pow(K, -1, P)
    d0 = K_inv
    d_rest = [(-m0_row[j] * K_inv) % P for j in range(15)]
    d = [d0] + d_rest
    
    print(f"Triangulation vector d: d[0]={d0}")
    
    # Now encode: after 20 partial rounds, the state satisfies:
    # For each round r (4..23): (s0^(r) + rc0^r)³ = Σ d_j * s_j^(r+1)
    
    # For the collision, we need: two inputs X and Y with same outputs
    
    # Build Z3 integer solver
    s = z3.Solver()
    
    # We'll work with integer theory and encode mod P constraints
    # This allows exact field arithmetic
    
    # Variables: s0 at each partial round (indices 4..23), plus s_rest
    # For simplicity, only model the s0 recurrence through 20 partial rounds
    
    # Actually, let me try a MUCH simpler approach first:
    # Encode ONE partial round with 3 state variables and verify Z3 can solve
    
    from framework.field import random_element
    
    # Pick random state and rc
    state = [random_element() for _ in range(3)] + [0] * 13  # Only 3 active vars
    rc0 = random_element()
    
    # Target: (s0 + rc0)³ = d0*s'0 + d1*s'1 + ... + d15*s'15
    # Compute s' = M * sbox(s)
    t0 = pow((state[0] + rc0) % P, 3, P)
    t = [t0] + [(state[i] + 0) % P for i in range(1, 16)]  # rc_rest = 0 for simplicity
    s_next = [sum(M[i][j] * t[j] for j in range(16)) % P for i in range(16)]
    
    # Verify identity
    dot_verify = sum(d[i] * s_next[i] for i in range(16)) % P
    print(f"\nVerification: (s0+rc0)³ = {t0}, d·s' = {dot_verify}, match={t0==dot_verify}")
    
    # Now use Z3 to find s0 given s' and d
    s0_z3 = z3.Int('s0_z3')
    k_z3 = z3.Int('k_z3')  # Multiple for modular reduction
    
    # (s0_z3 + rc0)³ = d·s' + k_z3 * P
    rhs = sum(d[i] * s_next[i] for i in range(16)) % P
    
    s.add((s0_z3 + rc0) ** 3 == rhs + k_z3 * P)
    s.add(s0_z3 >= 0, s0_z3 < P)
    
    print("Solving for s0 given s'...")
    start = time.time()
    result = s.check()
    elapsed = time.time() - start
    
    if result == z3.sat:
        m = s.model()
        s0_val = m[s0_z3].as_long()
        print(f"  ✅ Found s0 = {s0_val} in {elapsed:.3f}s")
        print(f"     Actual s0 = {state[0]}")
        print(f"     Match: {s0_val == state[0]}")
    else:
        print(f"  ❌ No solution ({elapsed:.3f}s)")
    
    # Test 4: One partial round with s' UNKNOWN (just constraint)
    # Find s0 and s'_rest such that (s0+rc0)³ = d·s'
    print("\nTest 4: Find s0 and s' with partial round constraint")
    s4 = z3.Solver()
    s0_4 = z3.Int('s0_4')
    s1_4 = z3.Int('s1_4')  # s'_1
    k4 = z3.Int('k4')
    
    # (s0_4 + rc0)³ = d0 * s0_4 + d1 * s1_4 + ... (mod P)
    # For simplicity: (s0_4 + rc0)³ = d0 * s0_4 + d1 * s1_4 (only 2 terms)
    # with the rest of d·s' being 0 (set s'_2..s'_15 = 0)
    
    rhs_simple = (d0 * s0_4 + d[1] * s1_4) % P
    
    s4.add((s0_4 + 12345) ** 3 == (d0 * s0_4 + d[1] * s1_4) + k4 * P)
    s4.add(s0_4 >= 0, s0_4 < P, s1_4 >= 0, s1_4 < P)
    
    print("Solving 2-variable cubic system...")
    start = time.time()
    result = s4.check()
    elapsed = time.time() - start
    print(f"  Result: {result} ({elapsed:.3f}s)")
    
    if result == z3.sat:
        m = s4.model()
        v0 = m[s0_4].as_long()
        v1 = m[s1_4].as_long()
        print(f"  s0 = {v0}, s'_1 = {v1}")
        # Verify
        lhs = pow(v0 + 12345, 3, P)
        rhs_v = (d0 * v0 + d[1] * v1) % P
        print(f"  LHS = {lhs}, RHS = {rhs_v}, Match: {lhs == rhs_v}")


if __name__ == "__main__":
    verify_z3_basics()
    build_simplified_hash_system()
