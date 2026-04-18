"""
Tests for the Balancer weighted pool closed-form arbitrage solver.

Validates:
- Trade signature generation
- Closed-form solution correctness (Equation 9)
- Decimal scaling / descaling
- Integer refinement
- Profit calculation in token units
- Full solver integration
"""

from fractions import Fraction

import pytest

from degenbot.arbitrage.optimizers.balancer_weighted import (
    BalancerMultiTokenState,
    BalancerWeightedPoolSolver,
    MultiTokenArbitrageResult,
    TradeSignature,
    compute_optimal_trade,
    compute_profit_token_units,
    generate_trade_signatures,
    refine_to_integer,
    validate_trade,
)

# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

# 3-token pool: WETH (50%), USDC (25%), DAI (25%)
# Equilibrium pool: 100 WETH @ $2000, 2M USDC @ $1, 1M DAI @ $1
# Pool value = $200k + $2M + $1M = $3.2M
# Implied WETH price = V * w_weth / R_weth = 3.2M * 0.5 / 100 = $16k
# NOT at equilibrium with (2000, 1, 1) prices
#
# For TRUE equilibrium: reserves must satisfy R_i = V * w_i / m_p,i
# V = total value, choose V = $4M
# R_weth = 4M * 0.5 / 2000 = 1000 WETH
# R_usdc = 4M * 0.25 / 1 = 1M USDC
# R_dai  = 4M * 0.25 / 1 = 1M DAI

EQUILIBRIUM_POOL_3TOKEN = BalancerMultiTokenState(
    reserves=(1_000_000_000_000_000_000_000, 1_000_000_000_000, 1_000_000_000_000),
    weights=(500_000_000_000_000_000, 250_000_000_000_000_000, 250_000_000_000_000_000),
    fee=Fraction(3, 1000),
    decimals=(18, 6, 6),
)

EQUILIBRIUM_PRICES = (2000.0, 1.0, 1.0)

# Mispriced pool: ETH over-represented (150 WETH instead of 1000)
MISPRICED_POOL_3TOKEN = BalancerMultiTokenState(
    reserves=(150_000_000_000_000_000_000, 1_000_000_000_000, 1_000_000_000_000),
    weights=(500_000_000_000_000_000, 250_000_000_000_000_000, 250_000_000_000_000_000),
    fee=Fraction(3, 1000),
    decimals=(18, 6, 6),
)

# Pool without decimals (all 18-decimal)
POOL_NO_DECIMALS = BalancerMultiTokenState(
    reserves=(
        1_000_000_000_000_000_000_000,
        1_000_000_000_000_000_000_000,
        1_000_000_000_000_000_000_000,
    ),
    weights=(500_000_000_000_000_000, 250_000_000_000_000_000, 250_000_000_000_000_000),
    fee=Fraction(3, 1000),
)

# 4-token pool
POOL_4TOKEN = BalancerMultiTokenState(
    reserves=(500_000_000_000_000_000_000, 1_000_000_000_000, 1_000_000_000_000, 500_000_000_000),
    weights=(
        400_000_000_000_000_000,
        200_000_000_000_000_000,
        200_000_000_000_000_000,
        200_000_000_000_000_000,
    ),
    fee=Fraction(3, 1000),
    decimals=(18, 6, 6, 6),
)


# ---------------------------------------------------------------------------
# Test Signature Generation
# ---------------------------------------------------------------------------


class TestSignatureGeneration:
    def test_n3_signature_count(self):
        """N=3 should have 12 valid signatures."""
        signatures = generate_trade_signatures(3)
        assert len(signatures) == 12

    def test_n4_signature_count(self):
        """N=4 should have 50 valid signatures."""
        signatures = generate_trade_signatures(4)
        assert len(signatures) == 50

    def test_n5_signature_count(self):
        """N=5 should have 180 valid signatures."""
        signatures = generate_trade_signatures(5)
        assert len(signatures) == 180

    def test_all_signatures_valid(self):
        """All generated signatures should have at least one +1 and one -1."""
        signatures = generate_trade_signatures(4)
        for sig in signatures:
            assert 1 in sig
            assert -1 in sig

    def test_signature_examples(self):
        """Check some known signatures exist."""
        signatures = generate_trade_signatures(3)
        assert (1, -1, -1) in signatures
        assert (1, 1, -1) in signatures
        assert (1, -1, 0) in signatures


# ---------------------------------------------------------------------------
# Test Decimal Scaling
# ---------------------------------------------------------------------------


class TestDecimalScaling:
    def test_upscaled_reserves_same_decimals(self):
        """When all tokens have 18 decimals, upscaled reserves should be unchanged."""
        pool = POOL_NO_DECIMALS
        upscaled = pool.upscaled_reserves()
        for i in range(3):
            assert upscaled[i] == pytest.approx(float(pool.reserves[i]))

    def test_upscaled_reserves_different_decimals(self):
        """USDC (6 dec) should be upscaled by 1e12."""
        pool = EQUILIBRIUM_POOL_3TOKEN
        upscaled = pool.upscaled_reserves()
        # ETH (18 dec): no scaling
        assert upscaled[0] == pytest.approx(float(pool.reserves[0]))
        # USDC (6 dec): scale by 1e12
        assert upscaled[1] == pytest.approx(float(pool.reserves[1]) * 1e12)

    def test_descale_roundtrip(self):
        """Descale(upscale(x)) should approximately equal x."""
        pool = EQUILIBRIUM_POOL_3TOKEN
        upscaled = pool.upscaled_reserves()
        for i in range(3):
            descaled = pool.descale_trade(upscaled[i], i)
            assert descaled == pool.reserves[i]


# ---------------------------------------------------------------------------
# Test Closed-form Solution (Equation 9)
# ---------------------------------------------------------------------------


class TestClosedFormSolution:
    def test_zero_trade_at_equilibrium_no_fees(self):
        """At equilibrium with zero fees, trade should be exactly zero."""
        pool = BalancerMultiTokenState(
            reserves=(1_000_000_000_000_000_000_000, 1_000_000_000_000, 1_000_000_000_000),
            weights=(500_000_000_000_000_000, 250_000_000_000_000_000, 250_000_000_000_000_000),
            fee=Fraction(0, 1),
            decimals=(18, 6, 6),
        )
        prices = (2000.0, 1.0, 1.0)
        signature = (1, -1, -1)

        trades = compute_optimal_trade(pool, prices, signature)

        # All trades should be near zero (floating point precision)
        for t in trades:
            # In upscaled 18-decimal, 1e8 ≈ 1e-10 tokens — negligible
            assert abs(t) < 1e10

    def test_nonzero_trade_at_mispricing(self):
        """When pool is mispriced, trade should be nonzero."""
        # ETH cheap on market: deposit ETH, withdraw stables
        prices = (1900.0, 1.0, 1.0)
        signature = (1, -1, -1)

        trades = compute_optimal_trade(EQUILIBRIUM_POOL_3TOKEN, prices, signature)

        # Token 0 should be deposited (positive)
        assert trades[0] > 0
        # Token 1 should be withdrawn (negative)
        assert trades[1] < 0
        # Token 2 should be withdrawn (negative)
        assert trades[2] < 0

    def test_invariant_preserved_after_trade(self):
        """The AMM invariant should be preserved after applying the trade."""
        prices = (1900.0, 1.0, 1.0)
        signature = (1, -1, -1)

        trades = compute_optimal_trade(EQUILIBRIUM_POOL_3TOKEN, prices, signature)

        assert validate_trade(trades, signature, EQUILIBRIUM_POOL_3TOKEN)

    def test_trade_direction_matches_signature(self):
        """Trade signs should match their signature direction."""
        # When ETH is cheap: deposit ETH (+1), withdraw stables (-1)
        prices = (1900.0, 1.0, 1.0)
        signature: TradeSignature = (1, -1, -1)

        trades = compute_optimal_trade(EQUILIBRIUM_POOL_3TOKEN, prices, signature)

        # Token 0: deposit (positive)
        assert trades[0] > 0
        # Token 1: withdraw (negative)
        assert trades[1] < 0
        # Token 2: withdraw (negative)
        assert trades[2] < 0

    def test_opposite_mispricing_gives_opposite_direction(self):
        """When ETH is expensive on market, should withdraw ETH."""
        prices = (2100.0, 1.0, 1.0)
        signature: TradeSignature = (-1, 1, 1)

        trades = compute_optimal_trade(EQUILIBRIUM_POOL_3TOKEN, prices, signature)

        # Token 0: withdraw (negative)
        assert trades[0] < 0
        # Token 1: deposit (positive)
        assert trades[1] > 0
        # Token 2: deposit (positive)
        assert trades[2] > 0


# ---------------------------------------------------------------------------
# Test Validation
# ---------------------------------------------------------------------------


class TestTradeValidation:
    def test_wrong_direction_invalid(self):
        """Trade with wrong direction should fail validation."""
        signature: TradeSignature = (1, -1, 0)
        # Wrong: negative when should be positive
        trades = (-1e18, -500_000_000_000, 0.0)

        is_valid = validate_trade(trades, signature, EQUILIBRIUM_POOL_3TOKEN)
        assert not is_valid

    def test_over_withdrawal_invalid(self):
        """Withdrawing more than reserve should fail validation."""
        signature: TradeSignature = (1, -1, 0)
        # Withdraw more USDC than exists
        trades = (1e18, -10_000_000_000_000, 0.0)

        is_valid = validate_trade(trades, signature, EQUILIBRIUM_POOL_3TOKEN)
        assert not is_valid

    def test_valid_trade_passes(self):
        """A correct trade from the formula should pass validation."""
        prices = (1900.0, 1.0, 1.0)
        signature = (1, -1, -1)
        trades = compute_optimal_trade(EQUILIBRIUM_POOL_3TOKEN, prices, signature)

        assert validate_trade(trades, signature, EQUILIBRIUM_POOL_3TOKEN)


# ---------------------------------------------------------------------------
# Test Integer Refinement
# ---------------------------------------------------------------------------


class TestIntegerRefinement:
    def test_refinement_produces_integers(self):
        """Refinement should produce integer trades."""
        prices = (1900.0, 1.0, 1.0)
        signature: TradeSignature = (1, -1, -1)
        float_trades = compute_optimal_trade(EQUILIBRIUM_POOL_3TOKEN, prices, signature)

        int_trades = refine_to_integer(float_trades, signature, EQUILIBRIUM_POOL_3TOKEN, prices)

        for t in int_trades:
            assert isinstance(t, int)

    def test_refinement_preserves_direction(self):
        """Refinement should preserve trade direction."""
        prices = (1900.0, 1.0, 1.0)
        signature: TradeSignature = (1, -1, -1)
        float_trades = compute_optimal_trade(EQUILIBRIUM_POOL_3TOKEN, prices, signature)

        int_trades = refine_to_integer(float_trades, signature, EQUILIBRIUM_POOL_3TOKEN, prices)

        for _i, (t, s) in enumerate(zip(int_trades, signature, strict=True)):
            if s == 1:
                assert t >= 0
            elif s == -1:
                assert t <= 0


# ---------------------------------------------------------------------------
# Test Profit Calculation
# ---------------------------------------------------------------------------


class TestProfitCalculation:
    def test_zero_trades_zero_profit(self):
        """Zero trades should give zero profit."""
        trades = (0.0, 0.0, 0.0)
        profit = compute_profit_token_units(trades, EQUILIBRIUM_PRICES)
        assert profit == pytest.approx(0.0)

    def test_deposit_costs_money(self):
        """Depositing (positive trade) should cost money."""
        # Deposit 1 WETH: upscaled = 1e18 (18-decimal), token amount = 1.0
        trades = (1e18, 0.0, 0.0)
        profit = compute_profit_token_units(trades, EQUILIBRIUM_PRICES)
        # Profit = -sum(price * token_amount) = -2000 * 1.0 = -2000
        assert profit == pytest.approx(-2000.0)

    def test_withdrawal_gives_money(self):
        """Withdrawing (negative trade) should give money."""
        # Withdraw 1 WETH: upscaled = -1e18, token amount = -1.0
        trades = (-1e18, 0.0, 0.0)
        profit = compute_profit_token_units(trades, EQUILIBRIUM_PRICES)
        # Profit = -sum(price * token_amount) = -2000 * (-1.0) = 2000
        assert profit == pytest.approx(2000.0)

    def test_usdc_profit_uses_token_units(self):
        """USDC trades should be divided by 1e6 (not 1e18) for token units."""
        # Deposit 1M USDC (6 decimals): reserve = 1_000_000e6 = 1e12
        # Upscaled = 1e12 * 1e12 = 1e24 (in 18-decimal)
        # Token amount = 1e24 / 1e18 = 1e6 → 1M USDC tokens
        trades = (0.0, 1e24, 0.0)
        profit = compute_profit_token_units(trades, EQUILIBRIUM_PRICES)
        # Profit = -1.0 * 1_000_000 = -1_000_000
        assert profit == pytest.approx(-1_000_000.0)


# ---------------------------------------------------------------------------
# Test Full Solver
# ---------------------------------------------------------------------------


class TestBalancerWeightedPoolSolver:
    def test_equilibrium_no_profit(self):
        """At equilibrium, solver should find no profitable trade."""
        solver = BalancerWeightedPoolSolver()
        result = solver.solve(EQUILIBRIUM_POOL_3TOKEN, EQUILIBRIUM_PRICES)

        # At equilibrium with fees, no profitable trade should exist
        assert not result.success or result.profit < 1.0

    def test_mispricing_finds_profit(self):
        """When ETH is cheap on market, should find profitable trade."""
        solver = BalancerWeightedPoolSolver()
        prices = (1900.0, 1.0, 1.0)  # ETH 5% cheaper than pool
        result = solver.solve(EQUILIBRIUM_POOL_3TOKEN, prices)

        assert result.success
        assert result.profit > 0

    def test_solver_returns_result(self):
        """Solver should return a result object."""
        solver = BalancerWeightedPoolSolver()
        result = solver.solve(EQUILIBRIUM_POOL_3TOKEN, EQUILIBRIUM_PRICES)

        assert isinstance(result, MultiTokenArbitrageResult)
        assert len(result.trades) == 3
        assert isinstance(result.profit, float)
        assert isinstance(result.success, bool)

    def test_solver_n4(self):
        """Solver should handle 4-token pools."""
        solver = BalancerWeightedPoolSolver()
        prices = (2000.0, 1.0, 1.0, 1.0)
        result = solver.solve(POOL_4TOKEN, prices)

        assert len(result.trades) == 4

    def test_profit_in_dollar_units(self):
        """Profit should be in reasonable dollar units, not wei."""
        solver = BalancerWeightedPoolSolver()
        prices = (1900.0, 1.0, 1.0)  # ETH 5% cheaper
        result = solver.solve(EQUILIBRIUM_POOL_3TOKEN, prices)

        if result.success:
            # Profit should be in thousands of dollars, not 10^20
            assert 1.0 < result.profit < 1_000_000.0

    def test_multi_token_mispricing(self):
        """When multiple tokens are mispriced, should find profitable trade."""
        solver = BalancerWeightedPoolSolver()
        prices = (2100.0, 0.95, 0.90)  # ETH expensive, stables cheap
        result = solver.solve(EQUILIBRIUM_POOL_3TOKEN, prices)

        assert result.success
        assert result.profit > 0


# ---------------------------------------------------------------------------
# Test Edge Cases
# ---------------------------------------------------------------------------


class TestEdgeCases:
    def test_price_mismatch_length(self):
        """Prices with wrong length should return failure."""
        solver = BalancerWeightedPoolSolver()
        result = solver.solve(EQUILIBRIUM_POOL_3TOKEN, (2000.0, 1.0))

        assert not result.success

    def test_no_decimals_pool(self):
        """Pool without decimals should still work (all 18-decimal assumed)."""
        solver = BalancerWeightedPoolSolver()
        result = solver.solve(POOL_NO_DECIMALS, (2000.0, 1.0, 1.0))

        assert isinstance(result, MultiTokenArbitrageResult)
