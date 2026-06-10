"""
Grain LFSR implementation for generating Poseidon round constants.

Reference: https://eprint.iacr.org/2019/458
The 80-bit LFSR uses feedback polynomial: x^80 + x^62 + x^51 + x^38 + x^23 + x^13 + 1
"""


class GrainLFSR:
    """
    80-bit Grain LFSR used to generate round constants and MDS elements for Poseidon.

    State is indexed 0..79 where state[0] is the oldest (output) bit.
    Feedback: new_bit = state[62] ^ state[51] ^ state[38] ^ state[23] ^ state[13] ^ state[0]
    After each clock: state = state[1:] + [new_bit]
    """

    def __init__(self, prime_bit_len: int, alpha: int, t: int, r_f: int, r_p: int):
        """
        Initialize GRAIN LFSR with Poseidon parameters.

        Args:
            prime_bit_len: Number of bits in the prime field modulus.
            alpha: S-box exponent. Use -1 for the inverse S-box (x^{-1}).
            t: State width (number of field elements).
            r_f: Number of full rounds.
            r_p: Number of partial rounds.
        """
        self.prime_bit_len = prime_bit_len
        self.alpha = alpha
        self.t = t
        self.r_f = r_f
        self.r_p = r_p

        self.state = [0] * 80
        self._init_state()
        # Discard first 160 output bits to mix state
        for _ in range(160):
            self._clock()

    def _set_bits_msb_first(self, offset: int, value: int, n_bits: int) -> None:
        """Store value in n_bits starting at offset, MSB first."""
        for i in range(n_bits):
            self.state[offset + i] = (value >> (n_bits - 1 - i)) & 1

    def _init_state(self) -> None:
        """Build the 80-bit initial state from Poseidon parameters."""
        # Bits 0-1: field type "10" → prime field
        self.state[0] = 1
        self.state[1] = 0

        # Bit 2: S-box type (0 = x^alpha, 1 = x^{-1})
        self.state[2] = 1 if self.alpha == -1 else 0

        # Bits 3-7: alpha in 5 bits MSB first (0 if alpha == -1)
        alpha_val = 0 if self.alpha == -1 else self.alpha
        self._set_bits_msb_first(3, alpha_val, 5)

        # Bits 8-17: prime bit length in 10 bits MSB first
        self._set_bits_msb_first(8, self.prime_bit_len, 10)

        # Bits 18-27: t in 10 bits MSB first
        self._set_bits_msb_first(18, self.t, 10)

        # Bits 28-37: R_F in 10 bits MSB first
        self._set_bits_msb_first(28, self.r_f, 10)

        # Bits 38-47: R_P in 10 bits MSB first
        self._set_bits_msb_first(38, self.r_p, 10)

        # Bits 48-79: all 1s
        for i in range(48, 80):
            self.state[i] = 1

    def _clock(self) -> int:
        """
        Advance the LFSR by one step and return the output bit (old state[0]).

        The feedback tap positions correspond to the polynomial x^80 + x^62 + x^51 + x^38 + x^23 + x^13 + 1.
        Since the state is shifted left (state[0] is clocked out), taps at positions n
        in the polynomial correspond to state[n-1] before shifting.
        """
        # Feedback: XOR of taps at indices 0, 13, 23, 38, 51, 62 (0-indexed)
        new_bit = (
            self.state[0]
            ^ self.state[13]
            ^ self.state[23]
            ^ self.state[38]
            ^ self.state[51]
            ^ self.state[62]
        )
        output_bit = self.state[0]
        # Shift state left and insert new bit at the end
        self.state = self.state[1:] + [new_bit]
        return output_bit

    def get_next_bit(self) -> int:
        """Return the next output bit from the LFSR."""
        return self._clock()

    def get_field_element(self, prime: int) -> int:
        """
        Return the next field element in GF(prime) using rejection sampling.

        Collects prime_bit_len bits (MSB first), converts to an integer,
        and discards the value if it is >= prime (rejection sampling).
        """
        while True:
            bits = [self._clock() for _ in range(self.prime_bit_len)]
            # bits[0] is the most significant bit
            value = 0
            for b in bits:
                value = (value << 1) | b
            if value < prime:
                return value
