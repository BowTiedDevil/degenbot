import pathlib
from pathlib import Path
from typing import Any

import pydantic_core
import pytest

from degenbot._ffi import Bot as _Engine
from degenbot.bot import Bot
from degenbot.checksum_cache import get_checksum_address
from degenbot.erc20.erc20 import Erc20Token
from degenbot.exceptions import ContractLogicError
from degenbot.exceptions.base import DegenbotValueError
from degenbot.exceptions.pool import (
    IncompleteSwap,
    LiquidityPoolError,
)
from degenbot.fork import AnvilFork
from degenbot.types.chain import ChainId
from degenbot.uniswap.concentrated.types import LiquidityAtTick
from degenbot.uniswap.deployments import (
    UniswapFactoryDeployment,
    UniswapV3ExchangeDeployment,
)
from degenbot.uniswap.math import (
    MAX_SQRT_RATIO,
    MIN_SQRT_RATIO,
    MIN_TICK,
)
from degenbot.uniswap.math import (
    get_tick_word_and_bit_position as concentrated_liquidity_get_tick_word_and_bit_position,
)
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v3_types import (
    UniswapV3PoolSimulationResult,
    UniswapV3PoolState,
)
from tests.golden.recorded_pool import load_pool
from tests.helpers.bot_factory import make_bot_with_provider
from tests.helpers.contract_compat import make_contract
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.swap_amount_multipliers import UNISWAP_TOKEN_AMOUNT_MULTIPLIERS

WBTC_WETH_V3_POOL_ADDRESS = get_checksum_address("0xCBCdF9626bC03E24f779434178A73a0B4bad62eD")
WETH_CONTRACT_ADDRESS = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
WBTC_CONTRACT_ADDRESS = get_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")
DAI_CONTRACT_ADDRESS = get_checksum_address("0x6B175474E89094C44Da98b954EedeAC495271d0F")
UNISWAP_V3_FACTORY_ADDRESS = get_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984")
UNISWAP_V3_QUOTER_ADDRESS = get_checksum_address("0xb27308f9F90D607463bb33eA1BeBb41C27CE5AB6")
BASE_CBETH_WETH_V3_POOL_ADDRESS = get_checksum_address("0x257fcbae4ac6b26a02e4fc5e1a11e4174b5ce395")
BASE_PANCAKESWAP_V3_FACTORY_ADDRESS = get_checksum_address(
    "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865",
)
BASE_PANCAKESWAP_V3_DEPLOYER_ADDRESS = get_checksum_address(
    "0x41ff9AA7e16B8B1a8a8dc4f0eFacd93D02d071c9",
)
BASE_PANCAKESWAP_V3_EXCHANGE = UniswapV3ExchangeDeployment(
    name="PancakeSwap V3",
    chain_id=ChainId.BASE,
    factory=UniswapFactoryDeployment(
        address=BASE_PANCAKESWAP_V3_FACTORY_ADDRESS,
        deployer=BASE_PANCAKESWAP_V3_DEPLOYER_ADDRESS,
        pool_init_hash="0x6ce8eb472fa82df5469c6ab6d485f17c3ad13c8cd7af59b3d4a8026c5ce0f7e2",
    ),
)

UNISWAP_V3_QUOTER_ABI = pydantic_core.from_json(
    """
    [{"inputs":[{"internalType":"address","name":"_factory","type":"address"},{"internalType":"address","name":"_WETH9","type":"address"}],"stateMutability":"nonpayable","type":"constructor"},{"inputs":[],"name":"WETH9","outputs":[{"internalType":"address","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"factory","outputs":[{"internalType":"address","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"bytes","name":"path","type":"bytes"},{"internalType":"uint256","name":"amountIn","type":"uint256"}],"name":"quoteExactInput","outputs":[{"internalType":"uint256","name":"amountOut","type":"uint256"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"address","name":"tokenIn","type":"address"},{"internalType":"address","name":"tokenOut","type":"address"},{"internalType":"uint24","name":"fee","type":"uint24"},{"internalType":"uint256","name":"amountIn","type":"uint256"},{"internalType":"uint160","name":"sqrtPriceLimitX96","type":"uint160"}],"name":"quoteExactInputSingle","outputs":[{"internalType":"uint256","name":"amountOut","type":"uint256"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"bytes","name":"path","type":"bytes"},{"internalType":"uint256","name":"amountOut","type":"uint256"}],"name":"quoteExactOutput","outputs":[{"internalType":"uint256","name":"amountIn","type":"uint256"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"address","name":"tokenIn","type":"address"},{"internalType":"address","name":"tokenOut","type":"address"},{"internalType":"uint24","name":"fee","type":"uint24"},{"internalType":"uint256","name":"amountOut","type":"uint256"},{"internalType":"uint160","name":"sqrtPriceLimitX96","type":"uint160"}],"name":"quoteExactOutputSingle","outputs":[{"internalType":"uint256","name":"amountIn","type":"uint256"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"int256","name":"amount0Delta","type":"int256"},{"internalType":"int256","name":"amount1Delta","type":"int256"},{"internalType":"bytes","name":"path","type":"bytes"}],"name":"uniswapV3SwapCallback","outputs":[],"stateMutability":"view","type":"function"}]
    """,
)


_V3_WBTC_WETH_GOLDEN = Path("tests/golden/data/uniswap/v3/wbtc_weth/17600000.json.gz")
_PY_BOT = _Engine()


@pytest.fixture
def dai() -> Erc20Token:
    return make_erc20(
        _PY_BOT,
        DAI_CONTRACT_ADDRESS,
        name="Dai Stablecoin",
        symbol="DAI",
        decimals=18,
        chain_id=1,
    )


def _load_wbtc_weth_v3_pool() -> UniswapV3Pool:
    """Return the I/O-free WBTC/WETH V3 pool recorded at block 17,600,000 (no RPC)."""
    pool = load_pool(_V3_WBTC_WETH_GOLDEN, chain_id=1, block=17_600_000)
    assert isinstance(pool, UniswapV3Pool)
    return pool


@pytest.fixture
def wbtc_weth_v3_lp_at_historical_block() -> UniswapV3Pool:
    return _load_wbtc_weth_v3_pool()


@pytest.fixture
def wbtc_weth_v3_lp() -> UniswapV3Pool:
    return _load_wbtc_weth_v3_pool()


@pytest.fixture
def testing_pools() -> Any:
    pools = pydantic_core.from_json(
        pathlib.Path("tests/uniswap/v3/first_200_uniswap_v3_pools.json").read_bytes(),
    )
    assert len(pools) == 200
    return pools


@pytest.mark.slow
def test_first_200_pools(
    fork_mainnet_full: AnvilFork,
    testing_pools,
):
    bot = make_bot_with_provider(fork_mainnet_full.provider)

    quoter = make_contract(
        fork_mainnet_full.http_url, UNISWAP_V3_QUOTER_ADDRESS, UNISWAP_V3_QUOTER_ABI
    )

    for pool in testing_pools:
        pool_address: str = pool["pool_address"]

        lp = bot.build_pool(pool_address)

        max_reserves_token0 = 1 * 10**lp.token0.decimals
        max_reserves_token1 = 1 * 10**lp.token1.decimals

        for token_mult in UNISWAP_TOKEN_AMOUNT_MULTIPLIERS:
            token_in_amount = max(1, int(token_mult * max_reserves_token0))

            try:
                quoter_amount_out = quoter.functions.quoteExactInputSingle(
                    lp.token0.address,  # tokenIn
                    lp.token1.address,  # tokenOut
                    lp.fee,  # fee
                    token_in_amount,  # amountIn
                    MIN_SQRT_RATIO + 1,  # sqrtPriceLimitX96
                ).call()
            except ContractLogicError:
                continue

            if quoter_amount_out == 0:
                continue

            try:
                helper_amount_out = lp.calculate_tokens_out_from_tokens_in(
                    token_in=lp.token0,
                    token_in_quantity=token_in_amount,
                )
            except IncompleteSwap as exc:
                helper_amount_out = exc.amount_out

            assert helper_amount_out == quoter_amount_out

        for token_mult in UNISWAP_TOKEN_AMOUNT_MULTIPLIERS:
            token_in_amount = max(1, int(token_mult * max_reserves_token1))

            try:
                quoter_amount_out = quoter.functions.quoteExactInputSingle(
                    lp.token1.address,  # tokenIn
                    lp.token0.address,  # tokenOut
                    lp.fee,  # fee
                    token_in_amount,  # amountIn
                    MAX_SQRT_RATIO - 1,  # sqrtPriceLimitX96
                ).call()
            except ContractLogicError:
                continue

            if quoter_amount_out == 0:
                continue

            try:
                helper_amount_out = lp.calculate_tokens_out_from_tokens_in(
                    token_in=lp.token1,
                    token_in_quantity=token_in_amount,
                )
            except IncompleteSwap as exc:
                helper_amount_out = exc.amount_out

            assert helper_amount_out == quoter_amount_out


@pytest.mark.online_rpc
def test_pool_creation(bot_mainnet_full: Bot) -> None:
    bot_mainnet_full.build_pool(WBTC_WETH_V3_POOL_ADDRESS)


@pytest.mark.online_rpc
def test_pool_creation_with_liquidity_map(bot_mainnet_full: Bot) -> None:
    # Pools built without explicit tick data have sparse liquidity maps
    pool = bot_mainnet_full.build_pool(WBTC_WETH_V3_POOL_ADDRESS)
    assert pool.sparse_liquidity_map is True


@pytest.mark.base
def test_built_pool_identity_matches_deployment_truth(fork_base_full: AnvilFork) -> None:
    # The wrong-pool-type guard used to raise here by construction. The one
    # build entry resolves the family from chain data now, so the same meaning
    # is "the object handed back IS the deployment's type, immutably" — a
    # mislabeled pool would price swaps with the wrong fee model.
    bot = make_bot_with_provider(fork_base_full.provider)
    pancake_pool_address = "0xC07d7737FD8A06359E9C877863119Bf5F6abFb9E"
    pool = bot.build_pool(pancake_pool_address)
    assert bot.resolve_pool_type(pancake_pool_address) == pool.pool_type
    with pytest.raises(DegenbotValueError):
        bot.resolve_pool_type("0x" + "00" * 20)
    assert pool.pool_type.kind == "pancakeswap_v3"
    assert pool.pool_type.variant == "pancakeswap"
    with pytest.raises(AttributeError):
        pool.pool_type = pool.pool_type  # type: ignore[misc]


@pytest.mark.base
def test_pancake_v3_pool_creation(fork_base_full: AnvilFork) -> None:
    bot = make_bot_with_provider(fork_base_full.provider)
    bot.build_pool("0xC07d7737FD8A06359E9C877863119Bf5F6abFb9E")


@pytest.mark.online_rpc
def test_sparse_liquidity_map(fork_mainnet_full: AnvilFork) -> None:
    bot = make_bot_with_provider(fork_mainnet_full.provider)
    lp = bot.build_pool(WBTC_WETH_V3_POOL_ADDRESS)
    current_word, _ = concentrated_liquidity_get_tick_word_and_bit_position(
        MIN_TICK, lp.tick_spacing
    )
    assert lp.sparse_liquidity_map is True
    assert current_word + 1 not in lp.tick_bitmap

    # Pools built without tick data can still perform swaps
    # The tick data fetcher will fetch missing data on-demand.
    # 10 WBTC crosses several bitmap words, exercising the on-demand fetch,
    # while staying within the pool's range liquidity (a 100k-WBTC swap
    # outruns the pool's entire range and cannot be served at any depth).
    lp.calculate_tokens_out_from_tokens_in(
        token_in=lp.token0,
        token_in_quantity=10 * 10**lp.token0.decimals,
    )


def test_tick_data_equality() -> None:
    with pytest.raises(AssertionError):
        assert LiquidityAtTick(liquidity_net=1, liquidity_gross=2, block=0) == LiquidityAtTick(
            liquidity_net=1,
            liquidity_gross=4,
            block=0,
        )
    with pytest.raises(AssertionError):
        assert LiquidityAtTick(liquidity_net=1, liquidity_gross=2, block=0) == LiquidityAtTick(
            liquidity_net=4,
            liquidity_gross=2,
            block=0,
        )

    assert LiquidityAtTick(liquidity_net=1, liquidity_gross=2, block=0) == LiquidityAtTick(
        liquidity_net=1,
        liquidity_gross=2,
        block=0,
    )


def test_price_is_inverse_of_exchange_rate(wbtc_weth_v3_lp: UniswapV3Pool):
    for token in [wbtc_weth_v3_lp.token0, wbtc_weth_v3_lp.token1]:
        assert (
            wbtc_weth_v3_lp.get_absolute_price(token)
            == wbtc_weth_v3_lp.get_absolute_exchange_rate(token) ** -1
        )


def test_nominal_rate_scaled_by_decimals(wbtc_weth_v3_lp: UniswapV3Pool):
    for token in [wbtc_weth_v3_lp.token0, wbtc_weth_v3_lp.token1]:
        nom_rate = int(wbtc_weth_v3_lp.get_nominal_rate(token))
        abs_rate = int(wbtc_weth_v3_lp.get_absolute_exchange_rate(token))
        assert nom_rate == abs_rate // (
            10 ** (wbtc_weth_v3_lp.token1.decimals - wbtc_weth_v3_lp.token0.decimals)
        )


def test_nominal_price_scaled_by_decimals(wbtc_weth_v3_lp: UniswapV3Pool):
    for token in [wbtc_weth_v3_lp.token0, wbtc_weth_v3_lp.token1]:
        nom_price = int(wbtc_weth_v3_lp.get_nominal_price(token))
        abs_price = int(wbtc_weth_v3_lp.get_absolute_price(token))
        assert nom_price == abs_price // (
            10 ** (wbtc_weth_v3_lp.token1.decimals - wbtc_weth_v3_lp.token0.decimals)
        )


def test_calculate_tokens_out_from_tokens_in(
    wbtc_weth_v3_lp_at_historical_block: UniswapV3Pool,
) -> None:
    lp: UniswapV3Pool = wbtc_weth_v3_lp_at_historical_block

    assert (
        lp.calculate_tokens_out_from_tokens_in(
            token_in=lp.token0,
            token_in_quantity=1 * 10**8,
        )
        == 15808930695950518795
    )
    assert (
        lp.calculate_tokens_out_from_tokens_in(
            token_in=lp.token1,
            token_in_quantity=1 * 10**18,
        )
        == 6287477
    )


def test_calculate_tokens_out_from_tokens_in_with_override(
    wbtc_weth_v3_lp_at_historical_block: UniswapV3Pool,
) -> None:
    # Overridden state values for this test are taken at block height 17,650,000

    lp: UniswapV3Pool = wbtc_weth_v3_lp_at_historical_block
    pool_state_override = UniswapV3PoolState(
        address=lp.address,
        block=None,
        liquidity=1533143241938066251,
        sqrt_price_x96=31881290961944305252140777263703426,
        tick=258116,
        tick_bitmap=lp.tick_bitmap,
        tick_data=lp.tick_data,
    )

    assert (
        lp.calculate_tokens_out_from_tokens_in(
            token_in=lp.token1,
            token_in_quantity=1 * 10**18,
            override_state=pool_state_override,
        )
        == 6157179
    )


def test_calculate_tokens_in_from_tokens_out(
    wbtc_weth_v3_lp_at_historical_block: UniswapV3Pool,
) -> None:
    lp: UniswapV3Pool = wbtc_weth_v3_lp_at_historical_block
    assert (
        lp.calculate_tokens_in_from_tokens_out(
            token_out=lp.token1,
            token_out_quantity=1 * 10**18,
        )
        == 6325394
    )

    assert (
        lp.calculate_tokens_in_from_tokens_out(
            token_out=lp.token0,
            token_out_quantity=1 * 10**8,
        )
        == 15904996952773072855
    )


def test_calculate_tokens_in_from_tokens_out_with_override(
    wbtc_weth_v3_lp_at_historical_block: UniswapV3Pool,
) -> None:
    # Overridden state values for this test are taken at block height 17,650,000

    lp: UniswapV3Pool = wbtc_weth_v3_lp_at_historical_block
    pool_state_override = UniswapV3PoolState(
        address=lp.address,
        block=None,
        liquidity=1533143241938066251,
        sqrt_price_x96=31881290961944305252140777263703426,
        tick=258116,
        tick_bitmap=lp.tick_bitmap,
        tick_data=lp.tick_data,
    )

    assert (
        lp.calculate_tokens_in_from_tokens_out(
            token_out=lp.token0,
            token_out_quantity=6157179,
            override_state=pool_state_override,
        )
        == 999999892383362636
    )


def test_simulations(wbtc_weth_v3_lp_at_historical_block: UniswapV3Pool) -> None:
    lp: UniswapV3Pool = wbtc_weth_v3_lp_at_historical_block
    # 1 WETH -> WBTC swap
    weth_amount_in = 1 * 10**18

    simulated_state = UniswapV3PoolSimulationResult(
        amount0_delta=-6287477,
        amount1_delta=1000000000000000000,
        specified_met=True,
        initial_state=lp.state,
        final_state=UniswapV3PoolState(
            address=lp.address,
            liquidity=1612978974357835825,
            sqrt_price_x96=31549266832914462409708360853542079,
            tick=257907,
            tick_bitmap=lp.tick_bitmap,
            tick_data=lp.tick_data,
            block=17_600_000,
        ),
    )

    assert (
        lp.simulate_exact_input_swap(
            token_in=lp.token1,
            token_in_quantity=weth_amount_in,
        )
        == simulated_state
    )
    assert (
        lp.calculate_tokens_out_from_tokens_in(
            token_in=lp.token1,
            token_in_quantity=weth_amount_in,
        )
        == -simulated_state.amount0_delta
    )
    assert weth_amount_in == simulated_state.amount1_delta

    # 1 WBTC -> WETH swap
    wbtc_amount_in = 1 * 10**8

    simulated_state = UniswapV3PoolSimulationResult(
        amount0_delta=100000000,
        amount1_delta=-15808930695950518795,
        specified_met=True,
        initial_state=lp.state,
        final_state=UniswapV3PoolState(
            address=lp.address,
            liquidity=1612978974357835825,
            sqrt_price_x96=31548441339817807300885591332345643,
            tick=257906,
            tick_bitmap=lp.tick_bitmap,
            tick_data=lp.tick_data,
            block=17_600_000,
        ),
    )

    assert (
        lp.simulate_exact_input_swap(
            token_in=lp.token0,
            token_in_quantity=wbtc_amount_in,
        )
        == simulated_state
    )
    assert (
        lp.calculate_tokens_out_from_tokens_in(
            token_in=lp.token0,
            token_in_quantity=wbtc_amount_in,
        )
        == -simulated_state.amount1_delta
    )
    assert wbtc_amount_in == simulated_state.amount0_delta


def test_simulation_input_validation(
    wbtc_weth_v3_lp: UniswapV3Pool,
    dai: Erc20Token,
) -> None:
    with pytest.raises(DegenbotValueError, match=f"Unknown token {dai}"):
        wbtc_weth_v3_lp.simulate_exact_input_swap(
            token_in=dai,
            token_in_quantity=1,
        )
    with pytest.raises(DegenbotValueError, match=f"Unknown token {dai}"):
        wbtc_weth_v3_lp.simulate_exact_output_swap(
            token_out=dai,
            token_out_quantity=69,
        )


def test_simulations_with_override(
    wbtc_weth_v3_lp: UniswapV3Pool,
) -> None:
    # Overridden state values for this test are taken at block height 17,650,000

    # Capture tick data before the simulation to use in the expected result
    tick_bitmap_before = wbtc_weth_v3_lp.tick_bitmap
    tick_data_before = wbtc_weth_v3_lp.tick_data

    pool_state_override = UniswapV3PoolState(
        address=wbtc_weth_v3_lp.address,
        block=None,
        liquidity=1533143241938066251,
        sqrt_price_x96=31881290961944305252140777263703426,
        tick=258116,
        tick_bitmap=tick_bitmap_before,
        tick_data=tick_data_before,
    )

    assert wbtc_weth_v3_lp.simulate_exact_input_swap(
        token_in=wbtc_weth_v3_lp.token1,
        token_in_quantity=1 * 10**18,
        override_state=pool_state_override,
    ) == UniswapV3PoolSimulationResult(
        amount0_delta=-6157179,
        amount1_delta=1 * 10**18,
        specified_met=True,
        initial_state=pool_state_override,
        final_state=UniswapV3PoolState(
            address=wbtc_weth_v3_lp.address,
            block=None,
            sqrt_price_x96=31881342483860761583159860586051776,
            liquidity=1533143241938066251,
            tick=258116,
            tick_bitmap=tick_bitmap_before,
            tick_data=tick_data_before,
        ),
    )

    assert wbtc_weth_v3_lp.simulate_exact_output_swap(
        token_out=wbtc_weth_v3_lp.token0,
        token_out_quantity=6157179,
        override_state=pool_state_override,
    ) == UniswapV3PoolSimulationResult(
        amount0_delta=-6157179,
        amount1_delta=999999892383362636,
        specified_met=True,
        initial_state=pool_state_override,
        final_state=UniswapV3PoolState(
            address=wbtc_weth_v3_lp.address,
            block=None,
            sqrt_price_x96=31881342483855216967760245337454994,
            liquidity=1533143241938066251,
            tick=258116,
            tick_bitmap=tick_bitmap_before,
            tick_data=tick_data_before,
        ),
    )


def test_zero_swaps(wbtc_weth_v3_lp_at_historical_block: UniswapV3Pool) -> None:
    with pytest.raises(LiquidityPoolError):
        assert (
            wbtc_weth_v3_lp_at_historical_block.calculate_tokens_out_from_tokens_in(
                wbtc_weth_v3_lp_at_historical_block.token0,
                0,
            )
            == 0
        )

    with pytest.raises(LiquidityPoolError):
        assert (
            wbtc_weth_v3_lp_at_historical_block.calculate_tokens_out_from_tokens_in(
                wbtc_weth_v3_lp_at_historical_block.token1,
                0,
            )
            == 0
        )


@pytest.mark.online_rpc
def test_swap_for_all(wbtc_weth_v3_lp_at_historical_block: UniswapV3Pool) -> None:
    # Runs against the full-range recorded golden (the tick map carries the
    # pool's walk to MIN/MAX_TICK), fully offline. The pool's range
    # capacities at block 17,600,000 (L = 1.6e18, the walk to the protocol
    # tick bounds): ~41k WBTC out on the token0 side, ~630k WETH out on the
    # token1 side. These requests exceed both, so the walk exhausts the
    # range and the pool raises its revert-analog (the real pool reverts the
    # same swap at the TickMath bound).

    with pytest.raises(IncompleteSwap):
        wbtc_weth_v3_lp_at_historical_block.calculate_tokens_in_from_tokens_out(
            token_out=wbtc_weth_v3_lp_at_historical_block.token0,
            token_out_quantity=100_000 * 10**wbtc_weth_v3_lp_at_historical_block.token0.decimals,
        )

    with pytest.raises(IncompleteSwap):
        wbtc_weth_v3_lp_at_historical_block.calculate_tokens_in_from_tokens_out(
            token_out=wbtc_weth_v3_lp_at_historical_block.token1,
            token_out_quantity=1_000_000 * 10**wbtc_weth_v3_lp_at_historical_block.token1.decimals,
        )


@pytest.mark.base
def test_base_pancakeswap_v3(fork_base_full: AnvilFork):
    bot = make_bot_with_provider(fork_base_full.provider)

    # Build a PancakeSwap V3 pool, which extends UniswapV3Pool
    lp = bot.build_pool(BASE_CBETH_WETH_V3_POOL_ADDRESS)
    assert lp is not None
