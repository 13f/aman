"""
Poseidon Framework — matching the official poseidon-tools implementation.

Reference: https://github.com/khovratovich/poseidon-tools

STABLE: Do not modify. Parameters match the official bounty specification:
  p = 2^31 - 2^24 + 1, α = 3, t = 16, RF = 8, RP = 20
"""

from .field import (
    P, ALPHA, STATE_WIDTH, FULL_ROUNDS, PARTIAL_ROUNDS,
    OUTPUT_LENGTH, INPUT_WIDTH, SEED,
    add, sub, mul, pow_mod, inv, neg, cube, sbox,
    is_valid, random_element, random_state,
)
from .grain_lfsr import GrainLFSR
from .mds import (
    generate_mds_matrix,
    generate_circulant_mds_matrix,
    apply_mds,
    PLONKY3_MDS_FIRST_ROW_16,
    PLONKY3_ROUND_CONSTANTS_16,
)
from .poseidon import (
    Poseidon,
    make_default_poseidon,
    make_plonky3_poseidon,
    hash_with_seed,
    check_collision,
    count_matching_outputs,
)

__all__ = [
    "P", "ALPHA", "STATE_WIDTH", "FULL_ROUNDS", "PARTIAL_ROUNDS",
    "OUTPUT_LENGTH", "INPUT_WIDTH", "SEED",
    "add", "sub", "mul", "pow_mod", "inv", "neg", "cube", "sbox",
    "is_valid", "random_element", "random_state",
    "GrainLFSR",
    "generate_mds_matrix", "generate_circulant_mds_matrix", "apply_mds",
    "PLONKY3_MDS_FIRST_ROW_16", "PLONKY3_ROUND_CONSTANTS_16",
    "Poseidon", "make_default_poseidon", "make_plonky3_poseidon",
    "hash_with_seed", "check_collision", "count_matching_outputs",
]
