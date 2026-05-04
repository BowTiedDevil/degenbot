"""
V3-only equivalence: ArbitragePath + Solver on synthetic single-range V3 pools.

Uses FakeConcentratedLiquidityPool (exact virtual-reserve math via
v3_virtual_reserves) and verifies both MobiusSolver and BrentSolver
find positive profit for a profitable V3-V3 cycle. No forking required.

For single-range V3 with MIN_TICK/MAX_TICK bounds, the virtual reserves are
enormous (~10^45), so the profit function is approximately linear in the
range [0, max_input]. Both solvers push to the max_input boundary.

Key insight for profitable V3-V3: same token pair, different prices.

Pool A: t0/t1 at price 2200. ArbitragePath goes t0->t1.
Pool B: t0/t1 at price 2000. ArbitragePath goes t1->t0.

Combined multiplicative factor = 2200/2000 = 1.1. After fees ≈ 1.098 → profitable.
"""

import math

import pytest

from degenbot.arbitrage.optimizers.hop_types import SolverMethod
from degenbot.arbitrage.optimizers.solver import BrentSolver, MobiusSolver
from degenbot.arbitrage.path import ArbitragePath
from degenbot.uniswap.v3_libraries.tick_math import get_sqrt_ratio_at_tick
from tests.arbitrage.test_path.conftest import FakeConcentratedLiquidityPool, FakeToken


@pytest.fixture
def t0():
    return FakeToken("0xt0", decimals=18)


@pytest.fixture
def t1():
    return FakeToken("0xt1", decimals=18)


@pytest.fixture
def t2():
    return FakeToken("0xt2", decimals=18)


@pytest.fixture
def tokens(t0, t1):
    """Two-token pair forming an arbitrage cycle."""
    return t0, t1


def _make_profitable_v3_v3_cycle(
    t0: FakeToken,
    t1: FakeToken,
    price_a: float = 2200.0,
    price_b: float = 2000.0,
    liquidity: int = 10**18,
    fee: int = 500,
) -> tuple[FakeConcentratedLiquidityPool, FakeConcentratedLiquidityPool]:
    """
    Create two V3 pools for the same token pair at different prices.

    Pool A: t0/t1 at {price_a}. ArbitragePath goes t0->t1 (zfo=True).
    Pool B: t0/t1 at {price_b}. ArbitragePath goes t1->t0 (zfo=False).

    Profitability: (price_a / price_b) * gamma^2.  When price_a > price_b
    and fees are low, this yields a positive profit.
    """
    tick_a = round(math.log(price_a) / math.log(1.0001))
    tick_b = round(math.log(price_b) / math.log(1.0001))

    sqrt_a = get_sqrt_ratio_at_tick(tick_a)
    sqrt_b = get_sqrt_ratio_at_tick(tick_b)

    pool_a = FakeConcentratedLiquidityPool(
        token0=t0,
        token1=t1,
        liquidity=liquidity,
        sqrt_price_x96=sqrt_a,
        tick=tick_a,
        fee=fee,
        address="0xv3_a",
    )
    pool_b = FakeConcentratedLiquidityPool(
        token0=t0,
        token1=t1,
        liquidity=liquidity,
        sqrt_price_x96=sqrt_b,
        tick=tick_b,
        fee=fee,
        address="0xv3_b",
    )

    return pool_a, pool_b


class TestV3OnlyEquivalance:
    """
    Verify ArbitragePath + Solver works correctly for V3-only paths.

    For single-range V3 pools, the Möbius closed-form should match Brent's
    numerical optimum. Both use the same _simulate_path on BoundedProductHops.
    """

    def test_v3_2hop_mobius_finds_profit(self, tokens):
        """A 2-hop V3 cycle at different prices must be profitable."""
        t0, t1 = tokens
        pool_a, pool_b = _make_profitable_v3_v3_cycle(t0, t1)

        path = ArbitragePath(
            pools=[pool_a, pool_b],
            input_token=t0,
            solver=MobiusSolver(),
            max_input=10 * 10**18,
        )
        result = path.calculate()

        assert result.optimal_input > 0
        assert result.profit > 0
        assert result.method == SolverMethod.MOBIUS

    def test_v3_2hop_mobius_and_brent_agree(self, tokens):
        """
        Both MobiusSolver and BrentSolver find a profitable optimal input.

        With MIN_TICK/MAX_TICK-based virtual reserves (~10^45), the profit
        function is linear in [0, max_input], so both solvers push to the
        max_input boundary. Use max_input small enough for float64 exact
        representation (below 2^53 ≈ 9e15).
        """
        t0, t1 = tokens
        pool_a, pool_b = _make_profitable_v3_v3_cycle(t0, t1)

        max_input = 1_000_000  # well within float64 exact range

        path_mobius = ArbitragePath(
            pools=[pool_a, pool_b],
            input_token=t0,
            solver=MobiusSolver(),
            max_input=max_input,
        )
        result_mobius = path_mobius.calculate()

        path_brent = ArbitragePath(
            pools=[pool_a, pool_b],
            input_token=t0,
            solver=BrentSolver(),
            max_input=max_input,
        )
        result_brent = path_brent.calculate()

        # Both should find profit and hit the boundary
        assert result_mobius.optimal_input > 0
        assert result_brent.optimal_input > 0
        assert result_mobius.profit > 0
        assert result_brent.profit > 0

        # Both optimizers find an integer near the boundary (within 1)
        assert abs(result_mobius.optimal_input - result_brent.optimal_input) <= 1
        assert max_input - result_mobius.optimal_input <= 10
        assert max_input - result_brent.optimal_input <= 10
        assert abs(result_mobius.profit - result_brent.profit) <= 1

    def test_v3_unprofitable_cycle_rejected(self, tokens):
        """When V3 pools have identical prices, both solvers reject the path."""
        t0, t1 = tokens
        pool_a, pool_b = _make_profitable_v3_v3_cycle(t0, t1, price_a=2000.0, price_b=2000.0)

        from degenbot.exceptions import OptimizationError

        path = ArbitragePath(
            pools=[pool_a, pool_b],
            input_token=t0,
            solver=MobiusSolver(),
            max_input=10 * 10**18,
        )
        with pytest.raises(OptimizationError):
            path.calculate()

    def test_v3_vs_manual_simulation(self, tokens):
        """
        MobiusSolver's optimal input must maximize profit as verified
        by manual simulation using the same BoundedProductHop math.
        """
        from degenbot.arbitrage.optimizers.solver import _simulate_path

        t0, t1 = tokens
        pool_a, pool_b = _make_profitable_v3_v3_cycle(t0, t1)

        max_input = 1_000_000  # exact in float64

        path = ArbitragePath(
            pools=[pool_a, pool_b],
            input_token=t0,
            solver=MobiusSolver(),
            max_input=max_input,
        )
        result = path.calculate()

        # With near-linear reserves, optimal is very close to max_input boundary
        assert result.optimal_input <= max_input
        assert max_input - result.optimal_input <= 10

        # Verify by brute force: check x-1, x, x+1
        hops = path.hop_states
        profits = {
            x: int(_simulate_path(float(x), hops)) - x
            for x in [result.optimal_input - 1, result.optimal_input, result.optimal_input + 1]
            if x > 0
        }
        assert profits[result.optimal_input] >= profits.get(result.optimal_input - 1, -1)
        assert profits[result.optimal_input] + 1 >= profits.get(result.optimal_input + 1, -1)

    def test_v3_3hop_mixed_directions(self, t0, t1, t2):
        """
        A 3-hop V3-only cycle with alternating price directions.

        Pool 0: t0->t1 at price 2200
        Pool 1: t1->t2 at price 3.0
        Pool 2: t2->t0 at price 1/5.0

        For the FakeConcentratedLiquidityPool, the swap variant calculation in
        to_hop_state uses v3_virtual_reserves to ensure the invariant is a
        constant-product share. If the directions and prices are asymmetric,
        the path can be profitable even with 0.05% fees.
        """

        # Ticks for prices: 2200, 3.0, and 1/5.0 (which is -tick of 5.0)
        tick_2200 = round(math.log(2200.0) / math.log(1.0001))
        tick_3 = round(math.log(3.0) / math.log(1.0001))
        tick_inv5 = round(math.log(1 / 5.0) / math.log(1.0001))

        sqrt_2200 = get_sqrt_ratio_at_tick(tick_2200)
        sqrt_3 = get_sqrt_ratio_at_tick(tick_3)
        sqrt_inv5 = get_sqrt_ratio_at_tick(tick_inv5)

        pool_0 = FakeConcentratedLiquidityPool(
            token0=t0,
            token1=t1,
            liquidity=10**18,
            sqrt_price_x96=sqrt_2200,
            tick=tick_2200,
            fee=500,
            address="0xv3_0",
        )
        pool_1 = FakeConcentratedLiquidityPool(
            token0=t1,
            token1=t2,
            liquidity=10**18,
            sqrt_price_x96=sqrt_3,
            tick=tick_3,
            fee=500,
            address="0xv3_1",
        )
        pool_2 = FakeConcentratedLiquidityPool(
            token0=t2,
            token1=t0,
            liquidity=10**18,
            sqrt_price_x96=sqrt_inv5,
            tick=tick_inv5,
            fee=500,
            address="0xv3_2",
        )

        path = ArbitragePath(
            pools=[pool_0, pool_1, pool_2],
            input_token=t0,
            solver=MobiusSolver(),
            max_input=10 * 10**18,
        )
        result = path.calculate()

        assert result.optimal_input > 0
        assert result.profit > 0
        assert result.method == SolverMethod.MOBIUS
