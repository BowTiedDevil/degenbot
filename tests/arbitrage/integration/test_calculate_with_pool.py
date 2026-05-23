"""Multiprocessing wrapper: ArbitragePath.calculate_with_pool

Verifies that the new method serializes HopState correctly and returns
the same result as synchronous calculate(). Unlike the legacy
UniswapLpCycle.calculate_with_pool, this never fails on sparse V3 bitmaps
because it serializes lightweight SolveInput (frozen dataclasses) instead
of full pool objects.
"""

import asyncio
import math
from concurrent.futures import ProcessPoolExecutor, ThreadPoolExecutor
from fractions import Fraction

import pytest

from degenbot.arbitrage.optimizers.solver import BrentSolver, MobiusSolver
from degenbot.arbitrage.path import ArbitragePath
from degenbot.exceptions.arbitrage import OptimizationError
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v2_types import UniswapV2PoolState
from degenbot.uniswap.v3_libraries.tick_math import get_sqrt_ratio_at_tick
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from tests.fakes.tokens import FakeToken

# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

# Valid hex addresses for production pool construction
ADDR_USDC = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
ADDR_WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
ADDR_DAI = "0x6B175474E89094C44Da98b954EedeAC495271d0F"
ADDR_POOL0 = "0x00000000000000000000000000000000000000a0"
ADDR_POOL1 = "0x00000000000000000000000000000000000000a1"
ADDR_POOL2 = "0x00000000000000000000000000000000000000a2"
ADDR_V3A = "0x00000000000000000000000000000000000000b0"
ADDR_V3B = "0x00000000000000000000000000000000000000b1"
ADDR_UNPROF_A = "0x00000000000000000000000000000000000000c0"
ADDR_UNPROF_B = "0x00000000000000000000000000000000000000c1"


@pytest.fixture
def usdc() -> FakeToken:
    return FakeToken(ADDR_USDC, decimals=6)


@pytest.fixture
def weth() -> FakeToken:
    return FakeToken(ADDR_WETH, decimals=18)


@pytest.fixture
def dai() -> FakeToken:
    return FakeToken(ADDR_DAI, decimals=18)


@pytest.fixture
def t0() -> FakeToken:
    return FakeToken("0x0000000000000000000000000000000000000T0", decimals=18)


@pytest.fixture
def t1() -> FakeToken:
    return FakeToken("0x0000000000000000000000000000000000000T1", decimals=18)


@pytest.fixture
def t2() -> FakeToken:
    return FakeToken("0x0000000000000000000000000000000000000T2", decimals=18)


@pytest.fixture
def v2_v2_v2_pools(
    t0: FakeToken, t1: FakeToken, t2: FakeToken
) -> tuple[UniswapV2Pool, UniswapV2Pool, UniswapV2Pool]:
    """3-hop V2 cycle: t0 -> t1 -> t2 -> t0.

    Same reserve ratios as verify_legacy_equivalence.py, known profitable.
    """
    fee = Fraction(3, 1000)
    pool_0 = UniswapV2Pool(
        address=ADDR_POOL0,  # type: ignore[arg-type]
        token0=t0,  # type: ignore[arg-type]
        token1=t1,  # type: ignore[arg-type]
        factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
        fee_token0=fee,
        fee_token1=fee,
        reserves_token0=100 * 10**18,
        reserves_token1=200 * 10**18,
        state_block=1,
    )
    pool_1 = UniswapV2Pool(
        address=ADDR_POOL1,  # type: ignore[arg-type]
        token0=t1,  # type: ignore[arg-type]
        token1=t2,  # type: ignore[arg-type]
        factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
        fee_token0=fee,
        fee_token1=fee,
        reserves_token0=150 * 10**18,
        reserves_token1=300 * 10**18,
        state_block=1,
    )
    pool_2 = UniswapV2Pool(
        address=ADDR_POOL2,  # type: ignore[arg-type]
        token0=t2,  # type: ignore[arg-type]
        token1=t0,  # type: ignore[arg-type]
        factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
        fee_token0=fee,
        fee_token1=fee,
        reserves_token0=250 * 10**18,
        reserves_token1=500 * 10**18,
        state_block=1,
    )
    return (pool_0, pool_1, pool_2)


@pytest.fixture
def v3_profitable_pair(usdc: FakeToken, weth: FakeToken) -> list[UniswapV3Pool]:
    """A 2-hop single-range V3 cycle with 10% price spread."""
    tick_2200 = round(math.log(2200.0) / math.log(1.0001))
    tick_2000 = round(math.log(2000.0) / math.log(1.0001))
    sqrt_2200 = get_sqrt_ratio_at_tick(tick_2200)
    sqrt_2000 = get_sqrt_ratio_at_tick(tick_2000)

    pool_a = UniswapV3Pool(
        address=ADDR_V3A,  # type: ignore[arg-type]
        token0=usdc,  # type: ignore[arg-type]
        token1=weth,  # type: ignore[arg-type]
        factory="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        fee=500,
        tick_spacing=10,
        sqrt_price_x96=sqrt_2200,
        tick=tick_2200,
        liquidity=10**18,
        state_block=1,
    )
    pool_b = UniswapV3Pool(
        address=ADDR_V3B,  # type: ignore[arg-type]
        token0=usdc,  # type: ignore[arg-type]
        token1=weth,  # type: ignore[arg-type]
        factory="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        fee=500,
        tick_spacing=10,
        sqrt_price_x96=sqrt_2000,
        tick=tick_2000,
        liquidity=10**18,
        state_block=1,
    )
    return [pool_a, pool_b]


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------


class TestCalculateWithPool:
    """Verify calculate_with_pool returns the same SolveResult as calculate
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
        # Sparse liquidity map is already True for pools without tick data

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
        path = ArbitragePath(
            pools=v2_v2_v2_pools,
            input_token=t0,
            solver=MobiusSolver(),
            max_input=100 * 10**18,
        )
        baseline = path.calculate()

        new_state = UniswapV2PoolState(
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

    def test_unprofitable_path_raises(self, usdc, dai):
        """Unprofitable cycle raises OptimizationError in executor too."""
        # Symmetric pools — no arb
        fee = Fraction(3, 1000)
        pool_a = UniswapV2Pool(
            address=ADDR_UNPROF_A,  # type: ignore[arg-type]
            token0=usdc,  # type: ignore[arg-type]
            token1=dai,  # type: ignore[arg-type]
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=fee,
            fee_token1=fee,
            reserves_token0=1_000_000 * 10**6,
            reserves_token1=1_000_000 * 10**18,
            state_block=1,
        )
        pool_b = UniswapV2Pool(
            address=ADDR_UNPROF_B,  # type: ignore[arg-type]
            token0=usdc,  # type: ignore[arg-type]
            token1=dai,  # type: ignore[arg-type]
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=fee,
            fee_token1=fee,
            reserves_token0=1_000_000 * 10**6,
            reserves_token1=1_000_000 * 10**18,
            state_block=1,
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

        with pytest.raises(OptimizationError):
            asyncio.run(_run())
