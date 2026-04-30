"""
Multiprocessing wrapper: ArbitragePath.calculate_with_pool

Verifies that the new method serializes HopState correctly and returns
the same result as synchronous calculate(). Unlike the legacy
UniswapLpCycle.calculate_with_pool, this never fails on sparse V3 bitmaps
because it serializes lightweight SolveInput (frozen dataclasses) instead
of full pool objects.
"""

import asyncio
from concurrent.futures import ProcessPoolExecutor, ThreadPoolExecutor
from fractions import Fraction

import pytest

from degenbot.arbitrage.optimizers.solver import BrentSolver, MobiusSolver
from degenbot.arbitrage.path import ArbitragePath
from degenbot.uniswap.v3_libraries.tick_math import get_sqrt_ratio_at_tick

from tests.arbitrage.test_path.conftest import (
    FakeConcentratedLiquidityPool,
    FakeToken,
    FakeUniswapV2Pool,
)


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture
def usdc() -> FakeToken:
    return FakeToken("0xUSDC", decimals=6)


@pytest.fixture
def weth() -> FakeToken:
    return FakeToken("0xWETH", decimals=18)


@pytest.fixture
def t0() -> FakeToken:
    return FakeToken("0xt0", decimals=18)


@pytest.fixture
def t1() -> FakeToken:
    return FakeToken("0xt1", decimals=18)


@pytest.fixture
def t2() -> FakeToken:
    return FakeToken("0xt2", decimals=18)


@pytest.fixture
def v2_v2_v2_pools(t0: FakeToken, t1: FakeToken, t2: FakeToken) -> tuple[FakeUniswapV2Pool, FakeUniswapV2Pool, FakeUniswapV2Pool]:
    """3-hop V2 cycle: t0 -> t1 -> t2 -> t0.

    Same reserve ratios as verify_legacy_equivalence.py, known profitable.
    """
    pool_0 = FakeUniswapV2Pool(
        token0=t0, token1=t1,
        reserve0=100 * 10**18, reserve1=200 * 10**18,
        fee=Fraction(3, 1000), address="0xpool0",
    )
    pool_1 = FakeUniswapV2Pool(
        token0=t1, token1=t2,
        reserve0=150 * 10**18, reserve1=300 * 10**18,
        fee=Fraction(3, 1000), address="0xpool1",
    )
    pool_2 = FakeUniswapV2Pool(
        token0=t2, token1=t0,
        reserve0=250 * 10**18, reserve1=500 * 10**18,
        fee=Fraction(3, 1000), address="0xpool2",
    )
    return (pool_0, pool_1, pool_2)


@pytest.fixture
def v3_profitable_pair(usdc: FakeToken, weth: FakeToken) -> list[FakeConcentratedLiquidityPool]:
    """A 2-hop single-range V3 cycle with 10% price spread."""
    import math

    tick_2200 = round(math.log(2200.0) / math.log(1.0001))
    tick_2000 = round(math.log(2000.0) / math.log(1.0001))
    sqrt_2200 = get_sqrt_ratio_at_tick(tick_2200)
    sqrt_2000 = get_sqrt_ratio_at_tick(tick_2000)

    pool_a = FakeConcentratedLiquidityPool(
        token0=usdc,
        token1=weth,
        liquidity=10**18,
        sqrt_price_x96=sqrt_2200,
        tick=tick_2200,
        fee=500,
        address="0xv3_a",
    )
    pool_b = FakeConcentratedLiquidityPool(
        token0=usdc,
        token1=weth,
        liquidity=10**18,
        sqrt_price_x96=sqrt_2000,
        tick=tick_2000,
        fee=500,
        address="0xv3_b",
    )
    return [pool_a, pool_b]


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------


class TestCalculateWithPool:
    """
    Verify calculate_with_pool returns the same SolveResult as calculate
    for both ThreadPool and ProcessPool executors.
    """

    def test_v2_threadpool_matches_sync(self, t0, v2_v2_v2_pools):
        """ThreadPoolExecutor: V2 cycle result identical to synchronous."""
        path = ArbitragePath(
            pools=v2_v2_v2_pools,
            input_token=t0,
            solver=MobiusSolver(),
            max_input=100 * 10**18,
        )
        sync_result = path.calculate()

        async def _run():
            with ThreadPoolExecutor(max_workers=1) as executor:
                future = path.calculate_with_pool(executor)
                return await future

        async_result = asyncio.run(_run())

        assert async_result.optimal_input == sync_result.optimal_input
        assert async_result.profit == sync_result.profit
        assert async_result.method == sync_result.method

    def test_v2_processpool_matches_sync(self, t0, v2_v2_v2_pools):
        """ProcessPoolExecutor: V2 cycle result identical to synchronous."""
        path = ArbitragePath(
            pools=v2_v2_v2_pools,
            input_token=t0,
            solver=MobiusSolver(),
            max_input=100 * 10**18,
        )
        sync_result = path.calculate()

        async def _run():
            with ProcessPoolExecutor(max_workers=1) as executor:
                future = path.calculate_with_pool(executor)
                return await future

        async_result = asyncio.run(_run())

        assert async_result.optimal_input == sync_result.optimal_input
        assert async_result.profit == sync_result.profit
        assert async_result.method == sync_result.method

    def test_v3_threadpool_matches_sync(self, usdc, v3_profitable_pair):
        """ThreadPoolExecutor: V3 single-range cycle result identical."""
        path = ArbitragePath(
            pools=v3_profitable_pair,
            input_token=usdc,
            solver=BrentSolver(),
            max_input=1_000_000,
        )
        sync_result = path.calculate()

        async def _run():
            with ThreadPoolExecutor(max_workers=1) as executor:
                future = path.calculate_with_pool(executor)
                return await future

        async_result = asyncio.run(_run())

        assert async_result.optimal_input == sync_result.optimal_input
        assert async_result.profit == sync_result.profit

    def test_v3_processpool_matches_sync(self, usdc, v3_profitable_pair):
        """ProcessPoolExecutor: V3 single-range cycle result identical.

        This is the critical improvement over legacy UniswapLpCycle: the
        legacy method fails with "Cannot perform calculation with process
        pool executor" when any V3 pool has sparse_liquidity_map=True.
        The new calculate_with_pool serializes only HopState (frozen
        dataclasses), so it is immune to this limitation.
        """
        path = ArbitragePath(
            pools=v3_profitable_pair,
            input_token=usdc,
            solver=BrentSolver(),
            max_input=1_000_000,
        )
        # Mark a pool as sparse to simulate the legacy failure mode
        v3_profitable_pair[0].sparse_liquidity_map = True

        sync_result = path.calculate()

        async def _run():
            with ProcessPoolExecutor(max_workers=1) as executor:
                future = path.calculate_with_pool(executor)
                return await future

        async_result = asyncio.run(_run())

        assert async_result.optimal_input == sync_result.optimal_input
        assert async_result.profit == sync_result.profit

    def test_state_override_with_pool(self, t0, v2_v2_v2_pools):
        """calculate_with_pool respects state_overrides."""
        from tests.arbitrage.test_path.conftest import FakeV2PoolState

        path = ArbitragePath(
            pools=v2_v2_v2_pools,
            input_token=t0,
            solver=MobiusSolver(),
            max_input=100 * 10**18,
        )
        baseline = path.calculate()

        new_state = FakeV2PoolState(
            address=v2_v2_v2_pools[0].address,
            block=None,
            reserves_token0=200 * 10**18,
            reserves_token1=100 * 10**18,
        )
        override = {v2_v2_v2_pools[0].address: new_state}
        overridden = path.calculate_with_state_override(override)

        async def _run():
            with ThreadPoolExecutor(max_workers=1) as executor:
                future = path.calculate_with_pool(executor, state_overrides=override)
                return await future

        async_overridden = asyncio.run(_run())

        assert async_overridden.optimal_input == overridden.optimal_input
        assert async_overridden.profit == overridden.profit
        assert async_overridden.optimal_input != baseline.optimal_input

    def test_unprofitable_path_raises(self, usdc):
        """Unprofitable cycle raises OptimizationError in executor too."""


        # Symmetric pools — no arb
        pool_a = FakeUniswapV2Pool(
            token0=usdc,
            token1=FakeToken("0xDAI"),
            reserve0=1_000_000 * 10**6,
            reserve1=1_000_000 * 10**18,
            fee=Fraction(3, 1000),
            address="0xunprof_a",
        )
        pool_b = FakeUniswapV2Pool(
            token0=usdc,
            token1=FakeToken("0xDAI"),
            reserve0=1_000_000 * 10**6,
            reserve1=1_000_000 * 10**18,
            fee=Fraction(3, 1000),
            address="0xunprof_b",
        )

        path = ArbitragePath(
            pools=[pool_a, pool_b],
            input_token=usdc,
            solver=MobiusSolver(),
            max_input=1_000_000,
        )

        async def _run():
            with ThreadPoolExecutor(max_workers=1) as executor:
                future = path.calculate_with_pool(executor)
                return await future

        from degenbot.exceptions import OptimizationError
        with pytest.raises(OptimizationError):
            asyncio.run(_run())
