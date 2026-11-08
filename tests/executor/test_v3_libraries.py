"""
Tests for the Vyper port of UniswapV3 library contracts.

Validates that the Vyper implementations produce the same results as the
original Solidity, using reference values obtained from deploying the real
Solidity FullMath on a local Anvil instance.
"""

import pytest


# ── Fixtures ──

@pytest.fixture
def harness(project, owner_account):
    """Deploy the test harness that exposes library functions."""
    return project.test_harness.deploy(sender=owner_account)


# ═══════════════════════════════════════════════════════════════════════════
# FullMath
# ═══════════════════════════════════════════════════════════════════════════


class TestFullMath:
    """Test Vyper FullMath.mul_div and mul_div_rounding_up against Solidity reference."""

    # Reference values from Solidity FullMath (v3-core) deployed on Anvil
    # Computed via: cast call $ADDR "mul_div(uint256,uint256,uint256)(uint256)" a b d

    def test_mul_div_simple(self, harness):
        """mul_div(7, 11, 3) = 25"""
        assert harness.test_mul_div(7, 11, 3) == 25

    def test_mul_div_simple2(self, harness):
        """mul_div(5, 7, 2) = 17"""
        assert harness.test_mul_div(5, 7, 2) == 17

    def test_mul_div_exact(self, harness):
        """Exact division: mul_div(6, 7, 2) = 21"""
        assert harness.test_mul_div(6, 7, 2) == 21

    def test_mul_div_truncation(self, harness):
        """Truncation: mul_div(7, 11, 3) = 25 (77/3 = 25.666...→ 25)"""
        assert harness.test_mul_div(7, 11, 3) == 25

    def test_mul_div_phantom_overflow_u128(self, harness):
        """Phantom overflow: (2^128-1) * (2^128-1) / (2^128-1) = 2^128-1.

        a*b overflows 256 bits (prod1 > 0) but result fits.
        """
        u128_max = (1 << 128) - 1
        assert harness.test_mul_div(u128_max, u128_max, u128_max) == u128_max

    def test_mul_div_phantom_overflow_pow2(self, harness):
        """Phantom overflow: 2^128 * 2^128 / 2^128 = 2^128."""
        u128 = 1 << 128
        assert harness.test_mul_div(u128, u128, u128) == u128

    def test_mul_div_phantom_overflow_near_max(self, harness):
        """Phantom overflow: (2^128+1) * 3 / 2^128 = 3."""
        u128 = 1 << 128
        assert harness.test_mul_div(u128 + 1, 3, u128) == 3

    def test_mul_div_phantom_overflow_u255(self, harness):
        """Phantom overflow: (2^255+1) * 3 / 2^255 = 3."""
        u255 = 1 << 255
        assert harness.test_mul_div(u255 + 1, 3, u255) == 3

    def test_mul_div_one_max_one(self, harness):
        """1 * max / 1 = max."""
        u256_max = (1 << 256) - 1
        assert harness.test_mul_div(1, u256_max, 1) == u256_max

    def test_mul_div_denominator_one(self, harness):
        """Large a * b / 1 = a * b when no overflow."""
        a = (1 << 128) - 1
        b = 1
        assert harness.test_mul_div(a, b, 1) == a

    def test_mul_div_zero_numerator(self, harness):
        """0 * anything / anything = 0."""
        assert harness.test_mul_div(0, 12345, 7) == 0

    def test_mul_div_zero_denominator_reverts(self, harness):
        """Division by zero should revert."""
        with pytest.raises(Exception):
            harness.test_mul_div(1, 1, 0)

    def test_mul_div_rounding_up_simple(self, harness):
        """mulDivRoundingUp(7, 11, 3) = 26 (77/3 = 25.666...→ 26)."""
        assert harness.test_mul_div_rounding_up(7, 11, 3) == 26

    def test_mul_div_rounding_up_exact(self, harness):
        """Exact division, rounding up = same as floor."""
        u128_max = (1 << 128) - 1
        assert harness.test_mul_div_rounding_up(u128_max, u128_max, u128_max) == u128_max

    def test_mul_div_rounding_up_truncates(self, harness):
        """1 * 1 / 2 = 0 floor, 1 rounding up."""
        assert harness.test_mul_div(1, 1, 2) == 0
        assert harness.test_mul_div_rounding_up(1, 1, 2) == 1


# ═══════════════════════════════════════════════════════════════════════════
# UnsafeMath
# ═══════════════════════════════════════════════════════════════════════════


class TestUnsafeMath:
    """Test Vyper UnsafeMath.div_rounding_up against Solidity reference."""

    def test_div_rounding_up_exact(self, harness):
        """10 / 5 = 2 exact, rounding up = 2."""
        assert harness.test_div_rounding_up(10, 5) == 2

    def test_div_rounding_up_truncates(self, harness):
        """10 / 3 = 3.333..., rounding up = 4."""
        assert harness.test_div_rounding_up(10, 3) == 4

    def test_div_rounding_up_one_over(self, harness):
        """1 / 2 = 0.5, rounding up = 1."""
        assert harness.test_div_rounding_up(1, 2) == 1

    def test_div_rounding_up_zero(self, harness):
        """0 / 5 = 0."""
        assert harness.test_div_rounding_up(0, 5) == 0

    def test_div_rounding_up_large(self, harness):
        """max / 1 = max."""
        u256_max = (1 << 256) - 1
        assert harness.test_div_rounding_up(u256_max, 1) == u256_max


# ═══════════════════════════════════════════════════════════════════════════
# SafeCast
# ═══════════════════════════════════════════════════════════════════════════


class TestSafeCast:
    """Test Vyper SafeCast against Solidity reference."""

    # ── to_uint160 ──

    def test_to_uint160_small(self, harness):
        assert harness.test_to_uint160(100) == 100

    def test_to_uint160_max(self, harness):
        u160_max = (1 << 160) - 1
        assert harness.test_to_uint160(u160_max) == u160_max

    def test_to_uint160_overflow_reverts(self, harness):
        u161 = 1 << 160
        with pytest.raises(Exception):
            harness.test_to_uint160(u161)

    # ── to_int128 ──

    def test_to_int128_positive(self, harness):
        assert harness.test_to_int128(100) == 100

    def test_to_int128_negative(self, harness):
        assert harness.test_to_int128(-100) == -100

    def test_to_int128_max(self, harness):
        i128_max = (1 << 127) - 1
        assert harness.test_to_int128(i128_max) == i128_max

    def test_to_int128_min(self, harness):
        i128_min = -(1 << 127)
        assert harness.test_to_int128(i128_min) == i128_min

    def test_to_int128_overflow_reverts(self, harness):
        i129 = 1 << 127
        with pytest.raises(Exception):
            harness.test_to_int128(i129)

    # ── to_int256 ──

    def test_to_int256_small(self, harness):
        assert harness.test_to_int256(0) == 0

    def test_to_int256_positive(self, harness):
        assert harness.test_to_int256(100) == 100

    def test_to_int256_max(self, harness):
        i255_max = (1 << 255) - 1
        assert harness.test_to_int256(i255_max) == i255_max

    def test_to_int256_overflow_reverts(self, harness):
        u255_plus = 1 << 255
        with pytest.raises(Exception):
            harness.test_to_int256(u255_plus)


# ═══════════════════════════════════════════════════════════════════════════
# SqrtPriceMath
# ═══════════════════════════════════════════════════════════════════════════


class TestSqrtPriceMath:
    """Test Vyper SqrtPriceMath against Solidity reference values.

    Reference values computed by deploying the real Solidity SqrtPriceMath
    on a local Anvil instance and calling with cast.
    """

    Q96 = 79228162514264337593543950336  # 2^96

    def test_get_next_sqrt_price_from_input_zfo(self, harness):
        """zfo=true (selling token0): price decreases.

        Solidity: getNextSqrtPriceFromInput(Q96, 1e18, 1e18, true) = 39614081257132168796771975168
        """
        result = harness.test_get_next_sqrt_price_from_input(self.Q96, 10**18, 10**18, True)
        assert result == 39614081257132168796771975168

    def test_get_next_sqrt_price_from_input_not_zfo(self, harness):
        """zfo=false (selling token1): price increases.

        Solidity: getNextSqrtPriceFromInput(Q96, 1e18, 1e18, false) = 158456325028528675187087900672
        """
        result = harness.test_get_next_sqrt_price_from_input(self.Q96, 10**18, 10**18, False)
        assert result == 158456325028528675187087900672

    def test_get_next_sqrt_price_from_input_zero_amount(self, harness):
        """Zero amount: price doesn't change."""
        result = harness.test_get_next_sqrt_price_from_input(self.Q96, 10**18, 0, True)
        assert result == self.Q96

    def test_get_next_sqrt_price_from_output_zfo(self, harness):
        """zfo=true (selling token0, specifying token1 output): price decreases.

        Solidity: getNextSqrtPriceFromOutput(Q96, 1e18, 1e17, true) = 71305346262837903834189555302
        """
        result = harness.test_get_next_sqrt_price_from_output(self.Q96, 10**18, 10**17, True)
        assert result == 71305346262837903834189555302

    def test_get_next_sqrt_price_from_output_not_zfo(self, harness):
        """zfo=false (selling token1, specifying token0 output): price increases.

        Solidity: getNextSqrtPriceFromOutput(Q96, 1e18, 1e17, false) = 88031291682515930659493278152
        """
        result = harness.test_get_next_sqrt_price_from_output(self.Q96, 10**18, 10**17, False)
        assert result == 88031291682515930659493278152

    def test_get_amount0_delta_round_up(self, harness):
        """Amount0 between sqrt prices Q96 and 2*Q96.

        Solidity: getAmount0Delta(Q96, 2*Q96, 1e18, true) = 500000000000000000
        """
        result = harness.test_get_amount0_delta(self.Q96, self.Q96 * 2, 10**18, True)
        assert result == 500000000000000000

    def test_get_amount0_delta_round_down(self, harness):
        """Round down gives same result for exact division.

        Solidity: getAmount0Delta(Q96, 2*Q96, 1e18, false) = 500000000000000000
        """
        result = harness.test_get_amount0_delta(self.Q96, self.Q96 * 2, 10**18, False)
        assert result == 500000000000000000

    def test_get_amount1_delta_round_up(self, harness):
        """Amount1 between sqrt prices Q96 and 2*Q96.

        Solidity: getAmount1Delta(Q96, 2*Q96, 1e18, true) = 1000000000000000000
        """
        result = harness.test_get_amount1_delta(self.Q96, self.Q96 * 2, 10**18, True)
        assert result == 1000000000000000000

    def test_get_amount1_delta_round_down(self, harness):
        """Round down gives same result for exact division.

        Solidity: getAmount0Delta(Q96, 2*Q96, 1e18, false) = 1000000000000000000
        """
        result = harness.test_get_amount1_delta(self.Q96, self.Q96 * 2, 10**18, False)
        assert result == 1000000000000000000

    def test_get_amount0_delta_reversed_inputs(self, harness):
        """Swapping a/b should give same result (function swaps internally)."""
        r1 = harness.test_get_amount0_delta(self.Q96, self.Q96 * 2, 10**18, True)
        r2 = harness.test_get_amount0_delta(self.Q96 * 2, self.Q96, 10**18, True)
        assert r1 == r2

    def test_get_amount1_delta_reversed_inputs(self, harness):
        """Swapping a/b should give same result (function swaps internally)."""
        r1 = harness.test_get_amount1_delta(self.Q96, self.Q96 * 2, 10**18, True)
        r2 = harness.test_get_amount1_delta(self.Q96 * 2, self.Q96, 10**18, True)
        assert r1 == r2


# ═══════════════════════════════════════════════════════════════════════════
# TickMath
# ═══════════════════════════════════════════════════════════════════════════


class TestTickMath:
    """Test Vyper TickMath against Solidity reference values."""

    Q96 = 79228162514264337593543950336

    def test_get_sqrt_ratio_at_tick_zero(self, harness):
        """Tick 0 → sqrt price = 2^96 (1:1 ratio)."""
        assert harness.test_get_sqrt_ratio_at_tick(0) == self.Q96

    def test_get_sqrt_ratio_at_tick_one(self, harness):
        """Tick 1 → sqrt price slightly above Q96.

        Solidity: getSqrtRatioAtTick(1) = 79232123823359799118286999568
        """
        assert harness.test_get_sqrt_ratio_at_tick(1) == 79232123823359799118286999568

    def test_get_sqrt_ratio_at_tick_100(self, harness):
        """Tick 100.

        Solidity: getSqrtRatioAtTick(100) = 79625275426524748796330556128
        """
        assert harness.test_get_sqrt_ratio_at_tick(100) == 79625275426524748796330556128

    def test_get_sqrt_ratio_at_tick_100000(self, harness):
        """Large tick.

        Solidity: getSqrtRatioAtTick(100000) = 11755562826496067164730007768450
        """
        assert harness.test_get_sqrt_ratio_at_tick(100000) == 11755562826496067164730007768450

    def test_get_tick_at_sqrt_ratio_q96(self, harness):
        """Price Q96 → tick 0.

        Solidity: getTickAtSqrtRatio(Q96) = 0
        """
        assert harness.test_get_tick_at_sqrt_ratio(self.Q96) == 0

    def test_get_tick_at_sqrt_ratio_min(self, harness):
        """MIN_SQRT_RATIO → tick -887272.

        Solidity: getTickAtSqrtRatio(4295128739) = -887272
        """
        assert harness.test_get_tick_at_sqrt_ratio(4295128739) == -887272

    def test_round_trip_tick_100(self, harness):
        """tick→price→tick round trip for tick=100."""
        price = harness.test_get_sqrt_ratio_at_tick(100)
        tick = harness.test_get_tick_at_sqrt_ratio(price)
        assert tick == 100

    def test_round_trip_tick_1000(self, harness):
        """tick→price→tick round trip for tick=1000."""
        price = harness.test_get_sqrt_ratio_at_tick(1000)
        tick = harness.test_get_tick_at_sqrt_ratio(price)
        assert tick == 1000

    def test_round_trip_tick_negative(self, harness):
        """tick→price→tick round trip for tick=-1000."""
        price = harness.test_get_sqrt_ratio_at_tick(-1000)
        tick = harness.test_get_tick_at_sqrt_ratio(price)
        assert tick == -1000

    def test_get_sqrt_ratio_at_tick_reverts_past_max(self, harness):
        """Ticks beyond MAX_TICK should revert."""
        with pytest.raises(Exception):
            harness.test_get_sqrt_ratio_at_tick(887273)
