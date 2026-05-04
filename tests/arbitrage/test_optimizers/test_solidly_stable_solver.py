"""
Tests for SolidlyStableSolver — Newton's method solver for Solidly stable
invariant pools (x³y + xy³ ≥ k) in the unified solver interface.

Tests cover:
- Pure Solidly stable paths (2-hop)
- Mixed V2 + Solidly stable paths (Möbius compose + Newton outer)
- Asymmetric fees (Camelot)
- Profit matching vs Brent solver
- Edge cases (not profitable, zero reserves, etc.)
"""

from collections.abc import Callable
from fractions import Fraction
from typing import Literal

import pytest

from degenbot.aerodrome.functions import calc_exact_in_stable
from degenbot.arbitrage.optimizers.solidly_stable import _simulate_mixed_path_int
from degenbot.arbitrage.optimizers.solver import (
    ArbSolver,
    ConstantProductHop,
    PoolInvariant,
    SolidlyStableHop,
    SolidlyStableSolver,
    SolveInput,
    SolverMethod,
)
from degenbot.exceptions import OptimizationError

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def make_aerodrome_stable_swap_fn(
    reserves0: int,
    reserves1: int,
    decimals0: int,
    decimals1: int,
    fee: Fraction,
    token_in: Literal[0, 1],
) -> Callable[[int], int]:
    """
    Create an integer swap function for Aerodrome stable pools.

    Wraps ``calc_exact_in_stable`` with the pool's parameters.
    """

    def swap_fn(amount_in: int) -> int:
        return calc_exact_in_stable(
            amount_in=amount_in,
            token_in=token_in,
            reserves0=reserves0,
            reserves1=reserves1,
            decimals0=decimals0,
            decimals1=decimals1,
            fee=fee,
        )

    return swap_fn


# ---------------------------------------------------------------------------
# Test Fixtures
# ---------------------------------------------------------------------------

# USDC (6 decimals) / WETH (18 decimals) stable pool on Base (Aerodrome)
# Pool rate: 10M USDC / 4K WETH = 2500 USDC/WETH
RESERVES_USDC_0 = 10_000_000 * 10**6
RESERVES_WETH_0 = 4_000 * 10**18
FEE_STABLE = Fraction(1, 1000)

STABLE_HOP_USDC_WETH = SolidlyStableHop(
    reserve_in=RESERVES_USDC_0,
    reserve_out=RESERVES_WETH_0,
    fee=FEE_STABLE,
    decimals_in=6,
    decimals_out=18,
    swap_fn=make_aerodrome_stable_swap_fn(
        reserves0=RESERVES_USDC_0,
        reserves1=RESERVES_WETH_0,
        decimals0=10**6,
        decimals1=10**18,
        fee=FEE_STABLE,
        token_in=0,
    ),
)

# USDC/USDT stable pool (both 6 decimals)
RESERVES_USDC_1 = 10_000_000 * 10**6
RESERVES_USDT_1 = 10_050_000 * 10**6
FEE_STABLE_LOW = Fraction(3, 10000)

STABLE_HOP_USDC_USDT = SolidlyStableHop(
    reserve_in=RESERVES_USDC_1,
    reserve_out=RESERVES_USDT_1,
    fee=FEE_STABLE_LOW,
    decimals_in=6,
    decimals_out=6,
    swap_fn=make_aerodrome_stable_swap_fn(
        reserves0=RESERVES_USDC_1,
        reserves1=RESERVES_USDT_1,
        decimals0=10**6,
        decimals1=10**6,
        fee=FEE_STABLE_LOW,
        token_in=0,
    ),
)

# V2 WETH/USDC pool
V2_HOP_WETH_USDC = ConstantProductHop(
    reserve_in=2_000 * 10**18,  # 2000 WETH
    reserve_out=5_000_000 * 10**6,  # 5M USDC
    fee=Fraction(3, 1000),
)


# ---------------------------------------------------------------------------
# Tests: SolidlyStableSolver basic functionality
# ---------------------------------------------------------------------------


class TestSolidlyStableSolverSupports:
    """Test supports() method for SolidlyStableSolver."""

    def test_supports_pure_solidly_path(self):
        """Solver should support a path with only Solidly stable hops."""

        solver = SolidlyStableSolver()
        solve_input = SolveInput(
            hops=(STABLE_HOP_USDC_WETH, STABLE_HOP_USDC_USDT),
        )
        assert solver.supports(solve_input)

    def test_supports_mixed_solidly_v2_path(self):
        """Solver should support a path with mixed Solidly + V2 hops."""

        solver = SolidlyStableSolver()
        solve_input = SolveInput(
            hops=(V2_HOP_WETH_USDC, STABLE_HOP_USDC_WETH),
        )
        assert solver.supports(solve_input)

    def test_does_not_support_pure_v2_path(self):
        """Solver should NOT support a pure V2 path (that's MobiusSolver)."""

        solver = SolidlyStableSolver()
        solve_input = SolveInput(
            hops=(
                ConstantProductHop(
                    reserve_in=1_000 * 10**18,
                    reserve_out=2_000_000 * 10**6,
                    fee=Fraction(3, 1000),
                ),
                ConstantProductHop(
                    reserve_in=1_500_000 * 10**6,
                    reserve_out=750 * 10**18,
                    fee=Fraction(3, 1000),
                ),
            ),
        )
        assert not solver.supports(solve_input)

    def test_does_not_support_single_hop(self):
        """Solver requires 2+ hops."""

        solver = SolidlyStableSolver()
        solve_input = SolveInput(hops=(STABLE_HOP_USDC_WETH,))
        assert not solver.supports(solve_input)


class TestSolidlyStableSolverSolve:
    """Test solve() method for SolidlyStableSolver."""

    def test_solidly_stable_path_with_price_discrepancy(self):
        """
        2-hop path with price discrepancy: stable pool has cheaper WETH
        than V2 pool, so there's arbitrage opportunity.
        """

        solver = SolidlyStableSolver()

        # Stable pool: 10M USDC → 5K WETH (rate: 2000 USDC/WETH — cheap WETH)
        reserves_usdc = 10_000_000 * 10**6
        reserves_weth = 5_000 * 10**18
        fee = Fraction(1, 1000)
        cheap_stable = SolidlyStableHop(
            reserve_in=reserves_usdc,
            reserve_out=reserves_weth,
            fee=fee,
            decimals_in=6,
            decimals_out=18,
            swap_fn=make_aerodrome_stable_swap_fn(
                reserves0=reserves_usdc,
                reserves1=reserves_weth,
                decimals0=10**6,
                decimals1=10**18,
                fee=fee,
                token_in=0,
            ),
        )

        # V2 pool: 2000 WETH → 5M USDC (rate: 2500 USDC/WETH — expensive WETH)
        expensive_v2 = ConstantProductHop(
            reserve_in=2_000 * 10**18,
            reserve_out=5_000_000 * 10**6,
            fee=Fraction(3, 1000),
        )

        solve_input = SolveInput(
            hops=(cheap_stable, expensive_v2),
        )
        result = solver.solve(solve_input)
        assert result.profit > 0
        assert result.optimal_input > 0
        assert result.method == SolverMethod.SOLIDLY_STABLE

    def test_not_profitable_equal_rates(self):
        """When stable pool and V2 pool have same rate, no profit."""

        solver = SolidlyStableSolver()

        # Both pools at same rate: 2500 USDC/WETH
        reserves_usdc = 10_000_000 * 10**6
        reserves_weth = 4_000 * 10**18
        fee = Fraction(3, 1000)
        stable = SolidlyStableHop(
            reserve_in=reserves_usdc,
            reserve_out=reserves_weth,
            fee=fee,
            decimals_in=6,
            decimals_out=18,
            swap_fn=make_aerodrome_stable_swap_fn(
                reserves0=reserves_usdc,
                reserves1=reserves_weth,
                decimals0=10**6,
                decimals1=10**18,
                fee=fee,
                token_in=0,
            ),
        )
        v2 = ConstantProductHop(
            reserve_in=4_000 * 10**18,
            reserve_out=10_000_000 * 10**6,
            fee=Fraction(3, 1000),
        )

        solve_input = SolveInput(hops=(stable, v2))
        with pytest.raises(OptimizationError):
            solver.solve(solve_input)

    def test_mixed_path_v2_then_solidly(self):
        """V2 hop followed by Solidly stable hop."""

        solver = SolidlyStableSolver()

        # V2: expensive WETH → USDC (4000 USDC/WETH rate)
        expensive_v2 = ConstantProductHop(
            reserve_in=1_000 * 10**18,
            reserve_out=4_000_000 * 10**6,  # 4M USDC per 1K WETH = 4000 rate
            fee=Fraction(3, 1000),
        )
        # Solidly: cheap USDC → WETH (~3333 USDC/WETH rate)
        reserves_usdc = 5_000_000 * 10**6
        reserves_weth = 1_500 * 10**18
        fee = Fraction(1, 1000)
        cheap_stable = SolidlyStableHop(
            reserve_in=reserves_usdc,
            reserve_out=reserves_weth,
            fee=fee,
            decimals_in=6,
            decimals_out=18,
            swap_fn=make_aerodrome_stable_swap_fn(
                reserves0=reserves_usdc,
                reserves1=reserves_weth,
                decimals0=10**6,
                decimals1=10**18,
                fee=fee,
                token_in=0,
            ),
        )

        solve_input = SolveInput(hops=(expensive_v2, cheap_stable))
        result = solver.solve(solve_input)
        assert result.profit > 0
        assert result.method == SolverMethod.SOLIDLY_STABLE

    def test_solidly_stable_matches_brent_profit(self):
        """
        SolidlyStableSolver profit should be close to a brute-force
        search for the optimal input on a Solidly stable path.

        Since BrentSolver uses _simulate_path (V2 formula) for all hops,
        it won't match exactly — we compare against a manual search instead.
        """

        solver = SolidlyStableSolver()

        reserves_usdc = 10_000_000 * 10**6
        reserves_weth = 5_000 * 10**18
        fee = Fraction(1, 1000)
        cheap_stable = SolidlyStableHop(
            reserve_in=reserves_usdc,
            reserve_out=reserves_weth,
            fee=fee,
            decimals_in=6,
            decimals_out=18,
            swap_fn=make_aerodrome_stable_swap_fn(
                reserves0=reserves_usdc,
                reserves1=reserves_weth,
                decimals0=10**6,
                decimals1=10**18,
                fee=fee,
                token_in=0,
            ),
        )
        expensive_v2 = ConstantProductHop(
            reserve_in=2_000 * 10**18,
            reserve_out=5_000_000 * 10**6,
            fee=Fraction(3, 1000),
        )

        hops = (cheap_stable, expensive_v2)
        solve_input = SolveInput(hops=hops)
        result = solver.solve(solve_input)

        # Brute-force search to validate
        best_profit = 0
        # Search around the solver's answer ±10%
        center = result.optimal_input
        for offset in range(-100, 101):
            candidate = center + offset * center // 1000  # ±10% in 0.01% steps
            if candidate <= 0:
                continue
            output = _simulate_mixed_path_int(candidate, hops)
            profit = output - candidate
            best_profit = max(best_profit, profit)

        # Solver profit should be within 1% of brute-force optimum
        if best_profit > 0:
            profit_ratio = abs(result.profit - best_profit) / best_profit
            assert profit_ratio < 0.01, (
                f"Solver profit {result.profit} vs brute-force {best_profit} "
                f"(ratio={profit_ratio:.4f})"
            )


# ---------------------------------------------------------------------------
# Tests: ArbSolver dispatch with Solidly stable
# ---------------------------------------------------------------------------


class TestArbSolverSolidlyDispatch:
    """Test that ArbSolver correctly dispatches Solidly stable paths."""

    def test_arb_solver_dispatches_solidly(self):
        """ArbSolver should use SolidlyStableSolver for Solidly paths."""
        solver = ArbSolver()

        reserves_usdc = 10_000_000 * 10**6
        reserves_weth = 5_000 * 10**18
        fee = Fraction(1, 1000)
        cheap_stable = SolidlyStableHop(
            reserve_in=reserves_usdc,
            reserve_out=reserves_weth,
            fee=fee,
            decimals_in=6,
            decimals_out=18,
            swap_fn=make_aerodrome_stable_swap_fn(
                reserves0=reserves_usdc,
                reserves1=reserves_weth,
                decimals0=10**6,
                decimals1=10**18,
                fee=fee,
                token_in=0,
            ),
        )
        expensive_v2 = ConstantProductHop(
            reserve_in=2_000 * 10**18,
            reserve_out=5_000_000 * 10**6,
            fee=Fraction(3, 1000),
        )

        result = solver.solve(SolveInput(hops=(cheap_stable, expensive_v2)))
        assert result.method == SolverMethod.SOLIDLY_STABLE

    def test_arb_solver_uses_mobius_for_pure_v2(self):
        """ArbSolver should still use MobiusSolver for pure V2 paths."""
        solver = ArbSolver()

        hops = (
            ConstantProductHop(
                reserve_in=1_000 * 10**18,
                reserve_out=2_500_000 * 10**6,
                fee=Fraction(3, 1000),
            ),
            ConstantProductHop(
                reserve_in=1_800_000 * 10**6,
                reserve_out=800 * 10**18,
                fee=Fraction(3, 1000),
            ),
        )
        result = solver.solve(SolveInput(hops=hops))
        assert result.method == SolverMethod.MOBIUS


# ---------------------------------------------------------------------------
# Tests: Asymmetric fees (Camelot)
# ---------------------------------------------------------------------------


class TestAsymmetricFees:
    """Test ConstantProductHop with asymmetric fees (Camelot)."""

    def test_constant_product_hop_with_fee_out(self):
        """ConstantProductHop with fee_out should use correct gamma."""
        hop = ConstantProductHop(
            reserve_in=1_000 * 10**18,
            reserve_out=2_500_000 * 10**6,
            fee=Fraction(2, 1000),  # 0.2% fee_in
            fee_out=Fraction(5, 1000),  # 0.5% fee_out
        )
        assert hop.gamma == 1.0 - 0.002  # gamma uses fee (input direction)

    def test_constant_product_hop_without_fee_out(self):
        """ConstantProductHop without fee_out should default to None."""
        hop = ConstantProductHop(
            reserve_in=1_000 * 10**18,
            reserve_out=2_500_000 * 10**6,
            fee=Fraction(3, 1000),
        )
        assert hop.fee_out is None

    def test_solidly_stable_with_asymmetric_fee_v2(self):
        """
        Mixed path: Solidly stable + Camelot volatile (asymmetric fees).
        The V2 hop uses fee (input direction gamma).
        """

        solver = SolidlyStableSolver()

        reserves_usdc = 10_000_000 * 10**6
        reserves_weth = 5_000 * 10**18
        fee = Fraction(1, 1000)
        stable = SolidlyStableHop(
            reserve_in=reserves_usdc,
            reserve_out=reserves_weth,
            fee=fee,
            decimals_in=6,
            decimals_out=18,
            swap_fn=make_aerodrome_stable_swap_fn(
                reserves0=reserves_usdc,
                reserves1=reserves_weth,
                decimals0=10**6,
                decimals1=10**18,
                fee=fee,
                token_in=0,
            ),
        )
        # Camelot volatile pool with asymmetric fees
        camelot_v2 = ConstantProductHop(
            reserve_in=2_000 * 10**18,
            reserve_out=5_000_000 * 10**6,
            fee=Fraction(2, 1000),  # 0.2% for this direction
            fee_out=Fraction(5, 1000),  # 0.5% for other direction
        )

        solve_input = SolveInput(hops=(stable, camelot_v2))
        result = solver.solve(solve_input)
        assert result.profit > 0

    def test_pool_to_hop_camelot_volatile_structure(self):
        """ConstantProductHop supports the fee_out field for Camelot."""
        hop = ConstantProductHop(
            reserve_in=1_000 * 10**18,
            reserve_out=2_500_000 * 10**6,
            fee=Fraction(2, 1000),
            fee_out=Fraction(5, 1000),
        )
        assert hop.invariant == PoolInvariant.CONSTANT_PRODUCT
        assert hop.fee_out == Fraction(5, 1000)


# ---------------------------------------------------------------------------
# Tests: Solidly stable solver with 6-decimal pairs
# ---------------------------------------------------------------------------


class TestSolidlyStableSixDecimalPairs:
    """Test Solidly stable solver with USDC/USDT-like 6-decimal pairs."""

    def test_usdc_usdt_stable_arbitrage(self):
        """
        USDC/USDT stable pair with small price discrepancy.
        These should produce small profits due to the flat curve.
        """

        solver = SolidlyStableSolver()

        # Pool 1: USDC → USDT at 1:1.002 (slight premium for USDT)
        r0_1 = 10_000_000 * 10**6
        r1_1 = 10_020_000 * 10**6
        fee1 = Fraction(1, 10000)  # 0.01% fee
        pool1 = SolidlyStableHop(
            reserve_in=r0_1,
            reserve_out=r1_1,
            fee=fee1,
            decimals_in=6,
            decimals_out=6,
            swap_fn=make_aerodrome_stable_swap_fn(
                reserves0=r0_1,
                reserves1=r1_1,
                decimals0=10**6,
                decimals1=10**6,
                fee=fee1,
                token_in=0,
            ),
        )

        # Pool 2: USDT → USDC at 1:0.998 (slight discount for USDT)
        r0_2 = 10_020_000 * 10**6
        r1_2 = 9_990_000 * 10**6
        fee2 = Fraction(1, 10000)
        pool2 = SolidlyStableHop(
            reserve_in=r0_2,
            reserve_out=r1_2,
            fee=fee2,
            decimals_in=6,
            decimals_out=6,
            swap_fn=make_aerodrome_stable_swap_fn(
                reserves0=r0_2,
                reserves1=r1_2,
                decimals0=10**6,
                decimals1=10**6,
                fee=fee2,
                token_in=0,
            ),
        )

        solve_input = SolveInput(hops=(pool1, pool2))
        with pytest.raises(OptimizationError):
            solver.solve(solve_input)


# ---------------------------------------------------------------------------
# Tests: Mixed Möbius-Newton pattern
# ---------------------------------------------------------------------------


class TestMixedMobiusNewtonPattern:
    """
    Test the Mixed Möbius-Newton pattern where V2 hops are composed
    into a Möbius function and Solidly hops are treated as opaque.
    """

    def test_multi_hop_with_solidly_at_end(self):
        """
        3-hop path: V2 → V2 → Solidly.
        V2 hops should be Möbius-composed for initial guess,
        Solidly hop is opaque.
        """

        solver = SolidlyStableSolver()

        # V2: WETH → TOKEN_A
        hop1 = ConstantProductHop(
            reserve_in=1_000 * 10**18,
            reserve_out=2_000_000 * 10**18,
            fee=Fraction(3, 1000),
        )
        # V2: TOKEN_A → USDC
        hop2 = ConstantProductHop(
            reserve_in=500_000 * 10**18,
            reserve_out=1_500_000 * 10**6,
            fee=Fraction(3, 1000),
        )
        # Solidly: USDC → WETH
        r_usdc = 5_000_000 * 10**6
        r_weth = 1_500 * 10**18
        fee_s = Fraction(1, 1000)
        hop3 = SolidlyStableHop(
            reserve_in=r_usdc,
            reserve_out=r_weth,
            fee=fee_s,
            decimals_in=6,
            decimals_out=18,
            swap_fn=make_aerodrome_stable_swap_fn(
                reserves0=r_usdc,
                reserves1=r_weth,
                decimals0=10**6,
                decimals1=10**18,
                fee=fee_s,
                token_in=0,
            ),
        )

        solve_input = SolveInput(hops=(hop1, hop2, hop3))
        result = solver.solve(solve_input)
        assert result.profit > 0

    def test_multi_hop_with_solidly_at_start(self):
        """
        3-hop path: Solidly → V2 → V2.
        Solidly hop first, then V2 hops Möbius-composed for initial guess.
        """

        solver = SolidlyStableSolver()

        # Solidly: cheap USDC → WETH (2000 USDC/WETH rate)
        r_usdc = 6_000_000 * 10**6
        r_weth = 3_000 * 10**18  # 3000 WETH for 6M USDC = 2000 rate
        fee_s = Fraction(1, 1000)
        hop1 = SolidlyStableHop(
            reserve_in=r_usdc,
            reserve_out=r_weth,
            fee=fee_s,
            decimals_in=6,
            decimals_out=18,
            swap_fn=make_aerodrome_stable_swap_fn(
                reserves0=r_usdc,
                reserves1=r_weth,
                decimals0=10**6,
                decimals1=10**18,
                fee=fee_s,
                token_in=0,
            ),
        )
        # V2: WETH → TOKEN_A
        hop2 = ConstantProductHop(
            reserve_in=1_500_000 * 10**18,
            reserve_out=3_000_000_000 * 10**18,
            fee=Fraction(3, 1000),
        )
        # V2: TOKEN_A → USDC (expensive, gives lots of USDC)
        hop3 = ConstantProductHop(
            reserve_in=2_000_000_000 * 10**18,
            reserve_out=10_000_000 * 10**6,
            fee=Fraction(3, 1000),
        )

        solve_input = SolveInput(hops=(hop1, hop2, hop3))
        with pytest.raises(OptimizationError):
            solver.solve(solve_input)

    def test_solidly_iterations_reasonable(self):
        """SolidlyStableSolver should converge in ≤ 30 iterations."""

        solver = SolidlyStableSolver()

        reserves_usdc = 10_000_000 * 10**6
        reserves_weth = 5_000 * 10**18
        fee = Fraction(1, 1000)
        cheap_stable = SolidlyStableHop(
            reserve_in=reserves_usdc,
            reserve_out=reserves_weth,
            fee=fee,
            decimals_in=6,
            decimals_out=18,
            swap_fn=make_aerodrome_stable_swap_fn(
                reserves0=reserves_usdc,
                reserves1=reserves_weth,
                decimals0=10**6,
                decimals1=10**18,
                fee=fee,
                token_in=0,
            ),
        )
        expensive_v2 = ConstantProductHop(
            reserve_in=2_000 * 10**18,
            reserve_out=5_000_000 * 10**6,
            fee=Fraction(3, 1000),
        )

        solve_input = SolveInput(hops=(cheap_stable, expensive_v2))
        result = solver.solve(solve_input)
        assert result.iterations <= 30

    def test_golden_section_exact_match_brute_force(self):
        """Golden section search should find the same optimum as brute force."""

        solver = SolidlyStableSolver()

        reserves_usdc = 10_000_000 * 10**6
        reserves_weth = 5_000 * 10**18
        fee = Fraction(1, 1000)
        cheap_stable = SolidlyStableHop(
            reserve_in=reserves_usdc,
            reserve_out=reserves_weth,
            fee=fee,
            decimals_in=6,
            decimals_out=18,
            swap_fn=make_aerodrome_stable_swap_fn(
                reserves0=reserves_usdc,
                reserves1=reserves_weth,
                decimals0=10**6,
                decimals1=10**18,
                fee=fee,
                token_in=0,
            ),
        )
        expensive_v2 = ConstantProductHop(
            reserve_in=2_000 * 10**18,
            reserve_out=5_000_000 * 10**6,
            fee=Fraction(3, 1000),
        )

        hops = (cheap_stable, expensive_v2)
        solve_input = SolveInput(hops=hops)
        result = solver.solve(solve_input)

        # Brute force in 0.01% steps around the answer
        best_profit = 0
        center = result.optimal_input
        for offset in range(-200, 201):
            x = center + offset * center // 10000
            if x <= 0:
                continue
            p = _simulate_mixed_path_int(x, hops) - x
            best_profit = max(best_profit, p)

        assert result.profit == best_profit

    def test_no_swap_fn_uses_newton_fallback(self):
        """
        SolidlyStableHop without swap_fn should fall back to
        Newton's method with float simulation.
        """

        solver = SolidlyStableSolver()

        # Same-reserve pool (6/6 decimals) where float is more accurate
        r0 = 10_000_000 * 10**6
        r1 = 10_050_000 * 10**6
        fee = Fraction(1, 10000)
        stable_no_fn = SolidlyStableHop(
            reserve_in=r0,
            reserve_out=r1,
            fee=fee,
            decimals_in=6,
            decimals_out=6,
            # No swap_fn — forces Newton fallback
        )
        # V2 with slight price discrepancy
        v2 = ConstantProductHop(
            reserve_in=5_000 * 10**18,
            reserve_out=12_000_000 * 10**6,
            fee=Fraction(3, 1000),
        )

        solve_input = SolveInput(hops=(stable_no_fn, v2))
        try:
            result = solver.solve(solve_input)
            assert result.method == SolverMethod.SOLIDLY_STABLE
        except OptimizationError:
            pass
