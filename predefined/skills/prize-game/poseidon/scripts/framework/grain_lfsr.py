"""
Grain LFSR for Poseidon round constant generation.

Reference: https://eprint.iacr.org/2019/458
The 80-bit LFSR uses feedback polynomial:
  x^80 + x^62 + x^51 + x^38 + x^23 + x^13 + 1

From the official poseidon-tools implementation:
  https://github.com/khovratovich/poseidon-tools
"""


class GrainLFSR:
    """80-bit Grain LFSR for generating Poseidon round constants.

    State is indexed 0..79 where state[0] is the oldest (output) bit.
    Feedback: new_bit = state[62] ^ state[51] ^ state[38] ^ state[23] ^ state[13] ^ state[0]
    """

    def __init__(self, prime_bit_len: int, alpha: int, t: int, r_f: int, r_p: int):
        self.prime_bit_len = prime_bit_len
        self.alpha = alpha
        self.t = t
        self.r_f = r_f
        self.r_p = r_p
        self.state = [0] * 80
        self._init_state()
        for _ in range(160):
            self._clock()

    def _set_bits_msb_first(self, offset: int, value: int, n_bits: int) -> None:
        for i in range(n_bits):
            self.state[offset + i] = (value >> (n_bits - 1 - i)) & 1

    def _init_state(self) -> None:
        self.state[0] = 1
        self.state[1] = 0
        self.state[2] = 1 if self.alpha == -1 else 0
        alpha_val = 0 if self.alpha == -1 else self.alpha
        self._set_bits_msb_first(3, alpha_val, 5)
        self._set_bits_msb_first(8, self.prime_bit_len, 10)
        self._set_bits_msb_first(18, self.t, 10)
        self._set_bits_msb_first(28, self.r_f, 10)
        self._set_bits_msb_first(38, self.r_p, 10)
        for i in range(48, 80):
            self.state[i] = 1

    def _clock(self) -> int:
        new_bit = (
            self.state[0]
            ^ self.state[13]
            ^ self.state[23]
            ^ self.state[38]
            ^ self.state[51]
            ^ self.state[62]
        )
        output_bit = self.state[0]
        self.state = self.state[1:] + [new_bit]
        return output_bit

    def get_field_element(self, prime: int) -> int:
        """Return next field element using rejection sampling."""
        while True:
            bits = [self._clock() for _ in range(self.prime_bit_len)]
            value = 0
            for b in bits:
                value = (value << 1) | b
            if value < prime:
                return value
