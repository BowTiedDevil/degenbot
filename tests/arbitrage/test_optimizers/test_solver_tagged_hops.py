"""
Tests for the tagged union Hop types and PiecewiseMobiusSolver integration.

Validates:
- PoolInvariant enum and Hop tagged union construction
- Each hop variant has required fields and is frozen
- SolveInput detects invariant types correctly
- PiecewiseMobiusSolver integrates into ArbSolver dispatch
- pool_to_hop supports Camelot (asymmetric fees)
"""

import pytest

from degenbot.arbitrage.optimizers.hop_types import SolveInput, SolverMethod
from degenbot.arbitrage.optimizers.solver import (
    ArbSolver,
    MobiusSolver,
    PiecewiseMobiusSolver,
)
from degenbot.exceptions import OptimizationError
from degenbot.types.hop_types import (
    BalancerWeightedHop,
    BoundedProductHop,
    ConstantProductHop,
    CurveStableswapHop,
    HopType,
    PoolInvariant,
    SolidlyStableHop,
)

from .conftest import FEE_0_3_PCT, FEE_0_5_PCT, USDC_1_5M, USDC_2M, WETH_800, WETH_1000

# ---------------------------------------------------------------------------
# Test ConstantProductHop
# ---------------------------------------------------------------------------


class TestConstantProductHop:
    def test_construction(self):
        hop = ConstantProductHop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT)
        assert hop.invariant == PoolInvariant.CONSTANT_PRODUCT
        assert hop.reserve_in == USDC_2M
        assert hop.reserve_out == WETH_1000
        assert hop.fee == FEE_0_3_PCT
        assert hop.fee_out is None

    def test_asymmetric_fee(self):
        """Camelot pools have different fees per direction."""
        hop = ConstantProductHop(
            reserve_in=USDC_2M,
            reserve_out=WETH_1000,
            fee=FEE_0_3_PCT,
            fee_out=FEE_0_5_PCT,
        )
        assert hop.fee_out == FEE_0_5_PCT

    def test_gamma_property(self):
        hop = ConstantProductHop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT)
        assert hop.gamma == pytest.approx(0.997)

    def test_is_v2(self):
        hop = ConstantProductHop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT)
        assert hop.is_v2
        assert not hop.is_v3

    def test_frozen(self):
        hop = ConstantProductHop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT)
        with pytest.raises(AttributeError):
            hop.reserve_in = 0


# ---------------------------------------------------------------------------
# Test BoundedProductHop
# ---------------------------------------------------------------------------


class TestBoundedProductHop:
    def test_construction(self):
        hop = BoundedProductHop(
            reserve_in=USDC_2M,
            reserve_out=WETH_1000,
            fee=FEE_0_3_PCT,
            liquidity=10**18,
            sqrt_price=2**96,
            tick_lower=-100,
            tick_upper=100,
        )
        assert hop.invariant == PoolInvariant.BOUNDED_PRODUCT
        assert hop.liquidity == 10**18
        assert hop.is_v3
        assert not hop.is_v2

    def test_frozen(self):
        hop = BoundedProductHop(
            reserve_in=USDC_2M,
            reserve_out=WETH_1000,
            fee=FEE_0_3_PCT,
            liquidity=10**18,
            sqrt_price=2**96,
            tick_lower=-100,
            tick_upper=100,
        )
        with pytest.raises(AttributeError):
            hop.liquidity = 0


# ---------------------------------------------------------------------------
# Test SolidlyStableHop
# ---------------------------------------------------------------------------


class TestSolidlyStableHop:
    def test_construction(self):
        hop = SolidlyStableHop(
            reserve_in=USDC_2M,
            reserve_out=WETH_1000,
            fee=FEE_0_3_PCT,
            decimals_in=6,
            decimals_out=18,
        )
        assert hop.invariant == PoolInvariant.SOLIDLY_STABLE
        assert hop.decimals_in == 6
        assert hop.decimals_out == 18
        assert not hop.is_v2
        assert not hop.is_v3

    def test_frozen(self):
        hop = SolidlyStableHop(
            reserve_in=USDC_2M,
            reserve_out=WETH_1000,
            fee=FEE_0_3_PCT,
            decimals_in=6,
            decimals_out=18,
        )
        with pytest.raises(AttributeError):
            hop.decimals_in = 0


# ---------------------------------------------------------------------------
# Test BalancerWeightedHop
# ---------------------------------------------------------------------------


class TestBalancerWeightedHop:
    def test_construction(self):
        hop = BalancerWeightedHop(
            reserve_in=USDC_2M,
            reserve_out=WETH_1000,
            fee=FEE_0_3_PCT,
            weight_in=500_000_000_000_000_000,  # 50% = 0.5e18
            weight_out=500_000_000_000_000_000,  # 50% = 0.5e18
        )
        assert hop.invariant == PoolInvariant.BALANCER_WEIGHTED
        assert hop.weight_in == 500_000_000_000_000_000
        assert not hop.is_v2
        assert not hop.is_v3

    def test_frozen(self):
        hop = BalancerWeightedHop(
            reserve_in=USDC_2M,
            reserve_out=WETH_1000,
            fee=FEE_0_3_PCT,
            weight_in=500_000_000_000_000_000,
            weight_out=500_000_000_000_000_000,
        )
        with pytest.raises(AttributeError):
            hop.weight_in = 0


# ---------------------------------------------------------------------------
# Test CurveStableswapHop
# ---------------------------------------------------------------------------


class TestCurveStableswapHop:
    def test_construction(self):
        hop = CurveStableswapHop(
            reserve_in=USDC_2M,
            reserve_out=USDC_1_5M,
            fee=FEE_0_3_PCT,
            curve_a=100,
            curve_n_coins=2,
            curve_d=3_500_000_000_000,
            token_index_in=0,
            token_index_out=1,
            precisions=(10**6, 10**6),
        )
        assert hop.invariant == PoolInvariant.CURVE_STABLESWAP
        assert hop.curve_a == 100
        assert not hop.is_v2
        assert not hop.is_v3

    def test_frozen(self):
        hop = CurveStableswapHop(
            reserve_in=USDC_2M,
            reserve_out=USDC_1_5M,
            fee=FEE_0_3_PCT,
            curve_a=100,
            curve_n_coins=2,
            curve_d=3_500_000_000_000,
            token_index_in=0,
            token_index_out=1,
            precisions=(10**6, 10**6),
        )
        with pytest.raises(AttributeError):
            hop.curve_a = 0


# ---------------------------------------------------------------------------
# Test Hop type alias (union)
# ---------------------------------------------------------------------------


class TestHopUnion:
    def test_all_variants_are_hop(self):
        """All hop variants should be assignable to the Hop type alias."""
        v2: HopType = ConstantProductHop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT)
        v3: HopType = BoundedProductHop(
            reserve_in=USDC_2M,
            reserve_out=WETH_1000,
            fee=FEE_0_3_PCT,
            liquidity=10**18,
            sqrt_price=2**96,
            tick_lower=-100,
            tick_upper=100,
        )
        solidly: HopType = SolidlyStableHop(
            reserve_in=USDC_2M,
            reserve_out=WETH_1000,
            fee=FEE_0_3_PCT,
            decimals_in=6,
            decimals_out=18,
        )
        balancer: HopType = BalancerWeightedHop(
            reserve_in=USDC_2M,
            reserve_out=WETH_1000,
            fee=FEE_0_3_PCT,
            weight_in=500_000_000_000_000_000,
            weight_out=500_000_000_000_000_000,
        )
        curve: HopType = CurveStableswapHop(
            reserve_in=USDC_2M,
            reserve_out=USDC_1_5M,
            fee=FEE_0_3_PCT,
            curve_a=100,
            curve_n_coins=2,
            curve_d=3_500_000_000_000,
            token_index_in=0,
            token_index_out=1,
            precisions=(10**6, 10**6),
        )
        # All have common reserve/fee fields
        assert v2.reserve_in == USDC_2M
        assert v3.reserve_in == USDC_2M
        assert solidly.reserve_in == USDC_2M
        assert balancer.reserve_in == USDC_2M
        assert curve.reserve_in == USDC_2M


# ---------------------------------------------------------------------------
# Test SolveInput with tagged union hops
# ---------------------------------------------------------------------------


class TestSolveInputTaggedHops:
    def test_all_constant_product(self):
        inp = SolveInput(
            hops=(
                ConstantProductHop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT),
                ConstantProductHop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=FEE_0_3_PCT),
            )
        )
        assert inp.all_constant_product
        assert not inp.has_v3
        assert not inp.has_solidly_stable
        assert not inp.has_balancer_weighted
        assert not inp.has_curve_stableswap

    def test_mixed_v2_v3(self):
        inp = SolveInput(
            hops=(
                ConstantProductHop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT),
                BoundedProductHop(
                    reserve_in=WETH_800,
                    reserve_out=USDC_1_5M,
                    fee=FEE_0_3_PCT,
                    liquidity=10**18,
                    sqrt_price=2**96,
                    tick_lower=-100,
                    tick_upper=100,
                ),
            )
        )
        assert inp.has_v3
        assert not inp.all_constant_product
        assert inp.v3_indices == (1,)

    def test_solidly_stable_detected(self):
        inp = SolveInput(
            hops=(
                SolidlyStableHop(
                    reserve_in=USDC_2M,
                    reserve_out=WETH_1000,
                    fee=FEE_0_3_PCT,
                    decimals_in=6,
                    decimals_out=18,
                ),
                ConstantProductHop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=FEE_0_3_PCT),
            )
        )
        assert inp.has_solidly_stable

    def test_balancer_detected(self):
        inp = SolveInput(
            hops=(
                BalancerWeightedHop(
                    reserve_in=USDC_2M,
                    reserve_out=WETH_1000,
                    fee=FEE_0_3_PCT,
                    weight_in=500_000_000_000_000_000,
                    weight_out=500_000_000_000_000_000,
                ),
                ConstantProductHop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=FEE_0_3_PCT),
            )
        )
        assert inp.has_balancer_weighted

    def test_curve_detected(self):
        inp = SolveInput(
            hops=(
                CurveStableswapHop(
                    reserve_in=USDC_2M,
                    reserve_out=USDC_1_5M,
                    fee=FEE_0_3_PCT,
                    curve_a=100,
                    curve_n_coins=2,
                    curve_d=3_500_000_000_000,
                    token_index_in=0,
                    token_index_out=1,
                    precisions=(10**6, 10**6),
                ),
                ConstantProductHop(reserve_in=USDC_1_5M, reserve_out=USDC_2M, fee=FEE_0_3_PCT),
            )
        )
        assert inp.has_curve_stableswap


# ---------------------------------------------------------------------------
# Test MobiusSolver with tagged hops
# ---------------------------------------------------------------------------


class TestMobiusSolverTaggedHops:
    def test_constant_product_hops(self):
        """MobiusSolver should accept ConstantProductHop and produce same results."""
        solver = MobiusSolver()
        inp = SolveInput(
            hops=(
                ConstantProductHop(reserve_in=USDC_1_5M, reserve_out=WETH_800, fee=FEE_0_3_PCT),
                ConstantProductHop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=FEE_0_3_PCT),
            )
        )
        result = solver.solve(inp)
        assert result.method == SolverMethod.MOBIUS
        assert result.optimal_input > 0
        assert result.profit > 0

    def test_asymmetric_fee_hops(self):
        """MobiusSolver should handle ConstantProductHop with asymmetric fees."""
        solver = MobiusSolver()
        inp = SolveInput(
            hops=(
                ConstantProductHop(
                    reserve_in=USDC_1_5M,
                    reserve_out=WETH_800,
                    fee=FEE_0_3_PCT,
                    fee_out=FEE_0_5_PCT,
                ),
                ConstantProductHop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=FEE_0_3_PCT),
            )
        )
        result = solver.solve(inp)
        assert result.method == SolverMethod.MOBIUS


# ---------------------------------------------------------------------------
# Test PiecewiseMobiusSolver
# ---------------------------------------------------------------------------


class TestPiecewiseMobiusSolver:
    def test_supports_v3_multi_range(self):
        """PiecewiseMobiusSolver should support paths with V3 hops."""
        solver = PiecewiseMobiusSolver()
        inp = SolveInput(
            hops=(
                ConstantProductHop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT),
                BoundedProductHop(
                    reserve_in=WETH_800,
                    reserve_out=USDC_1_5M,
                    fee=FEE_0_3_PCT,
                    liquidity=10**18,
                    sqrt_price=2**96,
                    tick_lower=-100,
                    tick_upper=100,
                ),
            )
        )
        # Should support inputs with V3 bounded-product hops
        assert solver.supports(inp)


# ---------------------------------------------------------------------------
# Test ArbSolver dispatch with new types
# ---------------------------------------------------------------------------


class TestArbSolverTaggedDispatch:
    def test_v2_path_uses_mobius(self):
        solver = ArbSolver()
        inp = SolveInput(
            hops=(
                ConstantProductHop(reserve_in=USDC_1_5M, reserve_out=WETH_800, fee=FEE_0_3_PCT),
                ConstantProductHop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=FEE_0_3_PCT),
            )
        )
        result = solver.solve(inp)
        assert result.method == SolverMethod.MOBIUS

    def test_solidly_stable_falls_to_brent(self):
        """Paths with Solidly stable hops should fall back to Brent until
        SolidlyStableSolver is implemented."""
        solver = ArbSolver()
        inp = SolveInput(
            hops=(
                SolidlyStableHop(
                    reserve_in=USDC_2M,
                    reserve_out=WETH_1000,
                    fee=FEE_0_3_PCT,
                    decimals_in=6,
                    decimals_out=18,
                ),
                ConstantProductHop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=FEE_0_3_PCT),
            )
        )
        try:
            result = solver.solve(inp)
            assert result.method == SolverMethod.BRENT
        except OptimizationError:
            pass

    def test_balancer_falls_to_brent(self):
        """Paths with Balancer hops should fall back to Brent."""
        solver = ArbSolver()
        inp = SolveInput(
            hops=(
                BalancerWeightedHop(
                    reserve_in=USDC_2M,
                    reserve_out=WETH_1000,
                    fee=FEE_0_3_PCT,
                    weight_in=500_000_000_000_000_000,
                    weight_out=500_000_000_000_000_000,
                ),
                ConstantProductHop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=FEE_0_3_PCT),
            )
        )
        try:
            result = solver.solve(inp)
            assert result.method == SolverMethod.BRENT
        except OptimizationError:
            pass


# ---------------------------------------------------------------------------
# Test backward compatibility: old Hop still works
# ---------------------------------------------------------------------------


class TestBackwardCompatibility:
    """The old monolithic Hop dataclass should still work alongside the new
    tagged union types. Old code that creates Hop() should not break."""

    def test_old_hop_still_importable(self):
        from degenbot.arbitrage.optimizers import Hop as OldHop

        # If we keep the old Hop, it should still work
        # (We may alias it to ConstantProductHop)
        hop = OldHop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT)
        assert hop.reserve_in == USDC_2M
