"""
Poseidon hash function — matching the official poseidon-tools implementation.

Reference: https://github.com/khovratovich/poseidon-tools
Paper: https://eprint.iacr.org/2019/458

Bounty instance parameters (from partial_collision_verifier.py):
  p = 2130706433 (KoalaBear), α = 3, t = 16, RF = 8, RP = 20, ell = 16

Known test vectors (KoalaBear, Cauchy MDS):
  >>> pos = make_koala_poseidon()
  >>> pos.hash([1])
  1541345887
  >>> pos.permutation([0] * 16)[0]
  1393439926
"""

from framework.field import P as KOALABEAR_P
from framework.grain_lfsr import GrainLFSR
from framework.mds import generate_mds_matrix, apply_mds


class Poseidon:
    """Poseidon hash function over a prime field GF(prime).

    Uses the sponge construction with the Poseidon permutation.
    """

    def __init__(
        self,
        prime: int = KOALABEAR_P,
        alpha: int = 3,
        t: int = 16,
        r_f: int = 8,
        r_p: int = 20,
        rate: int | None = None,
        mds: list | None = None,
        round_constants: list | None = None,
    ):
        if r_f % 2 != 0:
            raise ValueError("r_f must be even")
        self.prime = prime
        self.alpha = alpha
        self.t = t
        self.r_f = r_f
        self.r_p = r_p
        self.rate = rate if rate is not None else t - 1
        self.capacity = t - self.rate

        total_rounds = r_f + r_p

        if round_constants is not None:
            expected = total_rounds * t
            if len(round_constants) != expected:
                raise ValueError(
                    f"round_constants must have {expected} elements, got {len(round_constants)}"
                )
            self.round_constants = [
                list(round_constants[i * t : (i + 1) * t])
                for i in range(total_rounds)
            ]
        else:
            prime_bit_len = prime.bit_length()
            lfsr = GrainLFSR(prime_bit_len, alpha, t, r_f, r_p)
            self.round_constants = [
                [lfsr.get_field_element(prime) for _ in range(t)]
                for _ in range(total_rounds)
            ]

        self.mds = mds if mds is not None else generate_mds_matrix(t, prime)

    # ── Internal helpers ──────────────────────────────────────────────

    def _sbox(self, x: int) -> int:
        if self.alpha == -1:
            return pow(x, -1, self.prime)
        return pow(x, self.alpha, self.prime)

    def _add_round_constants(self, state: list, constants: list) -> list:
        p = self.prime
        return [(state[i] + constants[i]) % p for i in range(self.t)]

    def _full_round(self, state: list, constants: list) -> list:
        state = self._add_round_constants(state, constants)
        state = [self._sbox(x) for x in state]
        state = apply_mds(state, self.mds, self.prime)
        return state

    def _partial_round(self, state: list, constants: list) -> list:
        state = self._add_round_constants(state, constants)
        state[0] = self._sbox(state[0])
        state = apply_mds(state, self.mds, self.prime)
        return state

    def _permutation_impl(self, state: list, initial_linear: bool = False) -> list:
        if len(state) != self.t:
            raise ValueError(f"State must have {self.t} elements, got {len(state)}")
        state = list(state)
        if initial_linear:
            state = apply_mds(state, self.mds, self.prime)

        half_f = self.r_f // 2
        rc_idx = 0
        for _ in range(half_f):
            state = self._full_round(state, self.round_constants[rc_idx])
            rc_idx += 1
        for _ in range(self.r_p):
            state = self._partial_round(state, self.round_constants[rc_idx])
            rc_idx += 1
        for _ in range(half_f):
            state = self._full_round(state, self.round_constants[rc_idx])
            rc_idx += 1
        return state

    # ── Public API ────────────────────────────────────────────────────

    def permutation(self, state: list) -> list:
        """Apply Poseidon permutation to a state of t field elements."""
        return self._permutation_impl(state)

    def permutation_plus_linear(self, state: list) -> list:
        """Permutation with initial MDS linear layer."""
        return self._permutation_impl(state, initial_linear=True)

    def hash(self, inputs: list) -> int:
        """Hash a list of field elements (sponge, returns single element)."""
        return self.sponge_hash(inputs, 1)[0]

    def hash(self, inputs: list) -> int:
        """Hash a list of field elements (sponge, returns single element).

        This is the convenience method referenced in the docstring examples.
        Equivalent to sponge_hash(inputs, 1)[0].
        """
        return self.sponge_hash(inputs, 1)[0]

    def sponge_hash(self, inputs: list, out_length: int) -> list:
        """Hash using the sponge construction.

        Inputs are absorbed rate elements at a time.
        Returns `out_length` elements from the rate portion.
        """
        if not inputs:
            raise ValueError("inputs must be non-empty")
        if out_length > self.rate:
            raise ValueError(f"out_length cannot exceed rate ({self.rate}), got {out_length}")

        state = [0] * self.t
        state[self.rate] = len(inputs) % self.prime

        for block_start in range(0, len(inputs), self.rate):
            block = inputs[block_start : block_start + self.rate]
            for i, val in enumerate(block):
                state[i] = (state[i] + val) % self.prime
            state = self.permutation(state)
        return state[:out_length]

    def compression_mode_hash(self, inputs: list, out_length: int) -> list:
        """Hash using compression mode (exactly t inputs).

        Puts inputs into state, runs permutation, adds feedforward,
        returns `out_length` elements.

        This is the mode used by the Poseidon bounty.
        """
        if not inputs:
            raise ValueError("inputs must be non-empty")
        if any(not (0 <= x < self.prime) for x in inputs):
            raise ValueError(f"All inputs must be integers in [0, {self.prime - 1}]")
        if out_length > self.t:
            raise ValueError(f"out_length cannot exceed state size ({self.t}), got {out_length}")
        if len(inputs) != self.t:
            raise ValueError(f"input length must be exactly state size ({self.t}), got {len(inputs)}")

        state = [0] * self.t
        for i, val in enumerate(inputs):
            state[i] = val % self.prime

        state = self.permutation(state)

        # Feedforward: add input to output
        for i in range(len(inputs)):
            state[i] = (state[i] + inputs[i]) % self.prime

        return state[:out_length]


# ── Cached Instances (avoid re-initialization overhead) ─────────────


_DEFAULT_POSEIDON: Poseidon | None = None
_PLONKY3_POSEIDON: Poseidon | None = None


def make_default_poseidon() -> Poseidon:
    """Create Poseidon with default bounty parameters (Cauchy MDS, Grain LFSR constants).

    Cached after first call for performance — the Grain LFSR round constant
    generation is expensive (~1ms) but only needs to happen once.
    """
    global _DEFAULT_POSEIDON
    if _DEFAULT_POSEIDON is None:
        _DEFAULT_POSEIDON = Poseidon(
            prime=KOALABEAR_P,
            alpha=3,
            t=16,
            r_f=8,
            r_p=20,
        )
    return _DEFAULT_POSEIDON


def make_plonky3_poseidon() -> Poseidon:
    """Create Poseidon with Plonky3 circulant MDS and pre-computed constants.

    Cached after first call for performance.
    """
    global _PLONKY3_POSEIDON
    if _PLONKY3_POSEIDON is None:
        from framework.mds import PLONKY3_MDS_FIRST_ROW_16, PLONKY3_ROUND_CONSTANTS_16
        from framework.mds import generate_circulant_mds_matrix
        mds = generate_circulant_mds_matrix(PLONKY3_MDS_FIRST_ROW_16, KOALABEAR_P)
        _PLONKY3_POSEIDON = Poseidon(
            prime=KOALABEAR_P,
            alpha=3,
            t=16,
            r_f=8,
            r_p=20,
            mds=mds,
            round_constants=PLONKY3_ROUND_CONSTANTS_16,
        )
    return _PLONKY3_POSEIDON


# ── Collision Helpers (for the bounty challenge) ──────────────────────


def hash_with_seed(
    inputs: list[int],
    pos: Poseidon | None = None,
    out_length: int = 16,
) -> list[int]:
    """Hash 15-element input with SEED prefix (matching the official verifier).

    This is the EXACT function the bounty verifier uses:
        padded = [SEED] + [v % p for v in inputs]  # 16 elements
        return pos.compression_mode_hash(padded, out_length)

    Args:
        inputs: List of 15 field elements (t_perm - 1 = 15).
        pos: Poseidon instance (creates default if None).
        out_length: Number of output elements (default 16).

    Returns:
        List of `out_length` field elements.
    """
    from framework.field import SEED

    if pos is None:
        pos = make_default_poseidon()

    padded = [SEED] + [v % pos.prime for v in inputs]
    return pos.compression_mode_hash(padded, out_length=out_length)


def check_collision(
    x: list[int],
    y: list[int],
    q: int,
    pos: Poseidon | None = None,
) -> bool:
    """Check if two 15-element inputs collide on first q outputs.

    Matches the official verify_collision_solution logic.
    """
    if x == y:
        return False

    hx = hash_with_seed(x, pos, q)
    hy = hash_with_seed(y, pos, q)
    return hx[:q] == hy[:q]


def count_matching_outputs(
    x: list[int],
    y: list[int],
    pos: Poseidon | None = None,
) -> int:
    """Count leading matching output elements between two hashes."""
    if pos is None:
        pos = make_default_poseidon()
    hx = hash_with_seed(x, pos, 16)
    hy = hash_with_seed(y, pos, 16)
    count = 0
    for a, b in zip(hx, hy):
        if a == b:
            count += 1
        else:
            break
    return count
