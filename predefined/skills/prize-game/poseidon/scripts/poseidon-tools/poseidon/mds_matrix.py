"""
MDS matrix generation and application for Poseidon.

Uses the Cauchy construction:
    MDS[i][j] = modular_inverse(x_i - y_j, prime)
where x_i = i and y_j = t + j.
"""

import random as _random


def generate_mds_matrix(t: int, prime: int) -> list:
    """
    Generate a t×t MDS matrix over GF(prime) using the Cauchy construction.

    The resulting matrix satisfies the MDS (Maximum Distance Separable) property:
    every square sub-matrix is invertible mod prime.

    Args:
        t: State width (matrix dimension).
        prime: Prime field modulus. Must satisfy prime > 2*t.

    Returns:
        A t×t list-of-lists of integers in GF(prime).
    """
    mds = []
    for i in range(t):
        row = []
        for j in range(t):
            # x_i = i, y_j = t + j  →  x_i - y_j = i - t - j  (always != 0 for prime > 2t)
            diff = (i - t - j) % prime
            row.append(pow(diff, -1, prime))
        mds.append(row)
    return mds


def generate_circulant_mds_matrix(first_row: list, prime: int) -> list:
    """
    Generate a t×t circulant MDS matrix from its first row over GF(prime).

    The circulant matrix is defined by:
        MDS[i][j] = first_row[(j - i) % t] % prime

    This construction is used by Plonky3 and the leanSpec Poseidon1 reference.

    Args:
        first_row: First row of the circulant matrix (length t).
        prime: Prime field modulus.

    Returns:
        A t×t list-of-lists of integers in GF(prime).
    """
    t = len(first_row)
    return [
        [first_row[(j - i) % t] % prime for j in range(t)]
        for i in range(t)
    ]


def apply_mds(state: list, mds: list, prime: int) -> list:
    """
    Apply an MDS matrix to a state vector over GF(prime).

    Args:
        state: List of t field elements.
        mds: t×t MDS matrix as list-of-lists.
        prime: Prime field modulus.

    Returns:
        New state after multiplying by MDS matrix mod prime.
    """
    t = len(state)
    return [
        sum(mds[i][j] * state[j] for j in range(t)) % prime
        for i in range(t)
    ]


# ---------------------------------------------------------------------------
# Linear algebra helpers over GF(p)
# ---------------------------------------------------------------------------

def _mat_mul(A, B, p):
    """Multiply two t×t matrices mod p."""
    t = len(A)
    return [
        [sum(A[i][k] * B[k][j] for k in range(t)) % p for j in range(t)]
        for i in range(t)
    ]


def _mat_pow(M, n, p):
    """Compute M^n mod p using repeated squaring."""
    t = len(M)
    result = [[1 if i == j else 0 for j in range(t)] for i in range(t)]
    base = [row[:] for row in M]
    while n:
        if n & 1:
            result = _mat_mul(result, base, p)
        base = _mat_mul(base, base, p)
        n >>= 1
    return result


def _mat_vec_mul(M, v, p):
    """Multiply matrix M by column vector v mod p."""
    t = len(M)
    return [sum(M[i][j] * v[j] for j in range(t)) % p for i in range(t)]


def _is_scalar_multiple_of_identity(M, p):
    """Return True if M == c*I for some scalar c."""
    t = len(M)
    c = M[0][0]
    for i in range(t):
        for j in range(t):
            expected = c if i == j else 0
            if M[i][j] % p != expected % p:
                return False
    return True


def _row_echelon(rows, n_cols, p):
    """
    Perform row reduction (Gaussian elimination) in place on a list of rows
    over GF(p).  Returns (reduced_rows, pivot_cols).
    """
    rows = [row[:] for row in rows]
    pivot_col = 0
    pivot_row = 0
    pivot_cols = []
    n_rows = len(rows)
    while pivot_row < n_rows and pivot_col < n_cols:
        # Find pivot in this column
        found = -1
        for r in range(pivot_row, n_rows):
            if rows[r][pivot_col] % p != 0:
                found = r
                break
        if found == -1:
            pivot_col += 1
            continue
        rows[pivot_row], rows[found] = rows[found], rows[pivot_row]
        inv = pow(rows[pivot_row][pivot_col], -1, p)
        rows[pivot_row] = [(x * inv) % p for x in rows[pivot_row]]
        for r in range(n_rows):
            if r != pivot_row and rows[r][pivot_col] % p != 0:
                factor = rows[r][pivot_col]
                rows[r] = [(rows[r][c] - factor * rows[pivot_row][c]) % p for c in range(n_cols)]
        pivot_cols.append(pivot_col)
        pivot_row += 1
        pivot_col += 1
    return rows, pivot_cols


def _right_null_space(M_rows, n_cols, p):
    """
    Compute the right null space of an n_rows × n_cols matrix over GF(p).
    Returns a list of vectors v (length n_cols) such that M*v = 0.
    If n_rows == 0, returns the full identity basis.
    """
    n_rows = len(M_rows)
    if n_rows == 0:
        return [[1 if i == j else 0 for j in range(n_cols)] for i in range(n_cols)]
    # Build augmented matrix [M^T | I_{n_cols}], shape n_cols × (n_rows + n_cols)
    MT = [[M_rows[r][c] for r in range(n_rows)] for c in range(n_cols)]
    aug = [MT[c] + [1 if c == k else 0 for k in range(n_cols)] for c in range(n_cols)]
    reduced, pivot_cols = _row_echelon(aug, n_rows + n_cols, p)
    # Rows where the M^T part is all-zero give null space vectors
    null_vecs = []
    for row in reduced:
        if all(row[c] % p == 0 for c in range(n_rows)):
            vec = [row[n_rows + k] for k in range(n_cols)]
            if any(x % p != 0 for x in vec):
                null_vecs.append([x % p for x in vec])
    return null_vecs


def _row_space_basis(vecs, p):
    """Return a basis for the row space of vecs over GF(p) via Gaussian elimination."""
    if not vecs:
        return []
    n_cols = len(vecs[0])
    reduced, _ = _row_echelon(vecs, n_cols, p)
    basis = []
    for row in reduced:
        if any(x % p != 0 for x in row):
            basis.append([x % p for x in row])
    return basis


def _is_in_span(v, basis, p):
    """Return True if vector v is in the span of basis vectors."""
    if not basis:
        return all(x % p == 0 for x in v)
    n = len(v)
    rows = [b[:] + [v[i] for i in range(n)] for b in basis]
    # Actually check if augmented system has solution: use rank comparison
    rank_basis = len(_row_space_basis(basis, p))
    rank_aug = len(_row_space_basis([b[:] for b in basis] + [list(v)], p))
    return rank_aug == rank_basis


def _is_same_subspace(basis1, basis2, p):
    """Return True if span(basis1) == span(basis2)."""
    if len(basis1) != len(basis2):
        # Ranks may still match after basis reduction
        pass
    b1 = _row_space_basis(basis1, p)
    b2 = _row_space_basis(basis2, p)
    if len(b1) != len(b2):
        return False
    # Each vector of b1 must be in span(b2) and vice versa
    for v in b1:
        if not _is_in_span(v, b2, p):
            return False
    return True


def _apply_matrix_to_subspace(basis, M, p):
    """Apply matrix M to each basis vector and return the row space of the images."""
    images = [_mat_vec_mul(M, v, p) for v in basis]
    return _row_space_basis(images, p)


def _subspace_intersection(basis1, basis2, t, p):
    """
    Compute the intersection of two subspaces given by their bases.
    Uses the relation: v in span(basis1) ∩ span(basis2)
      ↔ exists x1, x2 s.t. sum(x1[j]*basis1[j]) = sum(x2[j]*basis2[j])
      ↔ [basis1 | -basis2] * [x1; x2] = 0
    """
    k = len(basis1)
    l = len(basis2)
    if k == 0 or l == 0:
        return []
    # Build t × (k+l) matrix C where C[i][j] = basis1[j][i] for j<k, -basis2[j-k][i] for j>=k
    C_rows = []
    for i in range(t):
        row = [basis1[j][i] for j in range(k)] + [(-basis2[j][i]) % p for j in range(l)]
        C_rows.append(row)
    null_vecs = _right_null_space(C_rows, k + l, p)
    inter_vecs = []
    for nv in null_vecs:
        x1 = nv[:k]
        v = [sum(x1[j] * basis1[j][i] for j in range(k)) % p for i in range(t)]
        inter_vecs.append(v)
    return _row_space_basis(inter_vecs, p)


def _generate_vectorspace(round_num, M_powers, t, p):
    """
    Generate the vector space S_i used in Algorithm 1.
    M_powers[i] = M^(i+1).
    round_num=0: full space (identity basis)
    round_num=1: span{e_1, ..., e_{t-1}}
    round_num>=2: right null space of certain rows, extended
    """
    if round_num == 0:
        return [[1 if i == j else 0 for j in range(t)] for i in range(t)]
    if round_num == 1:
        return [[1 if j == i + 1 else 0 for j in range(t)] for i in range(t - 1)]
    # round_num >= 2
    # rows: first row of M^k, dropping first element, for k=1..round_num-1
    rows = [M_powers[k][0][1:] for k in range(round_num - 1)]
    null_vecs = _right_null_space(rows, t - 1, p)
    # Extend each null vector (length t-1) by prepending 0
    extended = [[0] + list(nv) for nv in null_vecs]
    return _row_space_basis(extended, p)


# ---------------------------------------------------------------------------
# Polynomial helpers over GF(p)
# Polynomials represented as lists: poly[i] = coefficient of x^i
# ---------------------------------------------------------------------------

def _poly_trim(f):
    """Remove trailing zero coefficients."""
    f = list(f)
    while len(f) > 1 and f[-1] == 0:
        f.pop()
    return f


def _poly_add(a, b, p):
    n = max(len(a), len(b))
    return _poly_trim([((a[i] if i < len(a) else 0) + (b[i] if i < len(b) else 0)) % p for i in range(n)])


def _poly_sub(a, b, p):
    n = max(len(a), len(b))
    return _poly_trim([((a[i] if i < len(a) else 0) - (b[i] if i < len(b) else 0)) % p for i in range(n)])


def _poly_mul(a, b, p):
    if not a or not b:
        return [0]
    result = [0] * (len(a) + len(b) - 1)
    for i, ai in enumerate(a):
        for j, bj in enumerate(b):
            result[i + j] = (result[i + j] + ai * bj) % p
    return _poly_trim(result)


def _poly_divmod(a, b, p):
    """Polynomial division: returns (quotient, remainder) over GF(p)."""
    a = _poly_trim(a)[:]
    b = _poly_trim(b)
    if len(b) == 0 or (len(b) == 1 and b[0] == 0):
        raise ZeroDivisionError("division by zero polynomial")
    if len(a) < len(b):
        return [0], a
    inv_lead = pow(b[-1], -1, p)
    q = []
    while len(a) >= len(b) and a[-1] != 0:
        coef = (a[-1] * inv_lead) % p
        q.append(coef)
        shift = len(a) - len(b)
        for i, bi in enumerate(b):
            a[shift + i] = (a[shift + i] - coef * bi) % p
        a = _poly_trim(a)
    q.reverse()
    return _poly_trim(q), _poly_trim(a)


def _poly_mod(a, b, p):
    """Return a mod b over GF(p)."""
    _, r = _poly_divmod(a, b, p)
    return r


def _poly_gcd(a, b, p):
    """Polynomial GCD over GF(p), returns monic result."""
    a, b = _poly_trim(a), _poly_trim(b)
    while not (len(b) == 1 and b[0] == 0):
        a, b = b, _poly_mod(a, b, p)
    # Make monic
    if a[-1] != 0:
        inv = pow(a[-1], -1, p)
        a = [(c * inv) % p for c in a]
    return _poly_trim(a)


def _poly_pow_mod(base, exp, mod_poly, p):
    """Compute base^exp mod mod_poly over GF(p) using fast exponentiation."""
    result = [1]  # 1
    base = _poly_mod(base, mod_poly, p)
    while exp:
        if exp & 1:
            result = _poly_mod(_poly_mul(result, base, p), mod_poly, p)
        base = _poly_mod(_poly_mul(base, base, p), mod_poly, p)
        exp >>= 1
    return result


def _char_poly(M, p):
    """
    Compute characteristic polynomial of M over GF(p) using Faddeev-LeVerrier.
    Returns coefficients [c_0, c_1, ..., c_{t-1}, 1] of det(xI - M).
    """
    t = len(M)
    # N starts as M, coefficients c[t-1], c[t-2], ...
    c = [0] * t
    N = [row[:] for row in M]
    # Trace of M
    c[t - 1] = (-sum(N[i][i] for i in range(t))) % p
    for k in range(2, t + 1):
        # N = M * (N + c[t-k+1] * I)
        temp = [[(N[i][j] + (c[t - k + 1] if i == j else 0)) % p for j in range(t)] for i in range(t)]
        N = _mat_mul(M, temp, p)
        c[t - k] = (-sum(N[i][i] for i in range(t)) * pow(k, -1, p)) % p
    return c + [1]


def _poly_is_irreducible(f, p):
    """
    Test irreducibility of polynomial f over GF(p).
    f is given as coefficient list (constant first).
    """
    f = _poly_trim(f)
    n = len(f) - 1  # degree
    if n <= 0:
        return False
    if n == 1:
        return True
    # Make monic copy
    inv_lead = pow(f[-1], -1, p)
    f_monic = [(c * inv_lead) % p for c in f]

    # Compute x^(p^n) mod f and check it equals x
    # x as polynomial: [0, 1]
    x_poly = [0, 1]
    xpn = x_poly[:]
    for _ in range(n):
        xpn = _poly_pow_mod(xpn, p, f_monic, p)
    # x^(p^n) should equal x mod f
    xpn_mod = _poly_mod(xpn, f_monic, p)
    if _poly_trim(xpn_mod) != _poly_trim(x_poly):
        return False

    # For each prime factor q of n, check gcd(f, x^(p^(n//q)) - x) has degree 0
    prime_factors = _prime_factors(n)
    for q in prime_factors:
        exp = n // q
        xpe = x_poly[:]
        for _ in range(exp):
            xpe = _poly_pow_mod(xpe, p, f_monic, p)
        xpe_minus_x = _poly_sub(xpe, x_poly, p)
        g = _poly_gcd(f_monic, xpe_minus_x, p)
        if len(g) - 1 != 0:  # degree > 0
            return False
    return True


def _prime_factors(n):
    """Return the set of distinct prime factors of n."""
    factors = set()
    d = 2
    while d * d <= n:
        if n % d == 0:
            factors.add(d)
            while n % d == 0:
                n //= d
        d += 1
    if n > 1:
        factors.add(n)
    return factors


def _roots_over_gfp(f, p):
    """Find all roots of polynomial f in GF(p)."""
    f = _poly_trim(f)
    if len(f) <= 1:
        return []
    x_poly = [0, 1]
    # g = gcd(f, x^p - x) isolates all linear factors
    xp = _poly_pow_mod(x_poly, p, f, p)
    xp_minus_x = _poly_sub(xp, x_poly, p)
    g = _poly_gcd(f, xp_minus_x, p)
    if len(g) - 1 == 0:
        return []
    return _extract_linear_factors(g, p)


def _extract_linear_factors(f, p):
    """
    Extract all roots from a polynomial f that splits completely over GF(p).
    Uses Cantor-Zassenhaus with deterministic RNG.
    """
    f = _poly_trim(f)
    deg = len(f) - 1
    if deg == 0:
        return []
    if deg == 1:
        # f = c1*x + c0  →  root = -c0 * c1^{-1}
        return [(-f[0] * pow(f[1], -1, p)) % p]
    # Make monic
    inv_lead = pow(f[-1], -1, p)
    f = [(c * inv_lead) % p for c in f]

    # Deterministic seed ensures reproducible factoring across runs.
    # Cantor-Zassenhaus succeeds with probability ~1/2 per attempt for odd prime p;
    # 100 attempts gives a failure probability below 2^{-100}.
    rng = _random.Random(42)
    x_poly = [0, 1]
    for _ in range(100):
        a = rng.randrange(0, p)
        # h = (x + a)^((p-1)//2) mod f
        xa = [(a % p), 1]  # x + a
        exp = (p - 1) // 2
        h = _poly_pow_mod(xa, exp, f, p)
        h_minus_1 = _poly_sub(h, [1], p)
        g = _poly_gcd(f, h_minus_1, p)
        dg = len(g) - 1
        if 0 < dg < deg:
            f_div_g, _ = _poly_divmod(f, g, p)
            return _extract_linear_factors(g, p) + _extract_linear_factors(f_div_g, p)
    raise RuntimeError(  # pragma: no cover
        f"Cantor-Zassenhaus failed to factor polynomial of degree {deg} after 100 attempts"
    )


# ---------------------------------------------------------------------------
# Security check functions
# ---------------------------------------------------------------------------

def _check_minpoly(M, t, p):
    """Check that char poly of M^i is irreducible of degree t for i=1..2t."""
    M_pow = [row[:] for row in M]
    for _ in range(1, 2 * t + 1):
        cp = _char_poly(M_pow, p)
        if not _poly_is_irreducible(cp, p):
            return False
        M_pow = _mat_mul(M_pow, M, p)
    return True


def _algorithm_1(M, t, p):
    """
    Check no subspace trail type 1.
    Returns [True, 0] if check passes, [False, reason] otherwise.
    """
    r = t - 1  # floor((t-1)/1)

    # M_powers[i] = M^(i+1)
    M_powers = []
    Mp = [row[:] for row in M]
    for _ in range(t + 1):
        M_powers.append(Mp)
        Mp = _mat_mul(Mp, M, p)

    for i in range(1, r + 1):
        mat_test = M_powers[i - 1]  # M^i

        if _is_scalar_multiple_of_identity(mat_test, p):
            return [False, 1]

        S = _generate_vectorspace(i, M_powers, t, p)

        cp = _char_poly(mat_test, p)
        eigenvalues = _roots_over_gfp(cp, p)
        int_vecs = []
        for lam in eigenvalues:
            M_minus_lam = [
                [(mat_test[ii][jj] - (lam if ii == jj else 0)) % p for jj in range(t)]
                for ii in range(t)
            ]
            eigenspace = _right_null_space(M_minus_lam, t, p)
            inter = _subspace_intersection(S, eigenspace, t, p)
            int_vecs.extend(inter)
        IS = _row_space_basis(int_vecs, p)

        if 1 <= len(IS) < t:
            return [False, 2]

        for j in range(1, i + 1):
            S_image = _apply_matrix_to_subspace(S, M_powers[j - 1], p)
            if _is_same_subspace(S, S_image, p):
                return [False, 3]

    return [True, 0]


def _algorithm_2(M, t, p):
    """
    Invariant subspace check.
    Returns [True, None] if no invariant subspace, [False, info] otherwise.
    """
    e0 = [1 if j == 0 else 0 for j in range(t)]
    IS = [e0]
    v = e0[:]
    while True:
        delta = len(IS)
        v = _mat_vec_mul(M, v, p)
        IS = _row_space_basis(IS + [v], p)
        if len(IS) == t:
            return [True, None]
        if len(IS) <= delta:
            return [False, [IS, [0]]]


def _algorithm_3(M, t, p):
    """
    Apply algorithm_2 to M^r for r=2..4t.
    Returns [True, None] if all pass, [False, None] otherwise.
    """
    M_pow = _mat_mul(M, M, p)  # M^2
    for r in range(2, 4 * t + 1):
        if not _algorithm_2(M_pow, t, p)[0]:
            return [False, None]
        if r < 4 * t:
            M_pow = _mat_mul(M_pow, M, p)
    return [True, None]


# ---------------------------------------------------------------------------
# Public verification function
# ---------------------------------------------------------------------------

def verify_mds_matrix(mds: list, prime: int) -> bool:
    """
    Verify that an MDS matrix satisfies the Poseidon security criteria.

    Checks:
      1. (Sufficient) Minimal polynomial condition: char poly of M^i is irreducible for i=1..2t
      2. Algorithm 1: no subspace trail type 1
      3. Algorithm 2: no invariant subspace (starting from e_0)
      4. Algorithm 3: algorithm 2 holds for M^r, r=2..4t

    Args:
        mds: t×t MDS matrix as list-of-lists of integers in GF(prime).
        prime: Prime field modulus.

    Returns:
        True if check 1 passes or checks 2-4 pass, False otherwise.
    """
    t = len(mds)
    if _check_minpoly(mds, t, prime):
        return True
    if not _algorithm_1(mds, t, prime)[0]:
        return False
    if not _algorithm_2(mds, t, prime)[0]:
        return False
    if not _algorithm_3(mds, t, prime)[0]:
        return False
    return True
