
from poseidon.poseidon import Poseidon
from bounties.partial_collision_verifier import (
    _hash,
    verify_collision_solution,
    COLLISION_P,
    COLLISION_ALPHA,
    COLLISION_ELL,
    COLLISION_RF,
    COLLISION_RP,
    COLLISION_TPERM,
)

# ---------------------------------------------------------------------------
# Instance parameters
# ---------------------------------------------------------------------------

P      = COLLISION_P        # 2^31 - 2^24 + 1  (KoalaBear)
ALPHA  = COLLISION_ALPHA    # 3
ELL    = COLLISION_ELL      # 16  (hash output words = rate)
T_PERM = COLLISION_TPERM    # 16  (state width)
RF     = COLLISION_RF       # 8
RP     = COLLISION_RP       # 20
T      = 1                  # number of leading output words that must collide
SEED = 0xc09de4

# ---------------------------------------------------------------------------
# T=3 collision verification
# ---------------------------------------------------------------------------

def verify_t3_collision() -> bool:
    """
    Verify a known t=3 partial collision solution.

    By Izan Beltran, Simon-Philipp Merz, Àlex Rodríguez García and Matteo Scarlata

    Checks that the two hardcoded inputs X and Y satisfy
    verify_collision_solution with t=3, RF=8, RP=20, T_PERM=16.

    Both inputs have length t_perm-1=15 (the verifier prepends no seed;
    the caller is responsible for the full preimage vector).

    Returns:
        True if the pair is a valid t=3 collision, False otherwise.
    """
    X = [146101246, 585745660, 1080651781, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    Y = [310195439, 1632272689, 97247552,  0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]

    result = verify_collision_solution(
        X, Y,
        t=3,
        prime=P,
        ell=ELL,
        r_f=RF,
        r_p=RP,
        t_perm=T_PERM,
        alpha=ALPHA,
    )

    padded_X = [SEED]+X
    padded_Y = [SEED]+Y
    pos = Poseidon(prime=P, alpha=ALPHA, t=T_PERM, r_f=RF, r_p=RP)
    hash_x = _hash(X, pos, prime=P, ell=ELL, t_perm=T_PERM)
    hash_y = _hash(Y, pos, prime=P, ell=ELL, t_perm=T_PERM)

    print("T=3 collision verification")
    print(f"  X        = {X}")
    print(f"  Y        = {Y}")
    print(f"  H(X)[:3] = {hash_x[:3]}")
    print(f"  H(Y)[:3] = {hash_y[:3]}")
    print(f"  verify_collision_solution(t=3) -> {result}")
    return result
