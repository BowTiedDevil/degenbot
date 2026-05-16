import asyncio
import concurrent.futures
import contextlib
import multiprocessing
import pickle
import time
from fractions import Fraction
from typing import TYPE_CHECKING

import pytest

from degenbot.anvil_fork import AnvilFork
from degenbot.arbitrage._legacy import _UniswapLpCycle as UniswapLpCycle
from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.erc20.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.arbitrage import (
    ArbitrageError,
    OptimizationError,
    RateOfExchangeBelowMinimum,
)
from degenbot.provider import ProviderAdapter
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v2_types import (
    UniswapV2PoolExternalUpdate,
    UniswapV2PoolState,
    UniswapV2PoolStateUpdated,
)
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v3_types import (
    UniswapV3BitmapAtWord,
    UniswapV3LiquidityAtTick,
    UniswapV3PoolExternalUpdate,
    UniswapV3PoolState,
    UniswapV3PoolStateUpdated,
)
from tests.fakes.pools import MockLiquidityPool, MockV3LiquidityPool
from tests.fakes.subscribers import FakeSubscriber
from tests.helpers.bot_factory import make_bot_with_provider

if TYPE_CHECKING:
    from degenbot.arbitrage._legacy._uniswap_lp_cycle import Pool, PoolState

WBTC_ADDRESS = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
WBTC_WETH_V2_POOL_ADDRESS = "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940"
WBTC_WETH_V3_POOL_ADDRESS = "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD"
UNISWAP_V3_FACTORY = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
V3_FEE = 3000
V3_TICK_SPACING = 60


@pytest.fixture
def _bot(fork_mainnet_full: AnvilFork):
    return make_bot_with_provider(ProviderAdapter.from_web3(fork_mainnet_full.w3))


@pytest.fixture
def wbtc_token(_bot) -> Erc20Token:
    return _bot.build_erc20token(WBTC_ADDRESS)


@pytest.fixture
def weth_token(_bot) -> Erc20Token:
    return _bot.build_erc20token(WETH_ADDRESS)


@pytest.fixture
def wbtc_weth_v2_lp(
    _bot,
) -> UniswapV2Pool:
    pool = _bot.build_pool(WBTC_WETH_V2_POOL_ADDRESS)
    pool.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=pool.update_block,
            reserves_token0=16231137593,
            reserves_token1=2571336301536722443178,
        )
    )

    return pool


import json
import os
import pathlib

_SNAP = json.load(
    pathlib.Path(
        os.path.join(os.path.dirname(__file__), "../fixtures/wbtc_weth_v3_snapshot.json")
    ).open()
)
_WBTC_WETH_V3_TICK_BITMAP = {
    int(k): UniswapV3BitmapAtWord(bitmap=v["bitmap"], block=v["block"])
    for k, v in _SNAP["tick_bitmap"].items()
}
_WBTC_WETH_V3_TICK_DATA = {
    int(k): UniswapV3LiquidityAtTick(
        liquidity_net=v["liquidity_net"],
        liquidity_gross=v["liquidity_gross"],
        block=v["block"],
    )
    for k, v in _SNAP["tick_data"].items()
}


@pytest.fixture
def wbtc_weth_v3_lp(_bot) -> UniswapV3Pool:
    pool = _bot.build_pool(
        WBTC_WETH_V3_POOL_ADDRESS,
        tick_bitmap=_WBTC_WETH_V3_TICK_BITMAP,
        tick_data=_WBTC_WETH_V3_TICK_DATA,
    )

    pool._initial_state_block = 0
    pool.external_update(
        UniswapV3PoolExternalUpdate(
            block_number=pool.update_block,
            liquidity=1612978974357835825,
            sqrt_price_x96=31549217861118002279483878013792428,
            tick=257907,
        )
    )

    return pool


@pytest.fixture
def wbtc_weth_arb(
    wbtc_weth_v2_lp: UniswapV2Pool,
    wbtc_weth_v3_lp: UniswapV3Pool,
    weth_token: Erc20Token,
):
    return UniswapLpCycle(
        id="test_arb",
        input_token=weth_token,
        swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp],
        max_input=100 * 10**18,
    )


def test_create_with_either_token_input(
    wbtc_weth_v2_lp: UniswapV2Pool,
    wbtc_weth_v3_lp: UniswapV3Pool,
    weth_token: Erc20Token,
    wbtc_token: Erc20Token,
):
    UniswapLpCycle(
        id="test_arb",
        input_token=weth_token,
        swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp],
        max_input=100 * 10**18,
    )
    UniswapLpCycle(
        id="test_arb",
        input_token=wbtc_token,
        swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp],
        max_input=100 * 10**18,
    )


def test_arbitrage_with_overrides(
    wbtc_weth_arb: UniswapLpCycle,
    wbtc_weth_v2_lp: UniswapV2Pool,
    wbtc_weth_v3_lp: UniswapV3Pool,
    weth_token: Erc20Token,
    wbtc_token: Erc20Token,
):
    v2_pool_state_override = UniswapV2PoolState(
        address=wbtc_weth_v2_lp.address,
        reserves_token0=16027096956,
        reserves_token1=2602647332090181827846,
        block=None,
    )

    v3_pool_state_override = UniswapV3PoolState(
        address=wbtc_weth_v3_lp.address,
        block=None,
        liquidity=1533143241938066251,
        sqrt_price_x96=31881290961944305252140777263703426,
        tick=258116,
        tick_bitmap=wbtc_weth_v3_lp.tick_bitmap,
        tick_data=wbtc_weth_v3_lp.tick_data,
    )

    overrides: dict[Pool, PoolState]

    # Override both pools
    overrides = {
        wbtc_weth_v2_lp: v2_pool_state_override,
        wbtc_weth_v3_lp: v3_pool_state_override,
    }

    # ArbSolver may find profit where scipy did not; either outcome is valid
    try:
        result_both = wbtc_weth_arb.calculate(state_overrides=overrides)
    except (ArbitrageError, OptimizationError):
        pass  # not profitable: acceptable
    else:
        assert result_both.profit_amount > 0

    # Override V2 pool only
    overrides = {
        wbtc_weth_v2_lp: v2_pool_state_override,
    }

    try:
        result_v2 = wbtc_weth_arb.calculate(state_overrides=overrides)
    except (ArbitrageError, OptimizationError):
        pass  # not profitable: acceptable
    else:
        assert result_v2.profit_amount > 0

    # Override V3 pool only
    overrides = {
        wbtc_weth_v3_lp: v3_pool_state_override,
    }

    result = wbtc_weth_arb.calculate(state_overrides=overrides)
    assert result.profit_amount > 0
    # Optimal input should be in the same ballpark as the previous scipy result
    assert abs(result.input_amount - 20454968409226055680) < 20454968409226055680 * 0.01

    # Irrelevant V2 and V3 mocked pools, only the address is changed.
    irrelevant_v2_pool = MockLiquidityPool()
    irrelevant_v2_pool.address = get_checksum_address("0x0000000000000000000000000000000000000069")
    irrelevant_v2_pool.name = "WBTC-WETH (V2, 0.30%)"
    irrelevant_v2_pool.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    irrelevant_v2_pool._fee_token0 = Fraction(3, 1000)
    irrelevant_v2_pool._fee_token1 = Fraction(3, 1000)
    irrelevant_v2_pool.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=1,
            reserves_token0=16231137593,
            reserves_token1=2571336301536722443178,
        )
    )
    irrelevant_v2_pool._token0 = wbtc_token
    irrelevant_v2_pool._token1 = weth_token

    irrelevant_v3_pool = MockV3LiquidityPool()
    irrelevant_v3_pool._initial_state_block = 0
    irrelevant_v3_pool.address = get_checksum_address("0x0000000000000000000000000000000000000420")
    irrelevant_v3_pool.external_update(
        UniswapV3PoolExternalUpdate(
            block_number=1,
            liquidity=1612978974357835825,
            sqrt_price_x96=31549217861118002279483878013792428,
            tick=257907,
        )
    )
    irrelevant_v3_pool.name = "WBTC-WETH (V3, 0.30%)"
    irrelevant_v3_pool.factory = get_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984")
    irrelevant_v3_pool._fee = 3000
    irrelevant_v3_pool._token0 = wbtc_token
    irrelevant_v3_pool._token1 = weth_token
    irrelevant_v3_pool._sparse_liquidity_map = False
    irrelevant_v3_pool._tick_spacing = 60

    overrides = {
        irrelevant_v2_pool: v2_pool_state_override,  # <--- entry should be ignored
        wbtc_weth_v3_lp: v3_pool_state_override,
    }

    # This should equal the result from the test with the V3 override only
    result = wbtc_weth_arb.calculate(state_overrides=overrides)
    assert result.profit_amount > 0
    assert abs(result.input_amount - 20454968409226055680) < 20454968409226055680 * 0.01


@pytest.mark.skip
async def test_pickle_uniswap_lp_cycle_with_camelot_pool(fork_arbitrum_full: AnvilFork):
    # Arbitrum-specific token addresses
    weth_address = "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1"
    wbtc_address = "0x2f2a2543B76A7835D0D6980C1E5735743EFb2A2d"

    camelot_weth_wbtc_pool_address = "0x96059759C6492fb4e8a9777b65f307F2C811a34F"
    sushi_v2_weth_wbtc_pool_address = "0x515e252b2b5c22b4b2b6Df66c2eBeeA871AA4d69"

    bot = make_bot_with_provider(ProviderAdapter.from_web3(fork_arbitrum_full.w3))
    weth = bot.build_erc20token(weth_address)
    camelot_lp = bot.build_pool(camelot_weth_wbtc_pool_address)
    sushi_lp = bot.build_pool(sushi_v2_weth_wbtc_pool_address)

    arb = UniswapLpCycle(
        id="test_arb",
        input_token=weth,
        swap_pools=[camelot_lp, sushi_lp],
        max_input=100 * 10**18,
    )
    pickle.dumps(arb)

    loop = asyncio.get_running_loop()

    with concurrent.futures.ProcessPoolExecutor(
        mp_context=multiprocessing.get_context("spawn"),
    ) as executor:
        tasks = [
            loop.run_in_executor(
                executor,
                arb.calculate,
            )
            for _ in range(8)
        ]

        for task in asyncio.as_completed(tasks):
            with contextlib.suppress(RateOfExchangeBelowMinimum):
                await task

    with contextlib.suppress(RateOfExchangeBelowMinimum):
        arb.calculate()


async def test_process_pool_calculation(
    wbtc_weth_arb: UniswapLpCycle,
    wbtc_weth_v3_lp: UniswapV3Pool,
    weth_token: Erc20Token,
):
    start = time.perf_counter()

    v3_pool_state_override = UniswapV3PoolState(
        address=wbtc_weth_v3_lp.address,
        block=None,
        liquidity=1533143241938066251,
        sqrt_price_x96=31881290961944305252140777263703426,
        tick=258116,
        tick_bitmap=wbtc_weth_v3_lp.tick_bitmap,
        tick_data=wbtc_weth_v3_lp.tick_data,
    )

    overrides: dict[Pool, PoolState] = {
        wbtc_weth_v3_lp: v3_pool_state_override,
    }

    with concurrent.futures.ProcessPoolExecutor(
        mp_context=multiprocessing.get_context("spawn"),
    ) as executor:
        with pytest.raises(ArbitrageError):
            await wbtc_weth_arb.calculate_with_pool(executor=executor)

        future = await wbtc_weth_arb.calculate_with_pool(
            executor=executor,
            state_overrides=overrides,
        )
        result = await future
        assert result.profit_amount > 0
        # Optimal input should be in the same ballpark as the previous scipy result
        assert abs(result.input_amount - 20454968409226055680) < 20454968409226055680 * 0.01

        # Saturate the process pool executor with multiple calculations.
        # Should reveal cases of excessive latency.
        num_futures = 64
        calculation_futures = [
            await wbtc_weth_arb.calculate_with_pool(
                executor=executor,
                state_overrides=overrides,
            )
            for _ in range(num_futures)
        ]

        assert len(calculation_futures) == num_futures
        for i, task in enumerate(asyncio.as_completed(calculation_futures)):
            await task
            print(
                f"Completed process_pool calc #{i}, {time.perf_counter() - start:.2f}s since start"
            )
        print(f"Completed {num_futures} calculations in {time.perf_counter() - start:.1f}s")

        assert isinstance(wbtc_weth_arb.swap_pools[1], UniswapV3Pool)
        wbtc_weth_arb.swap_pools[1]._sparse_liquidity_map = True
        with pytest.raises(DegenbotValueError, match=r"One or more V3 pools has a sparse bitmap."):
            await wbtc_weth_arb.calculate_with_pool(
                executor=executor,
                state_overrides=overrides,
            )


def test_pre_calc_check(weth_token: Erc20Token, wbtc_token: Erc20Token):
    lp_1 = MockLiquidityPool()
    lp_1.name = "WBTC-WETH (V2, 0.30%)"
    lp_1.address = get_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D940")
    lp_1.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    lp_1._fee_token0 = Fraction(3, 1000)
    lp_1._fee_token1 = Fraction(3, 1000)
    lp_1.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=1,
            reserves_token0=16000000000,
            reserves_token1=2500000000000000000000,
        )
    )
    lp_1._token0 = wbtc_token
    lp_1._token1 = weth_token

    lp_2 = MockLiquidityPool()
    lp_2.name = "WBTC-WETH (V2, 0.30%)"
    lp_2.address = get_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D941")
    lp_2.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    lp_2._fee_token0 = Fraction(3, 1000)
    lp_2._fee_token1 = Fraction(3, 1000)
    lp_2.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=1,
            reserves_token0=15000000000,
            reserves_token1=2500000000000000000000,
        )
    )
    lp_2._token0 = wbtc_token
    lp_2._token1 = weth_token

    # lp_1 price = 2500000000000000000000/16000000000 ~= 156250000000.00
    # lp_2 price = 2500000000000000000000/15000000000 ~= 166666666666.67

    # This arb path should result in a profitable calculation, since token1
    # price is higher in the second pool.
    # i.e. sell overpriced token0 (WETH) in pool0 for token1 (WBTC),
    # buy underpriced token0 (WETH) in pool1 with token1 (WBTC)
    arb = UniswapLpCycle(
        id="test_arb",
        input_token=weth_token,
        swap_pools=[lp_1, lp_2],
        max_input=100 * 10**18,
    )
    result = arb.calculate()
    arb.generate_payloads(
        from_address=ZERO_ADDRESS,
        pool_swap_amounts=result.swap_amounts,
        swap_amount=result.input_amount,
    )

    # This arb path should result in an unprofitable calculation, since token1
    # price is lower in the second pool.
    # i.e. sell underpriced token0 (WETH) in pool0 for token1 (WBTC),
    # buy overpriced token0 (WETH) in pool1 with token1 (WBTC)
    arb = UniswapLpCycle(
        id="test_arb", input_token=weth_token, swap_pools=[lp_2, lp_1], max_input=100 * 10**18
    )
    with pytest.raises(RateOfExchangeBelowMinimum):
        arb.calculate()


def test_bad_pool_in_constructor(
    wbtc_weth_v2_lp: UniswapV2Pool, wbtc_weth_v3_lp: UniswapV3Pool, weth_token: Erc20Token
):
    with pytest.raises(
        DegenbotValueError, match=f"Incompatible pool type \\({type(None)}\\) provided."
    ):
        UniswapLpCycle(
            id="test_arb",
            input_token=weth_token,
            swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp, None],
            max_input=100 * 10**18,
        )


def test_no_max_input(
    wbtc_weth_v2_lp: UniswapV2Pool, wbtc_weth_v3_lp: UniswapV3Pool, weth_token: Erc20Token
):
    arb = UniswapLpCycle(
        id="test_arb",
        input_token=weth_token,
        swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp],
    )
    assert arb.max_input == 100 * 10**18


def test_zero_max_input(
    wbtc_weth_v2_lp: UniswapV2Pool, wbtc_weth_v3_lp: UniswapV3Pool, weth_token: Erc20Token
):
    with pytest.raises(DegenbotValueError, match=r"Maximum input must be positive."):
        UniswapLpCycle(
            id="test_arb",
            input_token=weth_token,
            swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp],
            max_input=0,
        )


def test_arbitrage_helper_subscriptions(
    wbtc_weth_arb: UniswapLpCycle, wbtc_weth_v2_lp: UniswapV2Pool, wbtc_weth_v3_lp: UniswapV3Pool
):
    assert wbtc_weth_arb in wbtc_weth_v2_lp._subscribers
    assert wbtc_weth_arb in wbtc_weth_v3_lp._subscribers

    pool_subscriber = FakeSubscriber()
    pool_subscriber.subscribe(publisher=wbtc_weth_v2_lp)
    pool_subscriber.subscribe(publisher=wbtc_weth_v3_lp)

    assert len(pool_subscriber.inbox) == 0

    # Trigger pool state updates
    wbtc_weth_v2_lp.external_update(
        update=UniswapV2PoolExternalUpdate(
            block_number=wbtc_weth_v2_lp.update_block,
            reserves_token0=69,
            reserves_token1=420,
        )
    )

    wbtc_weth_v3_lp.external_update(
        update=UniswapV3PoolExternalUpdate(
            block_number=wbtc_weth_v3_lp.update_block,
            liquidity=69_420,
            sqrt_price_x96=1,
            tick=-1,
        )
    )

    # Verify the subscribers have received state update notifications
    assert len(pool_subscriber.inbox) == 2
    assert pool_subscriber.inbox[0]["from"] == wbtc_weth_v2_lp
    assert pool_subscriber.inbox[1]["from"] == wbtc_weth_v3_lp
    assert isinstance(pool_subscriber.inbox[0]["message"], UniswapV2PoolStateUpdated)
    assert isinstance(pool_subscriber.inbox[1]["message"], UniswapV3PoolStateUpdated)

    pool_subscriber.unsubscribe(wbtc_weth_v2_lp)
    pool_subscriber.unsubscribe(wbtc_weth_v3_lp)


def test_pool_helper_unsubscriptions(
    wbtc_weth_arb: UniswapLpCycle, wbtc_weth_v2_lp: UniswapV2Pool, wbtc_weth_v3_lp: UniswapV3Pool
):
    assert wbtc_weth_arb in wbtc_weth_v2_lp._subscribers
    assert wbtc_weth_arb in wbtc_weth_v3_lp._subscribers

    wbtc_weth_v2_lp.unsubscribe(wbtc_weth_arb)
    wbtc_weth_v3_lp.unsubscribe(wbtc_weth_arb)

    assert wbtc_weth_arb not in wbtc_weth_v2_lp._subscribers
    assert wbtc_weth_arb not in wbtc_weth_v3_lp._subscribers
