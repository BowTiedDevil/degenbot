"""Tests for Balancer WeightedMath library, ported from the Balancer v2 monorepo tests at
https://github.com/balancer/balancer-v2-monorepo/blob/master/pkg/pool-weighted/test/WeightedMath.test.ts

The TypeScript reference implementation is at:
https://github.com/balancer/balancer-v2-monorepo/blob/master/pvt/helpers/src/models/pools/weighted/math.ts

These tests verify the weighted pool invariant calculation, swap math (outGivenIn, inGivenOut),
and fee handling.
"""

from decimal import Decimal

import pytest

from degenbot.balancer.libraries import weighted_math
from degenbot.balancer.libraries.helpers import bn, from_fp, to_fp
from degenbot.exceptions.pool import EVMRevertError

MAX_RELATIVE_ERROR = Decimal("0.0001")

ONE = 1 * 10**18


# ---------- Reference implementations (from TS math.ts) ----------


def _calculate_invariant(
    fp_raw_balances: list[int],
    fp_raw_weights: list[int],
) -> int:
    """Reference implementation using Python Decimal for arbitrary precision."""
    normalized_weights = [from_fp(weight) for weight in fp_raw_weights]
    balances = [Decimal(balance) for balance in fp_raw_balances]

    invariant = 1
    for i, balance in enumerate(balances):
        invariant *= balance ** normalized_weights[i]
    return bn(invariant)


def _calc_out_given_in(
    fp_balance_in: int,
    fp_weight_in: int,
    fp_balance_out: int,
    fp_weight_out: int,
    fp_amount_in: int,
) -> int:
    """Reference implementation using Python Decimal."""
    new_balance = from_fp(fp_balance_in) + from_fp(fp_amount_in)
    base = from_fp(fp_balance_in) / new_balance
    exponent = from_fp(fp_weight_in) / from_fp(fp_weight_out)
    ratio = Decimal(1) - base**exponent
    return to_fp(from_fp(fp_balance_out) * ratio)


def _calc_in_given_out(
    fp_balance_in: int,
    fp_weight_in: int,
    fp_balance_out: int,
    fp_weight_out: int,
    fp_amount_out: int,
) -> int:
    """Reference implementation using Python Decimal."""
    new_balance = from_fp(fp_balance_out) - from_fp(fp_amount_out)
    base = from_fp(fp_balance_out) / new_balance
    exponent = from_fp(fp_weight_out) / from_fp(fp_weight_in)
    ratio = base**exponent - Decimal(1)
    return int(to_fp(from_fp(fp_balance_in) * ratio))


# ---------- Invariant ----------


class TestInvariant:
    def test_zero_invariant_reverts(self):
        with pytest.raises(EVMRevertError, match="ZERO_INVARIANT"):
            weighted_math.calculate_invariant(normalized_weights=[1], balances=[0])

    def test_two_tokens(self):
        normalized_weights = [int(0.3 * 10**18), int(0.7 * 10**18)]
        balances = [10 * 10**18, 12 * 10**18]

        result = weighted_math.calculate_invariant(
            normalized_weights=normalized_weights,
            balances=balances,
        )
        expected_invariant = _calculate_invariant(
            fp_raw_balances=balances,
            fp_raw_weights=normalized_weights,
        )
        assert result == pytest.approx(expected_invariant, rel=MAX_RELATIVE_ERROR)

    def test_three_tokens(self):
        normalized_weights = [int(0.3 * 10**18), int(0.2 * 10**18), int(0.5 * 10**18)]
        balances = [10 * 10**18, 12 * 10**18, 14 * 10**18]
        result = weighted_math.calculate_invariant(
            normalized_weights=normalized_weights,
            balances=balances,
        )
        expected_invariant = _calculate_invariant(
            fp_raw_balances=balances,
            fp_raw_weights=normalized_weights,
        )
        assert result == pytest.approx(expected_invariant, rel=MAX_RELATIVE_ERROR)

    def test_equal_weights(self):
        """50/50 pool with equal balances."""
        normalized_weights = [int(0.5 * 10**18), int(0.5 * 10**18)]
        balances = [100 * 10**18, 100 * 10**18]
        result = weighted_math.calculate_invariant(
            normalized_weights=normalized_weights,
            balances=balances,
        )
        expected = _calculate_invariant(balances, normalized_weights)
        assert result == pytest.approx(expected, rel=MAX_RELATIVE_ERROR)
        # √(100 * 100) = 100
        assert result == pytest.approx(100 * 10**18, rel=MAX_RELATIVE_ERROR)

    def test_80_20_weights(self):
        """Typical 80/20 Balancer pool."""
        normalized_weights = [80 * 10**16, 20 * 10**16]
        balances = [100 * 10**18, 100 * 10**18]
        result = weighted_math.calculate_invariant(
            normalized_weights=normalized_weights,
            balances=balances,
        )
        expected = _calculate_invariant(balances, normalized_weights)
        assert result == pytest.approx(expected, rel=MAX_RELATIVE_ERROR)


# ---------- OutGivenIn ----------


class TestOutGivenIn:
    def test_simple_swap(self):
        """Ported from the Balancer WeightedMath.test.ts 'outGivenIn' test."""
        token_balance_in = 100 * 10**18
        token_weight_in = 50 * 10**18
        token_balance_out = 100 * 10**18
        token_weight_out = 40 * 10**18
        token_amount_in = 15 * 10**18

        out_amount_math = _calc_out_given_in(
            token_balance_in,
            token_weight_in,
            token_balance_out,
            token_weight_out,
            token_amount_in,
        )
        out_amount_pool = weighted_math._calc_out_given_in(
            balance_in=token_balance_in,
            weight_in=token_weight_in,
            balance_out=token_balance_out,
            weight_out=token_weight_out,
            amount_in=token_amount_in,
        )
        assert out_amount_pool == pytest.approx(expected=out_amount_math, rel=MAX_RELATIVE_ERROR)

    def test_equal_weights_fifty_fifty(self):
        """In a 50/50 pool with equal balances, swapping should be close to x/y constant product."""
        balance_in = 100 * 10**18
        balance_out = 100 * 10**18
        weight_in = int(0.5 * 10**18)
        weight_out = int(0.5 * 10**18)
        amount_in = 10 * 10**18

        result = weighted_math._calc_out_given_in(
            balance_in=balance_in,
            weight_in=weight_in,
            balance_out=balance_out,
            weight_out=weight_out,
            amount_in=amount_in,
        )
        # Constant product: 100 * 100 = 10000; 110 * x = 10000; x = 90.909...
        expected = _calc_out_given_in(balance_in, weight_in, balance_out, weight_out, amount_in)
        assert result == pytest.approx(expected, rel=MAX_RELATIVE_ERROR)

    def test_max_in_ratio_reverts(self):
        """Amounts exceeding 30% of balance_in should revert."""
        balance_in = 100 * 10**18
        weight_in = 50 * 10**18
        balance_out = 100 * 10**18
        weight_out = 40 * 10**18
        amount_in = 31 * 10**18  # 31% > 30%

        with pytest.raises(EVMRevertError, match="MAX_IN_RATIO"):
            weighted_math._calc_out_given_in(
                balance_in=balance_in,
                weight_in=weight_in,
                balance_out=balance_out,
                weight_out=weight_out,
                amount_in=amount_in,
            )

    def test_at_max_in_ratio(self):
        """Exactly 30% should work."""
        balance_in = 100 * 10**18
        weight_in = 50 * 10**18
        balance_out = 100 * 10**18
        weight_out = 40 * 10**18
        # 30% of balance_in, rounded per mul_down
        amount_in = 30 * 10**18

        result = weighted_math._calc_out_given_in(
            balance_in=balance_in,
            weight_in=weight_in,
            balance_out=balance_out,
            weight_out=weight_out,
            amount_in=amount_in,
        )
        assert result > 0

    def test_zero_amount_in(self):
        """Zero amount in should produce zero amount out."""
        result = weighted_math._calc_out_given_in(
            balance_in=100 * 10**18,
            weight_in=int(0.5 * 10**18),
            balance_out=100 * 10**18,
            weight_out=int(0.5 * 10**18),
            amount_in=0,
        )
        assert result == 0

    def test_small_amount_in(self):
        """Very small swap amounts should produce non-zero results."""
        result = weighted_math._calc_out_given_in(
            balance_in=100 * 10**18,
            weight_in=50 * 10**18,
            balance_out=100 * 10**18,
            weight_out=40 * 10**18,
            amount_in=10 * 10**6,
        )
        assert result > 0

    def test_rounding_direction(self):
        """OutGivenIn rounds down (favoring the pool). The result should be <= the mathematically
        exact value.
        """
        balance_in = 100 * 10**18
        weight_in = 50 * 10**18
        balance_out = 100 * 10**18
        weight_out = 40 * 10**18
        amount_in = 15 * 10**18

        result = weighted_math._calc_out_given_in(
            balance_in=balance_in,
            weight_in=weight_in,
            balance_out=balance_out,
            weight_out=weight_out,
            amount_in=amount_in,
        )
        exact = _calc_out_given_in(balance_in, weight_in, balance_out, weight_out, amount_in)
        # The fixed-point result should be at or below the exact value
        assert result <= exact or result == pytest.approx(exact, rel=1e-10)


# ---------- InGivenOut ----------


class TestInGivenOut:
    def test_simple_swap(self):
        """Ported from the Balancer WeightedMath.test.ts 'inGivenOut' test."""
        token_balance_in = 100 * 10**18
        token_weight_in = 50 * 10**18
        token_balance_out = 100 * 10**18
        token_weight_out = 40 * 10**18
        token_amount_out = 15 * 10**18

        in_amount_math = _calc_in_given_out(
            token_balance_in,
            token_weight_in,
            token_balance_out,
            token_weight_out,
            token_amount_out,
        )
        in_amount_pool = weighted_math._calc_in_given_out(
            balance_in=token_balance_in,
            weight_in=token_weight_in,
            balance_out=token_balance_out,
            weight_out=token_weight_out,
            amount_out=token_amount_out,
        )
        assert in_amount_pool == pytest.approx(expected=in_amount_math, rel=MAX_RELATIVE_ERROR)

    def test_max_out_ratio_reverts(self):
        """Amounts exceeding 30% of balance_out should revert."""
        balance_in = 100 * 10**18
        weight_in = 50 * 10**18
        balance_out = 100 * 10**18
        weight_out = 40 * 10**18
        amount_out = 31 * 10**18  # 31% > 30%

        with pytest.raises(EVMRevertError, match="MAX_OUT_RATIO"):
            weighted_math._calc_in_given_out(
                balance_in=balance_in,
                weight_in=weight_in,
                balance_out=balance_out,
                weight_out=weight_out,
                amount_out=amount_out,
            )

    def test_zero_amount_out(self):
        """With amount_out=0, base=div_up(bO,bO)=ONE, power=pow_up(ONE,ONE)=ONE+max_error,
        ratio=max_error. The on-chain contract does NOT have a y==1 fast path, so this produces
        a small non-zero result rather than exactly 0.
        """
        result = weighted_math._calc_in_given_out(
            balance_in=100 * 10**18,
            weight_in=int(0.5 * 10**18),
            balance_out=100 * 10**18,
            weight_out=int(0.5 * 10**18),
            amount_out=0,
        )
        # On-chain: pow_up(ONE, ONE) = ONE + 10001, ratio = 10001,
        # result = mul_up(100e18, 10001) = 1000100
        assert result > 0  # Not exactly zero due to pow_up error bound
        assert result < 10**7  # But very small relative to balances

    def test_rounding_direction(self):
        """InGivenOut rounds up (favoring the pool). The result should be >= the mathematically
        exact value.
        """
        balance_in = 100 * 10**18
        weight_in = 50 * 10**18
        balance_out = 100 * 10**18
        weight_out = 40 * 10**18
        amount_out = 15 * 10**18

        result = weighted_math._calc_in_given_out(
            balance_in=balance_in,
            weight_in=weight_in,
            balance_out=balance_out,
            weight_out=weight_out,
            amount_out=amount_out,
        )
        exact = _calc_in_given_out(balance_in, weight_in, balance_out, weight_out, amount_out)
        # The fixed-point result should be at or above the exact value
        assert result >= exact or result == pytest.approx(exact, rel=1e-10)


# ---------- Extreme cases ----------


class TestExtremeAmounts:
    def test_out_given_in_min_amount_in(self):
        token_balance_in = 100 * 10**18
        token_weight_in = 50 * 10**18
        token_balance_out = 100 * 10**18
        token_weight_out = 40 * 10**18
        token_amount_in = 10 * 10**6  # (MIN AMOUNT = 0.00000000001)

        out_amount_math = _calc_out_given_in(
            token_balance_in,
            token_weight_in,
            token_balance_out,
            token_weight_out,
            token_amount_in,
        )
        out_amount_pool = weighted_math._calc_out_given_in(
            token_balance_in,
            token_weight_in,
            token_balance_out,
            token_weight_out,
            token_amount_in,
        )
        # Note: high relative error for small amounts (matching Balancer's own tolerance)
        # Convert Decimal to int for comparison
        assert out_amount_pool == pytest.approx(int(out_amount_math), rel=0.1)

    def test_in_given_out_min_amount_out(self):
        token_balance_in = 100 * 10**18
        token_weight_in = 50 * 10**18
        token_balance_out = 100 * 10**18
        token_weight_out = 40 * 10**18
        token_amount_out = 10 * 10**6  # (MIN AMOUNT = 0.00000000001)

        in_amount_math = _calc_in_given_out(
            token_balance_in,
            token_weight_in,
            token_balance_out,
            token_weight_out,
            token_amount_out,
        )
        in_amount_pool = weighted_math._calc_in_given_out(
            token_balance_in,
            token_weight_in,
            token_balance_out,
            token_weight_out,
            token_amount_out,
        )
        # Note: high relative error for small amounts (matching Balancer's own tolerance)
        assert in_amount_pool == pytest.approx(int(in_amount_math), rel=0.5)


class TestExtremeWeights:
    def test_out_given_in_max_weights_relation(self):
        """Weight relation = 130.07"""
        token_balance_in = 100 * 10**18
        token_weight_in = int(130.7 * 10**18)
        token_balance_out = 100 * 10**18
        token_weight_out = 1 * 10**18
        token_amount_in = 15 * 10**18
        out_amount_math = _calc_out_given_in(
            token_balance_in,
            token_weight_in,
            token_balance_out,
            token_weight_out,
            token_amount_in,
        )
        out_amount_pool = weighted_math._calc_out_given_in(
            token_balance_in,
            token_weight_in,
            token_balance_out,
            token_weight_out,
            token_amount_in,
        )
        assert out_amount_pool == pytest.approx(expected=out_amount_math, rel=MAX_RELATIVE_ERROR)

    def test_out_given_in_min_weights_relation(self):
        """Weight relation = 0.00769"""
        token_balance_in = 100 * 10**18
        token_weight_in = int(0.00769 * 10**18)
        token_balance_out = 100 * 10**18
        token_weight_out = 1 * 10**18
        token_amount_in = 15 * 10**18
        out_amount_math = _calc_out_given_in(
            token_balance_in,
            token_weight_in,
            token_balance_out,
            token_weight_out,
            token_amount_in,
        )
        out_amount_pool = weighted_math._calc_out_given_in(
            token_balance_in,
            token_weight_in,
            token_balance_out,
            token_weight_out,
            token_amount_in,
        )
        assert out_amount_pool == pytest.approx(expected=out_amount_math, rel=MAX_RELATIVE_ERROR)


# ---------- Fee handling ----------


class TestSubtractSwapFeeAmount:
    def test_zero_fee(self):
        """Zero fee: amount is returned unchanged."""
        result = weighted_math._subtract_swap_fee_amount(1000, 0)
        assert result == 1000

    def test_one_percent_fee(self):
        """1% fee on 1000 = 10 fee, 990 returned."""
        fee_percentage = int(0.01 * 10**18)
        result = weighted_math._subtract_swap_fee_amount(1000, fee_percentage)
        assert result == 990

    def test_ten_percent_fee(self):
        """10% fee."""
        fee_percentage = int(0.1 * 10**18)
        result = weighted_math._subtract_swap_fee_amount(1000, fee_percentage)
        assert result == 900

    def test_rounds_up_fee(self):
        """Fee is rounded up, so the returned amount is rounded down."""
        # Use values that don't divide evenly
        fee_percentage = int(0.03 * 10**18)  # 3%
        amount = 7
        result = weighted_math._subtract_swap_fee_amount(amount, fee_percentage)
        # mul_up(7, 3e16) = ceil(7 * 3e16 / 1e18) = ceil(0.21) = 1
        # result = 7 - 1 = 6
        assert result == 6


class TestAddSwapFeeAmount:
    def test_zero_fee(self):
        """Zero fee: amount is returned unchanged."""
        result = weighted_math._add_swap_fee_amount(1000, 0)
        assert result == 1000

    def test_one_percent_fee(self):
        """Amount / (1 - 0.01) = amount / 0.99."""
        fee_percentage = int(0.01 * 10**18)
        amount = 990
        result = weighted_math._add_swap_fee_amount(amount, fee_percentage)
        # 990 / 0.99 = 1000
        assert result == 1000

    def test_ten_percent_fee(self):
        """Amount / (1 - 0.1) = amount / 0.9."""
        fee_percentage = int(0.1 * 10**18)
        amount = 900
        result = weighted_math._add_swap_fee_amount(amount, fee_percentage)
        # 900 / 0.9 = 1000
        assert result == 1000

    def test_rounds_up(self):
        """Result is rounded up (favoring the pool)."""
        fee_percentage = int(0.03 * 10**18)  # 3%
        amount = 97
        result = weighted_math._add_swap_fee_amount(amount, fee_percentage)
        # complement(3e16) = 1e18 - 3e16 = 97e16
        # div_up(97, 97e16) ≈ 100
        expected_exact = 97 / 0.97
        assert result >= int(expected_exact * 10**18 // 10**18)  # should round up


# ---------- Symmetry / consistency checks ----------


class TestSwapConsistency:
    def test_out_given_in_then_in_given_out_should_return_original_amount(self):
        """inGivenOut(outGivenIn(x)) should approximately equal x."""
        balance_in = 100 * 10**18
        weight_in = 50 * 10**18
        balance_out = 100 * 10**18
        weight_out = 40 * 10**18
        amount_in = 15 * 10**18

        amount_out = weighted_math._calc_out_given_in(
            balance_in,
            weight_in,
            balance_out,
            weight_out,
            amount_in,
        )
        recovered_amount_in = weighted_math._calc_in_given_out(
            balance_in,
            weight_in,
            balance_out,
            weight_out,
            amount_out,
        )
        # Due to rounding, recovered_amount_in may be slightly different from amount_in
        assert recovered_amount_in == pytest.approx(amount_in, rel=1e-6)

    def test_large_swap_consistency(self):
        """Consistency check with larger swap amounts (25% of balance)."""
        balance_in = 1000 * 10**18
        weight_in = 70 * 10**16
        balance_out = 500 * 10**18
        weight_out = 30 * 10**16
        amount_in = 100 * 10**18

        amount_out = weighted_math._calc_out_given_in(
            balance_in,
            weight_in,
            balance_out,
            weight_out,
            amount_in,
        )
        recovered = weighted_math._calc_in_given_out(
            balance_in,
            weight_in,
            balance_out,
            weight_out,
            amount_out,
        )
        assert recovered == pytest.approx(amount_in, rel=1e-6)

    def test_80_20_pool_swap(self):
        """Test with real-world 80/20 Balancer pool weights."""
        balance_in = 1000 * 10**18
        weight_in = 80 * 10**16
        balance_out = 2000 * 10**18
        weight_out = 20 * 10**16
        amount_in = 50 * 10**18

        out_math = _calc_out_given_in(
            balance_in,
            weight_in,
            balance_out,
            weight_out,
            amount_in,
        )
        out_pool = weighted_math._calc_out_given_in(
            balance_in,
            weight_in,
            balance_out,
            weight_out,
            amount_in,
        )
        assert out_pool == pytest.approx(out_math, rel=MAX_RELATIVE_ERROR)

    def test_three_token_pool_pairwise_swap(self):
        """In a 3-token pool, swaps between each pair should work."""
        # Token weights: A=50%, B=30%, C=20%
        # Token balances: A=100e18, B=200e18, C=300e18
        weight_a = int(0.5 * 10**18)
        weight_b = int(0.3 * 10**18)
        weight_c = int(0.2 * 10**18)
        balance_a = 100 * 10**18
        balance_b = 200 * 10**18
        balance_c = 300 * 10**18
        amount_in = 10 * 10**18

        # A -> B
        out_ab = weighted_math._calc_out_given_in(
            balance_a,
            weight_a,
            balance_b,
            weight_b,
            amount_in,
        )
        expected_ab = _calc_out_given_in(
            balance_a,
            weight_a,
            balance_b,
            weight_b,
            amount_in,
        )
        assert out_ab == pytest.approx(expected_ab, rel=MAX_RELATIVE_ERROR)

        # B -> C
        out_bc = weighted_math._calc_out_given_in(
            balance_b,
            weight_b,
            balance_c,
            weight_c,
            amount_in,
        )
        expected_bc = _calc_out_given_in(
            balance_b,
            weight_b,
            balance_c,
            weight_c,
            amount_in,
        )
        assert out_bc == pytest.approx(expected_bc, rel=MAX_RELATIVE_ERROR)

        # A -> C
        out_ac = weighted_math._calc_out_given_in(
            balance_a,
            weight_a,
            balance_c,
            weight_c,
            amount_in,
        )
        expected_ac = _calc_out_given_in(
            balance_a,
            weight_a,
            balance_c,
            weight_c,
            amount_in,
        )
        assert out_ac == pytest.approx(expected_ac, rel=MAX_RELATIVE_ERROR)
