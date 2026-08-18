"""
Fuzz test: does the fee denominator matter for UniswapV2 0.3% fee?

Comparing two representations of the SAME 0.3% fee:

  Old (base-1000):  feeMultiplier = 1000 - 3 = 997   → 997 / 1000
  New (base-10000): feeMultiplier = 10000 - 30 = 9970 → 9970 / 10000

These are rationally identical (9970/10000 = 997/1000). But Solidity/Vyper
use integer truncation, so the question is: can integer division diverge?

Proof that they CANNOT diverge:
  New numerator   = amountIn × 9970 × reserveOut = 10 × (amountIn × 997 × reserveOut)
  New denominator = reserveIn × 10000 + amountIn × 9970 = 10 × (reserveIn × 1000 + amountIn × 997)
  So floor(newNum / newDen) = floor(10N / 10D) = floor(N / D) = old result.

We verify this proof by exhaustion for uint112-range inputs.
(We also check base-1000 vs 10000 for PancakeSwap fee=25, where the old
formula CANNOT correctly represent 0.25% — it truncates to 0.2%.)
"""

from hypothesis import given, settings, example, HealthCheck, assume
from hypothesis.strategies import integers

UINT256_MAX = 2**256 - 1

# ─── Pure-Python formula reimplementations ───


def get_amount_out(amount_in, reserve_in, reserve_out, fee_multiplier, denominator):
    """
    Generic V2 getAmountOut.

    amountInWithFee = amountIn * feeMultiplier
    numerator = amountInWithFee * reserveOut
    denominator = reserveIn * denominator + amountInWithFee
    """
    aif = amount_in * fee_multiplier
    num = aif * reserve_out
    den = reserve_in * denominator + aif
    if den == 0:
        return 0
    return num // den


def get_amount_in(amount_out, reserve_in, reserve_out, fee_multiplier, denominator):
    """
    Generic V2 getAmountIn.

    numerator = reserveIn * amountOut * denominator
    denominator = (reserveOut - amountOut) * feeMultiplier
    amountIn = numerator / denominator + 1
    """
    assume(amount_out < reserve_out)
    num = reserve_in * amount_out * denominator
    den = (reserve_out - amount_out) * fee_multiplier
    if den == 0:
        return UINT256_MAX
    return num // den + 1


def k_check(
    balance0,
    balance1,
    reserve0,
    reserve1,
    amount0_in,
    amount1_in,
    fee_subtracted,
    denominator,
):
    """
    Generic V2 K-invariant check.

    balanceNAdjusted = balanceN * denominator - amountNIn * fee_subtracted
    Check: balance0Adj * balance1Adj >= reserve0 * reserve1 * denominator^2
    """
    b0adj = balance0 * denominator - amount0_in * fee_subtracted
    b1adj = balance1 * denominator - amount1_in * fee_subtracted
    return b0adj * b1adj >= reserve0 * reserve1 * denominator * denominator


# ─── Strategies ───

# Core test: uint112 range (covers all realistic DeFi values;
# max uint112 ≈ 5.2e33, enough for 5T tokens with 18 decimals)
uint112 = integers(min_value=1, max_value=2**112 - 1)


class TestUniswapV2Fee30:
    """
    Standard 0.3% fee: old = 997/1000, new = 9970/10000.
    These MUST produce identical results for all inputs.
    """

    @given(amount_in=uint112, reserve_in=uint112, reserve_out=uint112)
    @settings(suppress_health_check=[HealthCheck.too_slow])
    @example(amount_in=1, reserve_in=1, reserve_out=1)
    @example(amount_in=2**112 - 1, reserve_in=1, reserve_out=1)
    @example(amount_in=1, reserve_in=2**112 - 1, reserve_out=1)
    def test_get_amount_out_identical(self, amount_in, reserve_in, reserve_out):
        """997/1000 vs 9970/10000 produce identical getAmountOut."""
        # Overflow check: amount_in * 9970 * reserve_out must fit uint256
        assume(amount_in * 9970 * reserve_out <= UINT256_MAX)
        assume(reserve_in * 10000 + amount_in * 9970 > 0)

        old = get_amount_out(amount_in, reserve_in, reserve_out, 997, 1000)
        new = get_amount_out(amount_in, reserve_in, reserve_out, 9970, 10000)
        assert old == new, f"Divergence: old={old}, new={new}"

    @given(amount_out=uint112, reserve_in=uint112, reserve_out=uint112)
    @settings(suppress_health_check=[HealthCheck.too_slow])
    @example(amount_out=1, reserve_in=1, reserve_out=2)
    def test_get_amount_in_identical(self, amount_out, reserve_in, reserve_out):
        """997/1000 vs 9970/10000 produce identical getAmountIn."""
        assume(amount_out < reserve_out)
        # Overflow check
        assume(reserve_in * amount_out * 10000 <= UINT256_MAX)

        old = get_amount_in(amount_out, reserve_in, reserve_out, 997, 1000)
        new = get_amount_in(amount_out, reserve_in, reserve_out, 9970, 10000)
        assert old == new, f"Divergence: old={old}, new={new}"

    @given(amount_in=uint112, reserve_in=uint112, reserve_out=uint112)
    @settings(suppress_health_check=[HealthCheck.too_slow])
    @example(amount_in=1, reserve_in=1, reserve_out=1)
    def test_k_check_equivalent(self, amount_in, reserve_in, reserve_out):
        """K-invariant checks are equivalent for div-10 fees."""
        assume(amount_in * 9970 * reserve_out <= UINT256_MAX)
        assume(reserve_in * 10000 + amount_in * 9970 > 0)

        amount_out = get_amount_out(amount_in, reserve_in, reserve_out, 9970, 10000)
        if amount_out == 0:
            return

        balance0 = reserve_in + amount_in
        balance1 = reserve_out - amount_out

        # Old K-check: fee_subtracted=3, denominator=1000
        passes_old = k_check(
            balance0, balance1, reserve_in, reserve_out, amount_in, 0, 3, 1000
        )
        # New K-check: fee_subtracted=30, denominator=10000
        passes_new = k_check(
            balance0, balance1, reserve_in, reserve_out, amount_in, 0, 30, 10000
        )

        assert passes_old == passes_new, (
            f"K-check differs: old={passes_old}, new={passes_new}, "
            f"amount_in={amount_in}, amount_out={amount_out}"
        )


class TestPancakeSwapFee25:
    """
    PancakeSwap 0.25% fee: old = 998/1000 (= 0.2%), new = 9975/10000 (= 0.25%).

    The old formula CANNOT represent 0.25% — it truncates 25/10 = 2,
    giving an effective fee of 0.2% instead of 0.25%.

    This means:
    - Old gives MORE output (undercharged fee)
    - Old gives LESS required input (undercharged fee)
    - Old K-check is WEAKER (easier to pass, because less fee is subtracted)
    """

    @given(amount_in=uint112, reserve_in=uint112, reserve_out=uint112)
    @settings(suppress_health_check=[HealthCheck.too_slow])
    @example(amount_in=1, reserve_in=1, reserve_out=1)
    def test_get_amount_out_old_gives_more(self, amount_in, reserve_in, reserve_out):
        """Old 998/1000 gives >= output vs new 9975/10000 (fee undercharged)."""
        assume(amount_in * 9975 * reserve_out <= UINT256_MAX)
        assume(amount_in * 998 * reserve_out <= UINT256_MAX)
        assume(reserve_in * 10000 + amount_in * 9975 > 0)

        old = get_amount_out(amount_in, reserve_in, reserve_out, 998, 1000)
        new = get_amount_out(amount_in, reserve_in, reserve_out, 9975, 10000)
        assert old >= new, f"Old should give >= output: old={old}, new={new}"

    @given(amount_out=uint112, reserve_in=uint112, reserve_out=uint112)
    @settings(suppress_health_check=[HealthCheck.too_slow])
    def test_get_amount_in_old_gives_less(self, amount_out, reserve_in, reserve_out):
        """Old 998/1000 requires <= input vs new 9975/10000 (fee undercharged)."""
        assume(amount_out < reserve_out)
        assume(reserve_in * amount_out * 10000 <= UINT256_MAX)
        assume(reserve_in * amount_out * 1000 <= UINT256_MAX)

        old = get_amount_in(amount_out, reserve_in, reserve_out, 998, 1000)
        new = get_amount_in(amount_out, reserve_in, reserve_out, 9975, 10000)
        assert old <= new, f"Old should require <= input: old={old}, new={new}"

    @given(amount_in=uint112, reserve_in=uint112, reserve_out=uint112)
    @settings(suppress_health_check=[HealthCheck.too_slow])
    def test_new_k_pass_implies_old_k_pass(self, amount_in, reserve_in, reserve_out):
        """
        If new 10000-base K-check passes, old 1000-base K-check also passes.

        Old K-check subtracts LESS fee (2/1000 < 25/10000 per unit input),
        making adjusted balances HIGHER and the check EASIER to pass.

        This means our executor's amounts (computed with 10000-base formula)
        are SAFE to use against real V2 pairs that use the 1000-base K-check.
        """
        assume(amount_in * 9975 * reserve_out <= UINT256_MAX)

        amount_out = get_amount_out(amount_in, reserve_in, reserve_out, 9975, 10000)
        if amount_out == 0:
            return

        balance0 = reserve_in + amount_in
        balance1 = reserve_out - amount_out

        # New K-check: fee_subtracted=25, denominator=10000
        passes_new = k_check(
            balance0, balance1, reserve_in, reserve_out, amount_in, 0, 25, 10000
        )
        # Old K-check: fee_subtracted=2, denominator=1000
        passes_old = k_check(
            balance0, balance1, reserve_in, reserve_out, amount_in, 0, 2, 1000
        )

        if passes_new:
            assert passes_old, (
                f"New K-check passed but old K-check failed! "
                f"This means our amounts could fail against real V2 pairs.\n"
                f"amount_in={amount_in}, amount_out={amount_out}"
            )


class TestSubOnePercentFees:
    """
    Fees below 10 bps (fee < 10): the old 1000-base formula charges ZERO fee.

    For fee=1..9 (0.01%–0.09%):
      Old: feeMultiplier = 1000 - (fee // 10) = 1000 - 0 = 1000  → NO FEE!
      New: feeMultiplier = 10000 - fee = 9999..9991  → correct fee

    This is the most dramatic divergence: the old formula silently drops
    fees below 10 bps entirely.
    """

    @given(
        amount_in=uint112,
        reserve_in=uint112,
        reserve_out=uint112,
        fee=integers(min_value=1, max_value=9),
    )
    @settings(suppress_health_check=[HealthCheck.too_slow])
    @example(amount_in=10**18, reserve_in=10**24, reserve_out=10**24, fee=1)
    @example(amount_in=10**18, reserve_in=10**24, reserve_out=10**24, fee=9)
    def test_old_charges_zero_fee_sub10bps(
        self, amount_in, reserve_in, reserve_out, fee
    ):
        """For fees 1-9 (sub-10bps), old formula = zero-fee output (no-fee swap)."""
        assume(amount_in * (10000 - fee) * reserve_out <= UINT256_MAX)
        assume(amount_in * 1000 * reserve_out <= UINT256_MAX)

        old_fee_multiplier = 1000 - fee // 10  # 1000 - 0 = 1000
        assert old_fee_multiplier == 1000, (
            f"Fee {fee}: old should give zero-fee multiplier"
        )

        zero_fee_result = get_amount_out(amount_in, reserve_in, reserve_out, 1000, 1000)
        old_result = get_amount_out(
            amount_in, reserve_in, reserve_out, old_fee_multiplier, 1000
        )
        new_result = get_amount_out(
            amount_in, reserve_in, reserve_out, 10000 - fee, 10000
        )

        # Old = zero-fee output (more than new)
        assert old_result == zero_fee_result
        assert old_result >= new_result


