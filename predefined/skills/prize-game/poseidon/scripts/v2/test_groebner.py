"""
Quick Gröbner feasibility test — minimal and focused.
"""
import sys, os, time
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from framework.field import P, random_element


def test_small_groebner():
    """Test SymPy Gröbner on a tiny system over ℤ (mod P reduction later)."""
    from sympy import groebner, symbols
    
    print("=== Minimal Gröbner Test ===\n")
    
    # Test 1: 2 vars, degree 3
    print("Test 1: 2 vars, cubic, over ZZ")
    x, y = symbols('x y')
    eqs = [x**3 + y - 5, x + y**3 - 10]
    
    start = time.time()
    G = groebner(eqs, x, y, order='grevlex')
    elapsed = time.time() - start
    print(f"  {elapsed:.3f}s, basis size: {len(G)}")
    
    # Test 2: 3 vars, triangular cubic system (like our partial round recurrence)
    print("\nTest 2: 3 vars, triangular (recurrence structure)")
    a, b, c = symbols('a b c')
    c0, c1, c2 = 123, 456, 789
    k0, k1 = 111, 222
    
    # Simulates: b = (a+c0)³ + k0, c = (b+c1)³ + k1, a = c (collision)
    eqs2 = [
        (a + c0)**3 - b + k0,
        (b + c1)**3 - c + k1,
        a - c,
    ]
    
    start = time.time()
    G = groebner(eqs2, a, b, c, order='grevlex')
    elapsed = time.time() - start
    print(f"  {elapsed:.3f}s, basis size: {len(G)}")
    for i, poly in enumerate(G):
        print(f"  G[{i}]: {poly}"[:120])
    
    # Test 3: 4 vars, same triangular structure  
    print("\nTest 3: 4 vars, triangular")
    y0, y1, y2, y3 = symbols('y0 y1 y2 y3')
    c = [random.randint(1, 1000) for _ in range(4)]
    k = [random.randint(1, 1000) for _ in range(4)]
    
    eqs3 = [
        (y0 + c[0])**3 - y1 + k[0],
        (y1 + c[1])**3 - y2 + k[1],
        (y2 + c[2])**3 - y3 + k[2],
        y0 - y3 + k[3],
    ]
    
    start = time.time()
    G = groebner(eqs3, y0, y1, y2, y3, order='grevlex')
    elapsed = time.time() - start
    print(f"  {elapsed:.3f}s, basis size: {len(G)}")
    for i, poly in enumerate(G):
        s = str(poly)
        if len(s) > 100:
            s = s[:100] + "..."
        print(f"  G[{i}]: {s}")
    
    # Test 4: 5 vars
    print("\nTest 4: 5 vars, triangular")
    z = symbols('z0 z1 z2 z3 z4')
    c5 = [random.randint(1, 1000) for _ in range(5)]
    k5 = [random.randint(1, 1000) for _ in range(5)]
    
    eqs5 = [
        (z[0] + c5[0])**3 - z[1] + k5[0],
        (z[1] + c5[1])**3 - z[2] + k5[1],
        (z[2] + c5[2])**3 - z[3] + k5[2],
        (z[3] + c5[3])**3 - z[4] + k5[3],
        z[0] - z[4] + k5[4],
    ]
    
    start = time.time()
    G = groebner(eqs5, *z, order='grevlex')
    elapsed = time.time() - start
    print(f"  {elapsed:.3f}s, basis size: {len(G)}")
    
    # Test 5: Triangular with field element reduction
    print("\nTest 5: Same 3-var system over ZZ, then reduce mod P")
    print("  (Groebner over ZZ, then evaluate mod P to find roots)")
    # The Gröbner basis is already computed over ZZ in Test 2
    # Let's just verify the basis
    for i, poly in enumerate(G):
        # Evaluate at random point to verify
        print(f"  G[{i}]: deg={Poly(poly).total_degree()}")


if __name__ == "__main__":
    import random as pyrand
    test_small_groebner()
