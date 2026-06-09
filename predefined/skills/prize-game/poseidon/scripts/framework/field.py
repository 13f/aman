"""
KoalaBear finite field arithmetic for Poseidon.

Field: 𝔽_p where p = 2^31 - 2^24 + 1 = 2130706433

Parameters match the official Poseidon Initiative bounty spec:
  - p = 2^31 - 2^24 + 1  (KoalaBear prime)
  - S-Box exponent α = 3
  - State width t = 16
  - Full rounds RF = 8
  - Partial rounds RP = 20
  - Hash output length ell = 16

Reference: https://github.com/khovratovich/poseidon-tools
"""

import random

# The KoalaBear prime
P: int = 2130706433  # 2^31 - 2^24 + 1

# Poseidon bounty instance parameters (matching official verifier)
ALPHA: int = 3
STATE_WIDTH: int = 16  # t_perm
FULL_ROUNDS: int = 8    # R_F
PARTIAL_ROUNDS: int = 20  # R_P
OUTPUT_LENGTH: int = 16  # ell
INPUT_WIDTH: int = 15    # t_perm - 1 (for compression mode with SEED prefix)

# Domain-separation seed (matching official verifier)
SEED: int = 0xC09DE4


def add(a: int, b: int) -> int:
    """Addition in 𝔽_p (optimized: no modulo if sum < p)."""
    s = a + b
    return s if s < P else s - P


def sub(a: int, b: int) -> int:
    """Subtraction in 𝔽_p."""
    d = a - b
    return d + P if d < 0 else d


def mul(a: int, b: int) -> int:
    """Multiplication in 𝔽_p."""
    return (a * b) % P


def pow_mod(base: int, exp: int) -> int:
    """Fast exponentiation in 𝔽_p."""
    return pow(base, exp, P)


def inv(a: int) -> int:
    """Multiplicative inverse in 𝔽_p (Fermat: a^(p-2))."""
    if a % P == 0:
        raise ZeroDivisionError("Cannot invert 0 in field")
    return pow(a, P - 2, P)


def neg(a: int) -> int:
    """Additive inverse in 𝔽_p."""
    return P - (a % P) if a % P != 0 else 0


def cube(x: int) -> int:
    """S-Box: x → x^3 in 𝔽_p."""
    return pow(x, 3, P)


def sbox(x: int, alpha: int = 3) -> int:
    """S-Box: x → x^α in 𝔽_p (α=3 for Poseidon)."""
    if alpha == -1:
        return pow(x, -1, P)
    return pow(x, alpha, P)


def is_valid(x: int) -> bool:
    """Check if x is a valid field element (in [0, p-1])."""
    return isinstance(x, int) and 0 <= x < P


def random_element() -> int:
    """Generate a random field element."""
    return random.randrange(0, P)


def random_state(size: int = 16) -> list[int]:
    """Generate a random state vector of `size` field elements."""
    return [random_element() for _ in range(size)]


# Field size for birthday-bound calculations
FIELD_SIZE: int = P
ZERO: int = 0
ONE: int = 1
