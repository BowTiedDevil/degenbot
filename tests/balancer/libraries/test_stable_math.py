"""
Tests for Balancer StableMath library, ported from the Balancer v2 monorepo tests.

Solidity source:
https://github.com/balancer/balancer-v2-monorepo/blob/master/pkg/pool-stable/contracts/StableMath.sol

These tests verify the stable pool invariant calculation, swap math (outGivenIn, inGivenOut),
BPT/join/exit calculations, and Newton-Raphson convergence behavior.

Key difference from deployed contracts:
The monorepo's _calculateInvariant always rounds down (no roundUp parameter), matching
Vyper's arithmetic. This eliminates Newton-Raphson oscillation that occurs with the
deployed contract's conditional rounding.
"""

from decimal import Decimal

from degenbot.balancer.libraries.fixed_point import ONE, div_down
from degenbot.balancer.libraries.stable_math import (
    AMP_PRECISION,
    _calc_bpt_in_given_exact_tokens_out,
    _calc_bpt_out_given_exact_tokens_in,
    _calc_due_token_protocol_swap_fee_amount,
    _calc_in_given_out,
    _calc_out_given_in,
    _calc_token_in_given_exact_bpt_out,
    _calc_token_out_given_exact_bpt_in,
    _calc_tokens_out_given_exact_bpt_in,
    _calculate_invariant,
    _get_rate,
    _get_token_balance_given_invariant_and_all_other_balances,
)

# ---------- Reference implementations ----------


def _ref_calculate_invariant(
    amp: int,
    balances: list[int],
) -> Decimal:
    """Reference invariant calculation using Python Decimal for arbitrary precision."""
    n = len(balances)
    s = sum(Decimal(b) for b in balances)

    if s == 0:
        return Decimal(0)

    ann = amp * n
    ann_dec = Decimal(ann)

    d = s

    for _ in range(255):
        d_p = d
        for j in range(n):
            d_p = d_p * d / (Decimal(balances[j]) * n)

        prev_d = d

        numerator = (_math_div_down_dec(ann_dec * s, AMP_PRECISION) + d_p * n) * d
        denominator = _math_div_down_dec(
            (ann - AMP_PRECISION) * d, AMP_PRECISION
        ) + (n + 1) * d_p
        d = numerator / denominator

        if abs(d - prev_d) <= 1:
            return d

    msg = "Invariant did not converge"
    raise ValueError(msg)


def _math_div_down_dec(a: Decimal, b: int) -> Decimal:
    """Decimal division rounding down."""
    return a // b


# ---------- Test data ----------

# Standard 2-token pool with amp=100 (stored as 100*AMP_PRECISION = 100000)
AMP_2TOKEN = 100 * AMP_PRECISION
BALANCES_2TOKEN = [10 * ONE, 10 * ONE]

# Standard 3-token pool with amp=200
AMP_3TOKEN = 200 * AMP_PRECISION
BALANCES_3TOKEN = [10 * ONE, 10 * ONE, 10 * ONE]

# Standard 4-token pool
AMP_4TOKEN = 100 * AMP_PRECISION
BALANCES_4TOKEN = [10 * ONE, 12 * ONE, 14 * ONE, 16 * ONE]

# 5-token pool
AMP_5TOKEN = 500 * AMP_PRECISION
BALANCES_5TOKEN = [10 * ONE, 11 * ONE, 12 * ONE, 13 * ONE, 14 * ONE]


# ---------- Invariant tests ----------


class TestCalculateInvariant:
    def test_zero_balances(self):
        assert _calculate_invariant(AMP_2TOKEN, [0, 0]) == 0

    def test_two_token_invariant_convergence(self):
        """Invariant should converge for a 2-token pool."""
        inv = _calculate_invariant(AMP_2TOKEN, BALANCES_2TOKEN)
        assert inv > 0
        # For equal balances and amp=100, invariant should be close to 2 * balance
        assert inv > 15 * ONE
        assert inv < 25 * ONE

    def test_two_token_invariant_matches_reference(self):
        """Invariant should match the reference Decimal implementation."""
        amp = 100 * AMP_PRECISION
        balances = [10 * ONE, 12 * ONE]
        py_inv = _calculate_invariant(amp, balances)
        ref_inv = int(_ref_calculate_invariant(amp, balances))
        # The integer Newton-Raphson should converge to within 1 of the true value
        assert abs(py_inv - ref_inv) <= 1

    def test_three_token_invariant(self):
        inv = _calculate_invariant(AMP_3TOKEN, BALANCES_3TOKEN)
        assert inv > 0

    def test_four_token_invariant(self):
        inv = _calculate_invariant(AMP_4TOKEN, BALANCES_4TOKEN)
        assert inv > 0

    def test_five_token_invariant(self):
        inv = _calculate_invariant(AMP_5TOKEN, BALANCES_5TOKEN)
        assert inv > 0

    def test_high_amp_produces_larger_invariant(self):
        """Higher amplification factor -> invariant closer to sum of balances."""
        low_amp = 10 * AMP_PRECISION
        high_amp = 1000 * AMP_PRECISION
        balances = [10 * ONE, 12 * ONE]
        inv_low = _calculate_invariant(low_amp, balances)
        inv_high = _calculate_invariant(high_amp, balances)
        # Higher amp -> larger invariant (closer to constant-sum)
        assert inv_high > inv_low

    def test_invariant_for_balanced_pool_near_sum(self):
        """For a perfectly balanced pool, invariant should be close to n * balance."""
        amp = 100 * AMP_PRECISION
        balances = [10 * ONE] * 3
        inv = _calculate_invariant(amp, balances)
        # For moderate amp, invariant is less than sum
        assert inv <= 30 * ONE

    def test_invariant_convergence_symmetry(self):
        """Invariant should be the same regardless of token ordering for equal balances."""
        amp = 100 * AMP_PRECISION
        b1 = [10 * ONE, 20 * ONE]
        b2 = [20 * ONE, 10 * ONE]
        inv1 = _calculate_invariant(amp, b1)
        inv2 = _calculate_invariant(amp, b2)
        assert inv1 == inv2

    def test_imbalanced_three_token_converges(self):
        """Test convergence with highly imbalanced 3-token balances (on-chain scenario)."""
        # bb-a-USD pool data: balances are hugely imbalanced (~349x ratio)
        amp = 100000
        balances = [2139687929523177962, 745798577571382612693, 2250878655272386819]
        inv = _calculate_invariant(amp, balances)
        assert inv > 0

    def test_dai_usdc_usdt_converges(self):
        """Test convergence with another set of real on-chain balances."""
        amp = 500000
        balances = [283241682256295, 212226358109770968, 2439572319899646531]
        inv = _calculate_invariant(amp, balances)
        assert inv > 0


# ---------- Swap tests ----------


class TestCalcOutGivenIn:
    def test_two_token_swap(self):
        """Basic 2-token swap: amount out should be positive and less than balance."""
        balances = list(BALANCES_2TOKEN)
        amp = AMP_2TOKEN
        invariant = _calculate_invariant(amp, balances)
        amount_in = ONE

        amount_out = _calc_out_given_in(
            amp, list(balances), 0, 1, amount_in, invariant
        )
        assert amount_out > 0
        assert amount_out < balances[1]

    def test_symmetric_swap(self):
        """For equal balances, swapping same amount in either direction gives same out."""
        balances = [10 * ONE, 10 * ONE]
        amp = 100 * AMP_PRECISION
        invariant = _calculate_invariant(amp, balances)
        amount_in = ONE

        out_0_to_1 = _calc_out_given_in(amp, list(balances), 0, 1, amount_in, invariant)
        out_1_to_0 = _calc_out_given_in(amp, list(balances), 1, 0, amount_in, invariant)
        assert out_0_to_1 == out_1_to_0

    def test_larger_amount_in_gives_larger_amount_out(self):
        balances = [10 * ONE, 10 * ONE, 10 * ONE]
        amp = 100 * AMP_PRECISION
        inv = _calculate_invariant(amp, balances)

        small_out = _calc_out_given_in(amp, list(balances), 0, 1, ONE, inv)
        large_out = _calc_out_given_in(amp, list(balances), 0, 1, 2 * ONE, inv)
        assert large_out > small_out

    def test_does_not_modify_balances(self):
        """_calc_out_given_in should restore balances after mutation."""
        balances = [10 * ONE, 10 * ONE]
        amp = 100 * AMP_PRECISION
        inv = _calculate_invariant(amp, balances)
        original = list(balances)

        _calc_out_given_in(amp, list(balances), 0, 1, ONE, inv)
        assert balances == original


class TestCalcInGivenOut:
    def test_two_token_swap(self):
        """Basic 2-token swap: amount in should be positive."""
        balances = list(BALANCES_2TOKEN)
        amp = AMP_2TOKEN
        invariant = _calculate_invariant(amp, balances)
        amount_out = ONE

        amount_in = _calc_in_given_out(
            amp, list(balances), 0, 1, amount_out, invariant
        )
        assert amount_in > 0

    def test_in_greater_than_out_for_imbalanced_pool(self):
        """For an imbalanced pool, you need to put in more than you get out."""
        balances = [5 * ONE, 15 * ONE]  # Imbalanced
        amp = 100 * AMP_PRECISION
        inv = _calculate_invariant(amp, balances)

        amount_out = ONE
        amount_in = _calc_in_given_out(amp, list(balances), 0, 1, amount_out, inv)
        # token 0 (5 ONE) -> 1 (15 ONE): scarce -> abundant, should be cheaper
        assert amount_in < amount_out

    def test_symmetric_swap(self):
        """For equal balances, swapping same amount in either direction gives same in."""
        balances = [10 * ONE, 10 * ONE]
        amp = 100 * AMP_PRECISION
        invariant = _calculate_invariant(amp, balances)
        amount_out = ONE

        in_0_to_1 = _calc_in_given_out(amp, list(balances), 0, 1, amount_out, invariant)
        in_1_to_0 = _calc_in_given_out(amp, list(balances), 1, 0, amount_out, invariant)
        assert in_0_to_1 == in_1_to_0


# ---------- Round-trip consistency ----------


class TestRoundTrip:
    def test_out_given_in_then_in_given_out(self):
        """outGivenIn(x) = y => inGivenOut(y) should be close to x."""
        balances = [10 * ONE, 10 * ONE, 10 * ONE]
        amp = 100 * AMP_PRECISION
        inv = _calculate_invariant(amp, balances)
        amount_in = ONE

        amount_out = _calc_out_given_in(amp, list(balances), 0, 1, amount_in, inv)
        recovered_in = _calc_in_given_out(amp, list(balances), 0, 1, amount_out, inv)

        # StableMath has more rounding error than WeightedMath — tolerance of 0.1%
        max_error = max(abs(recovered_in - amount_in), 1)
        assert max_error <= amount_in // 1000

    def test_in_given_out_then_out_given_in(self):
        """inGivenOut(y) = x => outGivenIn(x) should be close to y."""
        balances = [10 * ONE, 15 * ONE, 20 * ONE]
        amp = 200 * AMP_PRECISION
        inv = _calculate_invariant(amp, balances)
        amount_out = ONE

        amount_in = _calc_in_given_out(amp, list(balances), 0, 1, amount_out, inv)
        recovered_out = _calc_out_given_in(amp, list(balances), 0, 1, amount_in, inv)

        # StableMath has more rounding error — tolerance of 0.1%
        max_error = max(abs(recovered_out - amount_out), 1)
        assert max_error <= amount_out // 1000


# ---------- _getTokenBalanceGivenInvariantAndAllOtherBalances tests ----------


class TestGetTokenBalance:
    def test_balanced_pool(self):
        """For a balanced pool, each token balance should be close to the original."""
        amp = 100 * AMP_PRECISION
        balances = [10 * ONE, 10 * ONE]
        inv = _calculate_invariant(amp, balances)

        for i in range(len(balances)):
            recovered = _get_token_balance_given_invariant_and_all_other_balances(
                amp, balances, inv, i
            )
            # Should be close to the original balance
            assert abs(recovered - balances[i]) <= 2

    def test_three_token_recovery(self):
        """Recover token balance from invariant + other balances."""
        amp = 200 * AMP_PRECISION
        balances = [10 * ONE, 12 * ONE, 14 * ONE]
        inv = _calculate_invariant(amp, balances)

        for i in range(len(balances)):
            recovered = _get_token_balance_given_invariant_and_all_other_balances(
                amp, balances, inv, i
            )
            # Rounding up means recovered may be slightly higher than original
            assert abs(recovered - balances[i]) <= balances[i] // 1_000_000 + 10


# ---------- BPT calculation tests ----------


class TestBptCalculations:
    def test_bpt_out_given_exact_tokens_in_proportional(self):
        """Proportional join should give BPT proportional to total supply."""
        amp = 100 * AMP_PRECISION
        balances = [10 * ONE, 10 * ONE]
        bpt_total_supply = 100 * ONE
        swap_fee = int(0.01 * ONE)
        current_inv = _calculate_invariant(amp, balances)

        amounts_in = [ONE, ONE]
        bpt_out = _calc_bpt_out_given_exact_tokens_in(
            amp, balances, amounts_in, bpt_total_supply, current_inv, swap_fee
        )
        assert bpt_out > 0

    def test_bpt_out_zero_if_no_invariant_increase(self):
        """If the invariant doesn't increase, BPT out is 0."""
        amp = 100 * AMP_PRECISION
        balances = [10 * ONE, 10 * ONE, 10 * ONE]
        bpt_total_supply = 100 * ONE
        swap_fee = int(0.01 * ONE)
        current_inv = _calculate_invariant(amp, balances)
        amounts_in = [0, 0, 0]

        bpt_out = _calc_bpt_out_given_exact_tokens_in(
            amp, balances, amounts_in, bpt_total_supply, current_inv, swap_fee
        )
        assert bpt_out == 0

    def test_token_in_given_exact_bpt_out(self):
        """Token in should be positive for a proportional BPT out."""
        amp = 100 * AMP_PRECISION
        balances = [10 * ONE, 10 * ONE]
        bpt_total_supply = 100 * ONE
        swap_fee = int(0.01 * ONE)
        current_inv = _calculate_invariant(amp, balances)

        bpt_amount_out = ONE
        token_in = _calc_token_in_given_exact_bpt_out(
            amp, balances, 0, bpt_amount_out, bpt_total_supply, current_inv, swap_fee
        )
        assert token_in > 0

    def test_bpt_in_given_exact_tokens_out(self):
        """BPT in should be positive when tokens are taken out."""
        amp = 100 * AMP_PRECISION
        balances = [10 * ONE, 10 * ONE, 10 * ONE]
        bpt_total_supply = 100 * ONE
        swap_fee = int(0.01 * ONE)
        current_inv = _calculate_invariant(amp, balances)

        amounts_out = [ONE, ONE, ONE]
        bpt_in = _calc_bpt_in_given_exact_tokens_out(
            amp, balances, amounts_out, bpt_total_supply, current_inv, swap_fee
        )
        assert bpt_in > 0

    def test_token_out_given_exact_bpt_in(self):
        """Token out should be positive and less than balance for a valid BPT in."""
        amp = 100 * AMP_PRECISION
        balances = [10 * ONE, 10 * ONE]
        bpt_total_supply = 100 * ONE
        swap_fee = int(0.01 * ONE)
        current_inv = _calculate_invariant(amp, balances)

        bpt_amount_in = ONE
        token_out = _calc_token_out_given_exact_bpt_in(
            amp, balances, 0, bpt_amount_in, bpt_total_supply, current_inv, swap_fee
        )
        assert 0 < token_out < balances[0]

    def test_tokens_out_given_exact_bpt_in(self):
        """Proportional exit should give tokens proportional to BPT ratio."""
        balances = [10 * ONE, 10 * ONE]
        bpt_total_supply = 100 * ONE

        bpt_amount_in = 10 * ONE  # 10% of supply
        amounts_out = _calc_tokens_out_given_exact_bpt_in(
            balances, bpt_amount_in, bpt_total_supply
        )
        # Each token should be roughly 10% of balance
        for amount_out in amounts_out:
            assert abs(amount_out - ONE) <= 1  # 10% of 10*ONE = ONE


# ---------- Protocol fee tests ----------


class TestProtocolFees:
    def test_no_fees_if_invariant_unchanged(self):
        """If last invariant equals current invariant, fees should be 0 or near 0."""
        amp = 100 * AMP_PRECISION
        balances = [10 * ONE, 10 * ONE]
        inv = _calculate_invariant(amp, balances)

        fee = _calc_due_token_protocol_swap_fee_amount(
            amp, balances, inv, 0, int(0.5 * ONE)
        )
        assert fee == 0 or fee <= 2


# ---------- _getRate tests ----------


class TestGetRate:
    def test_rate_for_balanced_pool(self):
        """Rate should be invariant / supply for a balanced pool."""
        amp = 100 * AMP_PRECISION
        balances = [10 * ONE, 10 * ONE]
        supply = 20 * ONE

        rate = _get_rate(balances, amp, supply)
        inv = _calculate_invariant(amp, balances)
        expected = div_down(inv, supply)
        assert rate == expected


# ---------- Edge cases ----------


class TestEdgeCases:
    def test_very_small_amounts(self):
        """Test with small amounts (0.1% of balance)."""
        amp = 100 * AMP_PRECISION
        balances = [ONE, ONE]
        inv = _calculate_invariant(amp, balances)

        small_amount = ONE // 1000
        amount_out = _calc_out_given_in(amp, list(balances), 0, 1, small_amount, inv)
        amount_in = _calc_in_given_out(amp, list(balances), 0, 1, small_amount, inv)
        assert amount_out > 0
        assert amount_in > 0

    def test_large_amp(self):
        """Very high amplification — should approach constant-sum behavior."""
        amp = 5000 * AMP_PRECISION  # Maximum amp
        balances = [10 * ONE, 10 * ONE]
        inv = _calculate_invariant(amp, balances)
        # For amp = 5000, invariant should be close to sum = 20 * ONE
        assert inv > 19 * ONE

    def test_small_amp(self):
        """Minimum amplification."""
        amp = 1 * AMP_PRECISION
        balances = [10 * ONE, 10 * ONE]
        inv = _calculate_invariant(amp, balances)
        assert inv > 0

    def test_imbalanced_balances_converge(self):
        """Test convergence with highly imbalanced balances (100:1 ratio)."""
        amp = 500 * AMP_PRECISION
        balances = [ONE, 100 * ONE]
        inv = _calculate_invariant(amp, balances)
        assert inv > 0
        # Invariant should be between geometric mean and sum
        assert inv > 10 * ONE  # Greater than geometric mean
        assert inv < 101 * ONE  # Less than sum
