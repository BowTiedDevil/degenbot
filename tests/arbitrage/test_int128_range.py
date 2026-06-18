"""Tests for int128 range validation in V4 swap encoding.

V4's BalanceDelta packs two int128 values (amount0, amount1).
If amountSpecified or swap output exceeds int128 range (-2^127 to 2^127-1),
V4's SafeCast.toInt128() reverts with SafeCastOverflow (0x93dafdf1).

The fits_int128() helper and INT128_MAX/INT128_MIN constants allow
encoders to skip paths that would overflow before submitting them on-chain.
"""


from degenbot.arbitrage.encoding import INT128_MAX, INT128_MIN, fits_int128


class TestInt128Range:
    """int128 range validation for V4 swap amounts."""

    def test_int128_max_is_2_127_minus_1(self) -> None:
        assert INT128_MAX == (1 << 127) - 1

    def test_int128_min_is_neg_2_127(self) -> None:
        assert INT128_MIN == -(1 << 127)

    def test_zero_fits(self) -> None:
        assert fits_int128(0) is True

    def test_one_fits(self) -> None:
        assert fits_int128(1) is True

    def test_neg_one_fits(self) -> None:
        assert fits_int128(-1) is True

    def test_max_fits(self) -> None:
        assert fits_int128(INT128_MAX) is True

    def test_min_fits(self) -> None:
        assert fits_int128(INT128_MIN) is True

    def test_max_plus_one_does_not_fit(self) -> None:
        assert fits_int128(INT128_MAX + 1) is False

    def test_min_minus_one_does_not_fit(self) -> None:
        assert fits_int128(INT128_MIN - 1) is False

    def test_large_weth_amount_fits(self) -> None:
        """10,000 WETH (18 decimals) fits comfortably in int128."""
        assert fits_int128(10_000 * 10**18) is True

    def test_absurdly_large_amount_does_not_fit(self) -> None:
        """An absurdly large token amount (2^200) does not fit."""
        assert fits_int128(1 << 200) is False

    def test_v4_exact_input_amount_specified(self) -> None:
        """V4 exact-input uses negative amountSpecified.
        -optimal_input must fit in int128 for V4 to accept it.
        """
        optimal_input = 1 * 10**18  # 1 WETH
        amount_specified = -optimal_input
        assert fits_int128(amount_specified) is True

    def test_v4_huge_exact_input_fails(self) -> None:
        """If optimal_input exceeds int128, the negative amountSpecified underflows."""
        huge_input = (1 << 127) + 100
        amount_specified = -huge_input
        assert fits_int128(amount_specified) is False
