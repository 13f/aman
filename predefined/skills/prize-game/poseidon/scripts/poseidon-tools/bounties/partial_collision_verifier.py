"""
Partial collision challenge verifier for the Poseidon Initiative 2026 bounty.

Reference: bounty2026.tex §2.1

The challenge asks for two distinct inputs x ≠ y ∈ F_p^* such that:

  (C1)  x ≠ y                                (distinct inputs)
  (C2)  (a_0, …, a_{ℓ-1}) := H(SEED,x)           (hash x with Poseidon1)
  (C3)  (b_0, …, b_{ℓ-1}) := H(SEED,y)           (hash y with Poseidon1)
  (C4)  a_i = b_i  for i = 0, 1, …, t-1     (first t output words collide)

Hash construction (Poseidon1 sponge, compression mode):
    State width = t_perm field elements, rate = ell field elements.
    Inputs are absorbed rate elements at a time; each block is XOR'd into
    the first `rate` positions of the state, then the permutation is applied.
    The first `ell` elements of the final state are the hash output.

Bounty instance parameters (bounty2026.tex §3.1):
    p      = 2^31 − 2^24 + 1  (KoalaBear prime)
    alpha  = 3                 (S-box exponent)
    ell    = 16                (hash output words)
    R_F    = 8                 (full rounds)
    R_P    = 20                (partial rounds)
    t_perm = 16                (state width = ell; full-state rate)

Best-attack success probability for t partial-collision words (§3.1):
    P(collision in m calls) ≤ m(m-1) / (2 · p^t)
"""

import sys
import os

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from poseidon.poseidon import Poseidon
from poseidon.mds_matrix import verify_mds_matrix, generate_mds_matrix

# ---------------------------------------------------------------------------
# Bounty instance constants
# ---------------------------------------------------------------------------

COLLISION_P      = 2130706433   # 2^31 − 2^24 + 1  (KoalaBear)
COLLISION_ALPHA  = 3            # S-box exponent
COLLISION_ELL    = 16           # hash output words
COLLISION_RF     = 8            # full rounds  (hardcoded per §3.1)
COLLISION_RP     = 20           # partial rounds (hardcoded per §3.1)
COLLISION_TPERM  = 16           # Poseidon1 state width (= ell; full-state rate)

SEED = 0xc09de4
# ---------------------------------------------------------------------------
# Internal helper
# ---------------------------------------------------------------------------

def _hash(
    inputs: list[int],
    pos: Poseidon,
    prime: int,
    ell: int,
    t_perm: int,
) -> list[int]:
    """
    Hash field elements with the given Poseidon instance using compression mode.

    Inputs must be `t_perm-1` long. They are prefixed with 0xc09de4 constant.


    Args:
        inputs: List of field elements (integers mod prime), len = t_perm-1.
        pos:    Pre-built Poseidon instance.
        prime:  Field modulus.
        ell:    Number of output words to return.
        t_perm: State width (= required input length for compression_mode_hash).

    Returns:
        List of ell integers — the first ell elements of the compressed output.
    """
    padded = ([SEED]+ [v % prime for v in inputs] )[:t_perm]
    return pos.compression_mode_hash(padded, out_length=ell)


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

def verify_collision_solution(
    x: list[int],
    y: list[int],
    t: int,
    prime: int = COLLISION_P,
    ell: int = COLLISION_ELL,
    r_f: int = COLLISION_RF,
    r_p: int = COLLISION_RP,
    t_perm: int = COLLISION_TPERM,
    alpha: int = COLLISION_ALPHA,
    mds: list | None = None,
    round_constants: list | None = None,
) -> bool:
    """
    Verify an alleged solution to the Poseidon partial collision challenge.

    Args:
        x:               First input vector (non-empty list of t_perm-1 field elements).
        y:               Second input vector (non-empty list of t_perm-1 field elements).
        t:               Number of leading output words that must collide (1 ≤ t ≤ ell).
        prime:           Field modulus p.
        ell:             Number of hash output words (= Poseidon rate).
        r_f:             Number of Poseidon full rounds.
        r_p:             Number of Poseidon partial rounds.
        t_perm:          Poseidon state width in field elements.
        alpha:           S-box exponent.
        mds:             Optional custom MDS matrix (t_perm × t_perm).
        round_constants: Optional flat list of (r_f + r_p)·t_perm pre-computed
                         round constants.

    Returns:
        True iff all four conditions (C1–C4) are satisfied.

    Raises:
        ValueError: if t is out of range, inputs are empty, or ell > t_perm.
    """
    if not x:
        raise ValueError("x must be non-empty")
    if not y:
        raise ValueError("y must be non-empty")
    if not (1 <= t <= ell):
        raise ValueError(f"t={t} must satisfy 1 <= t <= ell={ell}")
    if ell > t_perm:
        raise ValueError(f"ell={ell} must not exceed t_perm={t_perm}")
    if len(x) != t_perm - 1:
        raise ValueError(f"x must have length t_perm-1={t_perm - 1}, got {len(x)}")
    if len(y) != t_perm - 1:
        raise ValueError(f"y must have length t_perm-1={t_perm - 1}, got {len(y)}")
    p = prime

    # ------------------------------------------------------------------
    # C1: x ≠ y  (distinct as field-element vectors; compare mod p)
    # ------------------------------------------------------------------
    x_norm = [v % p for v in x]
    y_norm = [v % p for v in y]
    if x_norm == y_norm:
        return False

    # ------------------------------------------------------------------
    # C2 & C3: hash both inputs with Poseidon1
    # ------------------------------------------------------------------
    _mds = mds if mds is not None else generate_mds_matrix(t_perm, p)
    if not verify_mds_matrix(_mds, p):
        return False

    pos = Poseidon(
        prime=p,
        alpha=alpha,
        t=t_perm,
        r_f=r_f,
        r_p=r_p,
        mds=_mds,
        round_constants=round_constants,
    )

    hash_x = _hash(x_norm, pos, p, ell, t_perm)  # (a_0, …, a_{ℓ-1})
    hash_y = _hash(y_norm, pos, p, ell, t_perm)  # (b_0, …, b_{ℓ-1})

    # ------------------------------------------------------------------
    # C4: first t output words must match
    # ------------------------------------------------------------------
    return hash_x[:t] == hash_y[:t]


