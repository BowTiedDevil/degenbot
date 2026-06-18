import json
import pathlib
from fractions import Fraction
from typing import TYPE_CHECKING

import pytest

from degenbot.anvil_fork import AnvilFork
from degenbot.arbitrage._legacy import _UniswapLpCycle as UniswapLpCycle
from degenbot.constants import ZERO_ADDRESS
from degenbot.erc20.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.arbitrage import (
    ArbitrageError,
    OptimizationError,
    RateOfExchangeBelowMinimum,
)
from degenbot.provider import ProviderAdapter
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.liquidity_pool import LiquidityPool
from degenbot.uniswap.v2_types import (
    UniswapV2PoolExternalUpdate,
    UniswapV2PoolState,
    UniswapV2PoolStateUpdated,
)
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v3_types import (
    UniswapV3PoolExternalUpdate,
    UniswapV3PoolState,
    UniswapV3PoolStateUpdated,
)
from tests.fakes.subscribers import FakeSubscriber
from tests.helpers.bot_factory import make_bot_with_provider
from tests.helpers.v2_pool_factory import make_v2_pool

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
) -> LiquidityPool:
    pool = _bot.build_pool(WBTC_WETH_V2_POOL_ADDRESS)
    pool.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=pool.update_block,
            reserves_token0=16231137593,
            reserves_token1=2571336301536722443178,
        )
    )

    return pool


_snap_path = pathlib.Path(__file__).parent / "../fixtures/wbtc_weth_v3_snapshot.json"
with _snap_path.open(encoding="utf-8") as _f:
    _SNAP = json.load(_f)


_WBTC_WETH_V3_TICK_BITMAP = {
    int(k): BitmapAtWord(bitmap=v["bitmap"], block=v["block"])
    for k, v in _SNAP["tick_bitmap"].items()
}
_WBTC_WETH_V3_TICK_DATA = {
    int(k): LiquidityAtTick(
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
    wbtc_weth_v2_lp: LiquidityPool,
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
    wbtc_weth_v2_lp: LiquidityPool,
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
    wbtc_weth_v2_lp: LiquidityPool,
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

    # Irrelevant V2 and V3 pools, only the address is changed.
    irrelevant_v2_pool = make_v2_pool(
        address="0x0000000000000000000000000000000000000069",
        token0=wbtc_token,  # type: ignore[arg-type]
        token1=weth_token,  # type: ignore[arg-type]
        factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=16231137593,
        reserves_token1=2571336301536722443178,
        state_block=1,
    )

    _irrelevant_v3_pool = UniswapV3Pool(
        address="0x0000000000000000000000000000000000000420",
        token0=wbtc_token,  # type: ignore[arg-type]
        token1=weth_token,  # type: ignore[arg-type]
        factory="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        fee=3000,
        tick_spacing=60,
        sqrt_price_x96=31549217861118002279483878013792428,
        tick=257907,
        liquidity=1612978974357835825,
    )

    overrides = {
        irrelevant_v2_pool: v2_pool_state_override,  # <--- entry should be ignored
        wbtc_weth_v3_lp: v3_pool_state_override,
    }

    # This should equal the result from the test with the V3 override only
    result = wbtc_weth_arb.calculate(state_overrides=overrides)
    assert result.profit_amount > 0
    assert abs(result.input_amount - 20454968409226055680) < 20454968409226055680 * 0.01


def test_pre_calc_check(weth_token: Erc20Token, wbtc_token: Erc20Token):
    lp_1 = make_v2_pool(
        address="0xBb2b8038a1640196FbE3e38816F3e67Cba72D940",
        token0=wbtc_token,  # type: ignore[arg-type]
        token1=weth_token,  # type: ignore[arg-type]
        factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=16000000000,
        reserves_token1=2500000000000000000000,
        state_block=1,
    )

    lp_2 = make_v2_pool(
        address="0xBb2b8038a1640196FbE3e38816F3e67Cba72D941",
        token0=wbtc_token,  # type: ignore[arg-type]
        token1=weth_token,  # type: ignore[arg-type]
        factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=15000000000,
        reserves_token1=2500000000000000000000,
        state_block=1,
    )

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
    wbtc_weth_v2_lp: LiquidityPool, wbtc_weth_v3_lp: UniswapV3Pool, weth_token: Erc20Token
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
    wbtc_weth_v2_lp: LiquidityPool, wbtc_weth_v3_lp: UniswapV3Pool, weth_token: Erc20Token
):
    arb = UniswapLpCycle(
        id="test_arb",
        input_token=weth_token,
        swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp],
    )
    assert arb.max_input == 100 * 10**18


def test_zero_max_input(
    wbtc_weth_v2_lp: LiquidityPool, wbtc_weth_v3_lp: UniswapV3Pool, weth_token: Erc20Token
):
    with pytest.raises(DegenbotValueError, match=r"Maximum input must be positive."):
        UniswapLpCycle(
            id="test_arb",
            input_token=weth_token,
            swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp],
            max_input=0,
        )


def test_arbitrage_helper_subscriptions(
    wbtc_weth_arb: UniswapLpCycle, wbtc_weth_v2_lp: LiquidityPool, wbtc_weth_v3_lp: UniswapV3Pool
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
    wbtc_weth_arb: UniswapLpCycle, wbtc_weth_v2_lp: LiquidityPool, wbtc_weth_v3_lp: UniswapV3Pool
):
    assert wbtc_weth_arb in wbtc_weth_v2_lp._subscribers
    assert wbtc_weth_arb in wbtc_weth_v3_lp._subscribers

    wbtc_weth_v2_lp.unsubscribe(wbtc_weth_arb)
    wbtc_weth_v3_lp.unsubscribe(wbtc_weth_arb)

    assert wbtc_weth_arb not in wbtc_weth_v2_lp._subscribers
    assert wbtc_weth_arb not in wbtc_weth_v3_lp._subscribers
