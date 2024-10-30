import pickle

import pytest
from eth_typing import ChainId
from eth_utils.address import to_checksum_address
from hexbytes import HexBytes
from web3 import Web3

from degenbot.anvil_fork import AnvilFork
from degenbot.config import set_web3
from degenbot.erc20_token import Erc20Token
from degenbot.exceptions import (
    AddressMismatch,
    DegenbotValueError,
    ExternalUpdateError,
    InsufficientAmountOutError,
    LateUpdateError,
    LiquidityPoolError,
    NoPoolStateAvailable,
)
from degenbot.managers.erc20_token_manager import Erc20TokenManager
from degenbot.pancakeswap.pools import PancakeV3Pool
from degenbot.uniswap.deployments import (
    FACTORY_DEPLOYMENTS,
    UniswapFactoryDeployment,
    UniswapV3ExchangeDeployment,
)
from degenbot.uniswap.types import (
    UniswapV3BitmapAtWord,
    UniswapV3LiquidityAtTick,
    UniswapV3PoolExternalUpdate,
    UniswapV3PoolSimulationResult,
    UniswapV3PoolState,
)
from degenbot.uniswap.v3_functions import get_tick_word_and_bit_position
from degenbot.uniswap.v3_libraries.tick_math import MAX_TICK, MIN_TICK
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

WBTC_WETH_V3_POOL_ADDRESS = to_checksum_address("0xCBCdF9626bC03E24f779434178A73a0B4bad62eD")
WETH_CONTRACT_ADDRESS = to_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
WBTC_CONTRACT_ADDRESS = to_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")
DAI_CONTRACT_ADDRESS = to_checksum_address("0x6B175474E89094C44Da98b954EedeAC495271d0F")
UNISWAP_V3_FACTORY_ADDRESS = to_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984")
BASE_CBETH_WETH_V3_POOL_ADDRESS = to_checksum_address("0x257fcbae4ac6b26a02e4fc5e1a11e4174b5ce395")
BASE_PANCAKESWAP_V3_FACTORY_ADDRESS = to_checksum_address(
    "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"
)
BASE_PANCAKESWAP_V3_DEPLOYER_ADDRESS = to_checksum_address(
    "0x41ff9AA7e16B8B1a8a8dc4f0eFacd93D02d071c9"
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


@pytest.fixture(autouse=True)
def dai(ethereum_archive_node_web3: Web3) -> Erc20Token:
    set_web3(ethereum_archive_node_web3)
    return Erc20TokenManager(chain_id=ChainId.ETH).get_erc20token(DAI_CONTRACT_ADDRESS)


@pytest.fixture(autouse=True)
def wbtc(ethereum_archive_node_web3: Web3) -> Erc20Token:
    set_web3(ethereum_archive_node_web3)
    return Erc20TokenManager(chain_id=ChainId.ETH).get_erc20token(WBTC_CONTRACT_ADDRESS)


@pytest.fixture(autouse=True)
def weth(ethereum_archive_node_web3: Web3) -> Erc20Token:
    set_web3(ethereum_archive_node_web3)
    return Erc20TokenManager(chain_id=ChainId.ETH).get_erc20token(WETH_CONTRACT_ADDRESS)


@pytest.fixture
def wbtc_weth_v3_lp_at_block_17_600_000(fork_mainnet: AnvilFork) -> UniswapV3Pool:
    fork_mainnet.reset(block_number=17_600_000)
    set_web3(fork_mainnet.w3)
    return UniswapV3Pool(WBTC_WETH_V3_POOL_ADDRESS)


@pytest.fixture
def wbtc_weth_v3_lp(fork_mainnet: AnvilFork) -> UniswapV3Pool:
    set_web3(fork_mainnet.w3)
    return UniswapV3Pool(WBTC_WETH_V3_POOL_ADDRESS)


def convert_unsigned_integer_to_signed(num: int):
    """
    Workaround for the values shown on Tenderly's "State Changes" view, which converts signed
    integers in a tuple to their unsigned representation
    """
    return int.from_bytes(HexBytes(num), byteorder="big", signed=True)


def test_fetching_tick_data(wbtc_weth_v3_lp_at_block_17_600_000: UniswapV3Pool):
    word_position, _ = get_tick_word_and_bit_position(
        tick=wbtc_weth_v3_lp_at_block_17_600_000.tick,
        tick_spacing=wbtc_weth_v3_lp_at_block_17_600_000.tick_spacing,
    )
    wbtc_weth_v3_lp_at_block_17_600_000._fetch_tick_data_at_word(word_position + 5)


def test_pool_creation(ethereum_archive_node_web3: Web3) -> None:
    set_web3(ethereum_archive_node_web3)
    UniswapV3Pool(address=WBTC_WETH_V3_POOL_ADDRESS)


def test_pool_creation_with_liquidity_map(ethereum_archive_node_web3: Web3) -> None:
    set_web3(ethereum_archive_node_web3)
    assert (
        UniswapV3Pool(
            address=WBTC_WETH_V3_POOL_ADDRESS, tick_bitmap={}, tick_data={}
        ).sparse_liquidity_map
        is False
    )


def test_creation_with_bad_liquidity_overrides(ethereum_archive_node_web3: Web3) -> None:
    set_web3(ethereum_archive_node_web3)
    with pytest.raises(DegenbotValueError, match="Provide both tick_bitmap and tick_data."):
        UniswapV3Pool(address=WBTC_WETH_V3_POOL_ADDRESS, tick_bitmap={0: {}})

    with pytest.raises(DegenbotValueError, match="Provide both tick_bitmap and tick_data."):
        UniswapV3Pool(address=WBTC_WETH_V3_POOL_ADDRESS, tick_data={0: {}})


def test_creation_with_invalid_hash(ethereum_archive_node_web3: Web3) -> None:
    set_web3(ethereum_archive_node_web3)

    # Delete the preset deployment for this factory so the test uses the provided override instead
    # of preferring the known valid deployment data
    factory_deployment = FACTORY_DEPLOYMENTS[ethereum_archive_node_web3.eth.chain_id][
        UNISWAP_V3_FACTORY_ADDRESS
    ]
    del FACTORY_DEPLOYMENTS[ethereum_archive_node_web3.eth.chain_id][UNISWAP_V3_FACTORY_ADDRESS]

    # Change last byte of true init hash
    bad_init_hash = UniswapV3Pool.UNISWAP_V3_MAINNET_POOL_INIT_HASH[:-1] + "f"

    with pytest.raises(AddressMismatch, match="Pool address verification failed"):
        UniswapV3Pool(
            address=WBTC_WETH_V3_POOL_ADDRESS,
            init_hash=bad_init_hash,
        )

    # Restore the preset deployments
    FACTORY_DEPLOYMENTS[ethereum_archive_node_web3.eth.chain_id][UNISWAP_V3_FACTORY_ADDRESS] = (
        factory_deployment
    )


def test_creation_with_wrong_pool_type(base_full_node_web3: Web3) -> None:
    set_web3(base_full_node_web3)

    # Attempting to build a Pancake V3 pool with a Uniswap V3 (vanilla) helper should fail during
    # the contract value lookup
    pancake_pool_address = "0xC07d7737FD8A06359E9C877863119Bf5F6abFb9E"
    with pytest.raises(LiquidityPoolError, match="Could not decode contract data"):
        UniswapV3Pool(pancake_pool_address)


def test_pancake_v3_pool_creation(base_full_node_web3: Web3) -> None:
    set_web3(base_full_node_web3)
    PancakeV3Pool("0xC07d7737FD8A06359E9C877863119Bf5F6abFb9E")


def test_sparse_liquidity_map(ethereum_archive_node_web3: Web3) -> None:
    set_web3(ethereum_archive_node_web3)

    lp = UniswapV3Pool(address=WBTC_WETH_V3_POOL_ADDRESS)
    current_word, _ = get_tick_word_and_bit_position(MIN_TICK, lp.tick_spacing)
    known_words = set(lp.tick_bitmap.keys())
    assert lp.sparse_liquidity_map is True
    assert current_word + 1 not in lp.tick_bitmap

    lp._fetch_tick_data_at_word(current_word + 1)
    assert lp.sparse_liquidity_map is True
    assert current_word + 1 in lp.tick_bitmap
    assert set(lp.tick_bitmap.keys()) == known_words.union([current_word + 1])

    lp.calculate_tokens_out_from_tokens_in(
        token_in=lp.token0, token_in_quantity=100000 * 10**lp.token0.decimals
    )


def test_external_update_with_sparse_liquidity_map(ethereum_archive_node_web3: Web3) -> None:
    set_web3(ethereum_archive_node_web3)

    lp = UniswapV3Pool(address=WBTC_WETH_V3_POOL_ADDRESS)
    current_word, _ = get_tick_word_and_bit_position(MIN_TICK, lp.tick_spacing)
    assert lp.sparse_liquidity_map is True
    assert current_word + 1 not in lp.tick_bitmap

    lp.external_update(
        update=UniswapV3PoolExternalUpdate(
            block_number=lp.update_block + 1,
            liquidity_change=(
                1,
                lp.tick_spacing * (MIN_TICK // lp.tick_spacing),
                lp.tick_spacing * (MAX_TICK // lp.tick_spacing),
            ),
        )
    )


def test_reorg(wbtc_weth_v3_lp_at_block_17_600_000: UniswapV3Pool) -> None:
    lp: UniswapV3Pool = wbtc_weth_v3_lp_at_block_17_600_000

    start_block = wbtc_weth_v3_lp_at_block_17_600_000._update_block + 1
    end_block = start_block + 10

    # Provide some dummy updates, then simulate a reorg back to the starting state
    starting_state = lp.state
    starting_liquidity = lp.liquidity

    block_states: dict[int, UniswapV3PoolState] = {
        wbtc_weth_v3_lp_at_block_17_600_000._update_block: lp.state
    }

    for block_number in range(start_block, end_block + 1, 1):
        lp.external_update(
            update=UniswapV3PoolExternalUpdate(
                block_number=block_number,
                liquidity=starting_liquidity + 10_000 * (block_number - start_block),
            ),
        )
        block_states[block_number] = lp.state

    last_block_state = lp.state

    # Cannot restore to a pool state before the first
    with pytest.raises(NoPoolStateAvailable):
        lp.restore_state_before_block(0)

    # Non-op, the pool should already meet the requested condition
    lp.restore_state_before_block(end_block + 1)
    assert lp.state == last_block_state

    # Unwind the updates and compare to the stored states at previous blocks
    for block_number in range(end_block + 1, start_block, -1):
        lp.restore_state_before_block(block_number)
        assert lp.state == block_states[block_number - 1]

    # Verify the pool has been returned to the starting state
    assert lp.state == starting_state


def test_discard_before_finalized(wbtc_weth_v3_lp_at_block_17_600_000: UniswapV3Pool) -> None:
    lp: UniswapV3Pool = wbtc_weth_v3_lp_at_block_17_600_000

    start_block = wbtc_weth_v3_lp_at_block_17_600_000._update_block + 1
    end_block = start_block + 10

    # Provide some dummy updates, then simulate a reorg back to the starting state
    starting_liquidity = lp.liquidity

    block_states: dict[int, UniswapV3PoolState] = {
        wbtc_weth_v3_lp_at_block_17_600_000._update_block: lp.state
    }

    for block_number in range(start_block, end_block + 1, 1):
        lp.external_update(
            update=UniswapV3PoolExternalUpdate(
                block_number=block_number,
                liquidity=starting_liquidity + 10_000 * (block_number - start_block),
            ),
        )
        block_states[block_number] = lp.state

    wbtc_weth_v3_lp_at_block_17_600_000.discard_states_before_block(end_block)
    assert wbtc_weth_v3_lp_at_block_17_600_000._state_cache is not None
    assert wbtc_weth_v3_lp_at_block_17_600_000._state_cache.keys() == set([end_block])


def test_discard_earlier_than_created(wbtc_weth_v3_lp_at_block_17_600_000: UniswapV3Pool) -> None:
    lp: UniswapV3Pool = wbtc_weth_v3_lp_at_block_17_600_000

    assert lp._state_cache is not None
    state_before_discard = lp._state_cache.copy()
    wbtc_weth_v3_lp_at_block_17_600_000.discard_states_before_block(lp.update_block - 1)
    assert lp._state_cache == state_before_discard


def test_discard_after_last_update(wbtc_weth_v3_lp_at_block_17_600_000: UniswapV3Pool) -> None:
    lp: UniswapV3Pool = wbtc_weth_v3_lp_at_block_17_600_000

    with pytest.raises(
        NoPoolStateAvailable, match=f"No pool state known prior to block {lp.update_block + 1}"
    ):
        wbtc_weth_v3_lp_at_block_17_600_000.discard_states_before_block(lp.update_block + 1)


def test_tick_bitmap_equality() -> None:
    with pytest.raises(AssertionError):
        assert UniswapV3BitmapAtWord(bitmap=1) == UniswapV3BitmapAtWord(bitmap=2)

    # `block` field is set with `compare=False`, so that only the bitmap is
    # considered by equality checks
    assert UniswapV3BitmapAtWord(bitmap=1, block=1) == UniswapV3BitmapAtWord(bitmap=1, block=2)


def test_pickle_pool(wbtc_weth_v3_lp_at_block_17_600_000: UniswapV3Pool):
    pickle.dumps(wbtc_weth_v3_lp_at_block_17_600_000)


def test_tick_data_equality() -> None:
    with pytest.raises(AssertionError):
        assert UniswapV3LiquidityAtTick(
            liquidity_net=1, liquidity_gross=2
        ) == UniswapV3LiquidityAtTick(liquidity_net=3, liquidity_gross=4)

    # `block` field is set with `compare=False`, so that only the liquidity is
    # considered by equality checks
    assert UniswapV3LiquidityAtTick(
        liquidity_net=1, liquidity_gross=2, block=3
    ) == UniswapV3LiquidityAtTick(liquidity_net=1, liquidity_gross=2, block=4)


def test_price_is_inverse_of_exchange_rate(wbtc_weth_v3_lp: UniswapV3Pool):
    for token in [wbtc_weth_v3_lp.token0, wbtc_weth_v3_lp.token1]:
        assert wbtc_weth_v3_lp.get_absolute_price(token) == 1 / wbtc_weth_v3_lp.get_absolute_rate(
            token
        )


def test_nominal_rate_scaled_by_decimals(wbtc_weth_v3_lp: UniswapV3Pool):
    for token in [wbtc_weth_v3_lp.token0, wbtc_weth_v3_lp.token1]:
        nom_rate = int(wbtc_weth_v3_lp.get_nominal_rate(token))
        abs_rate = int(wbtc_weth_v3_lp.get_absolute_rate(token))
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
    wbtc_weth_v3_lp_at_block_17_600_000: UniswapV3Pool,
) -> None:
    lp: UniswapV3Pool = wbtc_weth_v3_lp_at_block_17_600_000

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
    wbtc_weth_v3_lp_at_block_17_600_000: UniswapV3Pool,
) -> None:
    lp: UniswapV3Pool = wbtc_weth_v3_lp_at_block_17_600_000
    # Overridden reserve values for this test are taken at block height 17,650,000
    # Liquidity: 1533143241938066251
    # SqrtPrice: 31881290961944305252140777263703426
    # Tick: 258116

    pool_state_override = UniswapV3PoolState(
        pool=lp.address,
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
    wbtc_weth_v3_lp_at_block_17_600_000: UniswapV3Pool,
) -> None:
    lp: UniswapV3Pool = wbtc_weth_v3_lp_at_block_17_600_000
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
    wbtc_weth_v3_lp_at_block_17_600_000: UniswapV3Pool,
) -> None:
    lp: UniswapV3Pool = wbtc_weth_v3_lp_at_block_17_600_000
    # Overridden reserve values for this test are taken at block height 17,650,000
    # Liquidity: 1533143241938066251
    # SqrtPrice: 31881290961944305252140777263703426
    # Tick: 258116

    pool_state_override = UniswapV3PoolState(
        pool=lp.address,
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


def test_simulations(wbtc_weth_v3_lp_at_block_17_600_000: UniswapV3Pool) -> None:
    lp: UniswapV3Pool = wbtc_weth_v3_lp_at_block_17_600_000
    # 1 WETH -> WBTC swap
    weth_amount_in = 1 * 10**18

    simulated_state = UniswapV3PoolSimulationResult(
        amount0_delta=-6287477,
        amount1_delta=1000000000000000000,
        initial_state=lp.state,
        final_state=UniswapV3PoolState(
            pool=lp.address,
            liquidity=1612978974357835825,
            sqrt_price_x96=31549266832914462409708360853542079,
            tick=257907,
            tick_bitmap=lp.tick_bitmap,
            tick_data=lp.tick_data,
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
        initial_state=lp.state,
        final_state=UniswapV3PoolState(
            pool=lp.address,
            liquidity=1612978974357835825,
            sqrt_price_x96=31548441339817807300885591332345643,
            tick=257906,
            tick_bitmap=lp.tick_bitmap,
            tick_data=lp.tick_data,
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
    wbtc_weth_v3_lp_at_block_17_600_000: UniswapV3Pool,
    dai: Erc20Token,
) -> None:
    lp: UniswapV3Pool = wbtc_weth_v3_lp_at_block_17_600_000
    with pytest.raises(DegenbotValueError, match="token_in is unknown."):
        lp.simulate_exact_input_swap(
            token_in=dai,
            token_in_quantity=1,
        )
    with pytest.raises(DegenbotValueError, match="token_out is unknown."):
        lp.simulate_exact_output_swap(
            token_out=dai,
            token_out_quantity=69,
        )


def test_simulations_with_override(
    wbtc_weth_v3_lp_at_block_17_600_000: UniswapV3Pool,
) -> None:
    lp: UniswapV3Pool = wbtc_weth_v3_lp_at_block_17_600_000
    # Overridden reserve values for this test are taken at block height 17,650,000
    # Liquidity: 1533143241938066251
    # SqrtPrice: 31881290961944305252140777263703426
    # Tick: 258116

    pool_state_override = UniswapV3PoolState(
        pool=lp.address,
        liquidity=1533143241938066251,
        sqrt_price_x96=31881290961944305252140777263703426,
        tick=258116,
        tick_bitmap=lp.tick_bitmap,
        tick_data=lp.tick_data,
    )

    assert lp.simulate_exact_input_swap(
        token_in=lp.token1,
        token_in_quantity=1 * 10**18,
        override_state=pool_state_override,
    ) == UniswapV3PoolSimulationResult(
        amount0_delta=-6157179,
        amount1_delta=1 * 10**18,
        initial_state=pool_state_override,
        final_state=UniswapV3PoolState(
            pool=lp.address,
            sqrt_price_x96=31881342483860761583159860586051776,
            liquidity=1533143241938066251,
            tick=258116,
            tick_bitmap=lp.tick_bitmap,
            tick_data=lp.tick_data,
        ),
    )

    assert lp.simulate_exact_output_swap(
        token_out=lp.token0,
        token_out_quantity=6157179,
        override_state=pool_state_override,
    ) == UniswapV3PoolSimulationResult(
        amount0_delta=-6157179,
        amount1_delta=999999892383362636,
        initial_state=pool_state_override,
        final_state=UniswapV3PoolState(
            pool=lp.address,
            sqrt_price_x96=31881342483855216967760245337454994,
            liquidity=1533143241938066251,
            tick=258116,
            tick_bitmap=lp.tick_bitmap,
            tick_data=lp.tick_data,
        ),
    )


def test_zero_swaps(wbtc_weth_v3_lp_at_block_17_600_000: UniswapV3Pool) -> None:
    with pytest.raises(LiquidityPoolError):
        assert (
            wbtc_weth_v3_lp_at_block_17_600_000.calculate_tokens_out_from_tokens_in(
                wbtc_weth_v3_lp_at_block_17_600_000.token0,
                0,
            )
            == 0
        )

    with pytest.raises(LiquidityPoolError):
        assert (
            wbtc_weth_v3_lp_at_block_17_600_000.calculate_tokens_out_from_tokens_in(
                wbtc_weth_v3_lp_at_block_17_600_000.token1,
                0,
            )
            == 0
        )


def test_swap_for_all(wbtc_weth_v3_lp_at_block_17_600_000: UniswapV3Pool) -> None:
    with pytest.raises(InsufficientAmountOutError):
        # pool has ~94,000 WETH, calculation should throw
        wbtc_weth_v3_lp_at_block_17_600_000.calculate_tokens_in_from_tokens_out(
            token_out=wbtc_weth_v3_lp_at_block_17_600_000.token0,
            token_out_quantity=2500 * 10**8,
        )

    with pytest.raises(InsufficientAmountOutError):
        # pool has ~94,000 WETH, calculation should throw
        wbtc_weth_v3_lp_at_block_17_600_000.calculate_tokens_in_from_tokens_out(
            token_out=wbtc_weth_v3_lp_at_block_17_600_000.token1,
            token_out_quantity=150_000 * 10**18,
        )


def test_external_update(wbtc_weth_v3_lp_at_block_17_600_000: UniswapV3Pool) -> None:
    start_block = wbtc_weth_v3_lp_at_block_17_600_000._update_block + 1

    wbtc_weth_v3_lp_at_block_17_600_000.sparse_liquidity_map = False

    wbtc_weth_v3_lp_at_block_17_600_000.external_update(
        update=UniswapV3PoolExternalUpdate(
            block_number=start_block,
            liquidity=69,
        ),
    )
    assert wbtc_weth_v3_lp_at_block_17_600_000._update_block == start_block

    # Ensure liquidity data is available for the manipulated tick range
    tick_spacing = wbtc_weth_v3_lp_at_block_17_600_000.tick_spacing
    word_pos_1, _ = get_tick_word_and_bit_position(tick=-887160, tick_spacing=tick_spacing)
    word_pos_2, _ = get_tick_word_and_bit_position(tick=-887220, tick_spacing=tick_spacing)
    wbtc_weth_v3_lp_at_block_17_600_000._fetch_tick_data_at_word(word_pos_1)
    wbtc_weth_v3_lp_at_block_17_600_000._fetch_tick_data_at_word(word_pos_2)

    new_liquidity = 10_000_000_000

    wbtc_weth_v3_lp_at_block_17_600_000.external_update(
        update=UniswapV3PoolExternalUpdate(
            block_number=start_block,
            liquidity_change=(new_liquidity, -887160, -887220),
        ),
    )

    assert wbtc_weth_v3_lp_at_block_17_600_000._update_block == start_block

    # New liquidity is added to liquidityNet at lower tick, subtracted from upper tick.
    assert (
        wbtc_weth_v3_lp_at_block_17_600_000.tick_data[-887160].liquidity_net
        == 80064092962998 + new_liquidity
    )
    assert (
        wbtc_weth_v3_lp_at_block_17_600_000.tick_data[-887220].liquidity_net
        == 82174936226787 - new_liquidity
    )

    # New liquidity is added to liquidityGross on both sides.
    assert (
        wbtc_weth_v3_lp_at_block_17_600_000.tick_data[-887160].liquidity_gross
        == 80064092962998 + new_liquidity
    )
    assert (
        wbtc_weth_v3_lp_at_block_17_600_000.tick_data[-887220].liquidity_gross
        == 82174936226787 + new_liquidity
    )

    # Try an update for a past block
    with pytest.raises(ExternalUpdateError):
        wbtc_weth_v3_lp_at_block_17_600_000.external_update(
            update=UniswapV3PoolExternalUpdate(
                block_number=start_block - 1,
                liquidity=10,
            ),
        )

    # Update the liquidity and then submit a liquidity change for the previous block
    # which is valid, but the in-range liquidity should not have been changed
    # NOTE: tick = 257907
    wbtc_weth_v3_lp_at_block_17_600_000.external_update(
        update=UniswapV3PoolExternalUpdate(
            block_number=start_block + 1,
            liquidity=69_420_000,
        ),
    )

    # Now repeat the liquidity change for a newer block and check that the in-range liquidity was
    # adjusted
    wbtc_weth_v3_lp_at_block_17_600_000.external_update(
        update=UniswapV3PoolExternalUpdate(
            block_number=start_block + 1,
            liquidity_change=(1, 257880, 257940),
        ),
    )
    assert wbtc_weth_v3_lp_at_block_17_600_000.liquidity == 69_420_000 + 1

    wbtc_weth_v3_lp_at_block_17_600_000.external_update(
        update=UniswapV3PoolExternalUpdate(
            block_number=start_block + 2,
            tick=69,
        )
    )
    # Update twice to test branches that check for a no-change update
    wbtc_weth_v3_lp_at_block_17_600_000.external_update(
        update=UniswapV3PoolExternalUpdate(
            block_number=start_block + 2,
            tick=69,
        )
    )


def test_mint_and_burn_in_empty_word(fork_mainnet: AnvilFork) -> None:
    """
    Test that minting and burning an equal position inside an empty word results in no net
    liquidity in the mapping, and the removal of the position.
    """
    block_number = 20751740
    fork_mainnet.reset(block_number=block_number)
    set_web3(fork_mainnet.w3)

    lp = UniswapV3Pool(address=WBTC_WETH_V3_POOL_ADDRESS)
    assert lp.sparse_liquidity_map is True

    empty_word = -57
    lower_tick = -871860
    upper_tick = lower_tick + lp.tick_spacing

    assert lower_tick not in lp.tick_data
    assert upper_tick not in lp.tick_data

    lp._fetch_tick_data_at_word(-57)
    assert lp.tick_bitmap[empty_word] == UniswapV3BitmapAtWord()

    assert lower_tick not in lp.tick_data
    assert upper_tick not in lp.tick_data

    lp.external_update(
        # Mint
        update=UniswapV3PoolExternalUpdate(
            block_number=block_number,
            liquidity_change=(69_420, lower_tick, upper_tick),
        )
    )
    assert lower_tick in lp.tick_data
    assert upper_tick in lp.tick_data
    lp.external_update(
        # Burn
        update=UniswapV3PoolExternalUpdate(
            block_number=block_number,
            liquidity_change=(-69_420, lower_tick, upper_tick),
        )
    )
    assert lower_tick not in lp.tick_data
    assert upper_tick not in lp.tick_data


def test_auto_update(fork_mainnet: AnvilFork) -> None:
    current_block = fork_mainnet.w3.eth.block_number
    fork_mainnet.reset(block_number=current_block - 500_000)
    set_web3(fork_mainnet.w3)
    lp = UniswapV3Pool(address=WBTC_WETH_V3_POOL_ADDRESS)
    fork_mainnet.reset(block_number=current_block)
    lp.auto_update()
    lp.auto_update()  # update twice to cover the "no update" cases

    # Attempt an update in the past
    with pytest.raises(LateUpdateError):
        lp.auto_update(block_number=current_block - 10)


def test_complex_liquidity_transaction_1(fork_mainnet: AnvilFork):
    """
    Tests transaction 0xcc9b213c730978b096e2b629470c510fb68b32a1cb708ca21bbbbdce4221b00d, which
    executes a complex Burn/Swap/Mint

    State values taken from Tenderly: https://dashboard.tenderly.co/tx/mainnet/0xcc9b213c730978b096e2b629470c510fb68b32a1cb708ca21bbbbdce4221b00d/state-diff
    """

    state_block = 19619258
    lp_address = "0x3416cF6C708Da44DB2624D63ea0AAef7113527C6"

    fork_mainnet.reset(block_number=state_block)
    set_web3(fork_mainnet.w3)
    lp = UniswapV3Pool(lp_address)

    # Verify initial state
    assert lp.liquidity == 14421592867765366

    # Apply relevant updates: Burn -> Swap -> Mint
    # ref: https://dashboard.tenderly.co/tx/mainnet/0xcc9b213c730978b096e2b629470c510fb68b32a1cb708ca21bbbbdce4221b00d/logs

    lp.external_update(
        # Burn
        update=UniswapV3PoolExternalUpdate(
            block_number=state_block + 1,
            liquidity_change=(-32898296636481156, -2, 0),
        )
    )
    lp.external_update(
        # Swap
        update=UniswapV3PoolExternalUpdate(
            block_number=state_block + 1,
            liquidity=14421592867765366,
            sqrt_price_x96=79231240136335768538165178627,
            tick=0,
        )
    )
    lp.external_update(
        # Mint
        update=UniswapV3PoolExternalUpdate(
            block_number=state_block + 1,
            liquidity_change=(32881222444111623, -1, 1),
        )
    )

    assert lp.liquidity == 47302815311876989

    assert lp.tick_data[-2].liquidity_gross == 2444435478572158
    assert lp.tick_data[-2].liquidity_net == convert_unsigned_integer_to_signed(
        340282366920938463463373056991514192626
    )

    assert lp.tick_data[-1].liquidity_gross == 35737394957587036
    assert lp.tick_data[-1].liquidity_net == 32197982189243310

    assert lp.tick_data[0].liquidity_gross == 3908477120807173
    assert lp.tick_data[0].liquidity_net == convert_unsigned_integer_to_signed(
        340282366920938463463370705564595110629
    )

    assert lp.tick_data[1].liquidity_gross == 35087990576870618
    assert lp.tick_data[1].liquidity_net == convert_unsigned_integer_to_signed(
        340282366920938463463340830792807716726
    )


def test_complex_liquidity_transaction_2(fork_mainnet: AnvilFork):
    """
    Tests transaction 0xb70e8432d3ee0bcaa0f21ca7c0d0fd496096e9d72f243186dc3880d857114a3b, which
    executes a complex Burn/Swap/Mint

    State values taken from Tenderly: https://dashboard.tenderly.co/tx/mainnet/0xb70e8432d3ee0bcaa0f21ca7c0d0fd496096e9d72f243186dc3880d857114a3b/state-diff
    """

    state_block = 19624318
    lp_address = "0x3416cF6C708Da44DB2624D63ea0AAef7113527C6"

    fork_mainnet.reset(block_number=state_block)
    set_web3(fork_mainnet.w3)
    lp = UniswapV3Pool(lp_address)

    # Verify initial state
    assert lp.liquidity == 14823044070524674

    # Apply relevant updates: Burn -> Swap -> Mint
    # ref: https://dashboard.tenderly.co/tx/mainnet/0xb70e8432d3ee0bcaa0f21ca7c0d0fd496096e9d72f243186dc3880d857114a3b/logs

    lp.external_update(
        # Burn
        update=UniswapV3PoolExternalUpdate(
            block_number=state_block + 1,
            liquidity_change=(-32832176391550116, 1, 3),
        )
    )
    lp.external_update(
        # Swap
        update=UniswapV3PoolExternalUpdate(
            block_number=state_block + 1,
            liquidity=14823044070524674,
            sqrt_price_x96=79229207277353295810379307480,
            tick=0,
        )
    )
    lp.external_update(
        # Mint
        update=UniswapV3PoolExternalUpdate(
            block_number=state_block + 1,
            liquidity_change=(32906745642438587, 0, 2),
        )
    )

    assert lp.liquidity == 47729789712963261

    assert lp.tick_data[0].liquidity_gross == 36789742298460066
    assert lp.tick_data[0].liquidity_net == 29030358934123454

    assert lp.tick_data[1].liquidity_gross == 2206768132758995
    assert lp.tick_data[1].liquidity_net == convert_unsigned_integer_to_signed(
        340282366920938463463373712015251828349
    )

    assert lp.tick_data[2].liquidity_gross == 33976822553596059
    assert lp.tick_data[2].liquidity_net == convert_unsigned_integer_to_signed(
        340282366920938463463340631050012819095
    )

    assert lp.tick_data[3].liquidity_gross == 996384072015849
    assert lp.tick_data[3].liquidity_net == convert_unsigned_integer_to_signed(
        340282366920938463463373611250495718043
    )


def test_base_pancakeswap_v3(base_full_node_web3: Web3):
    set_web3(base_full_node_web3)

    # Exchange provided explicitly
    PancakeV3Pool.from_exchange(
        address=BASE_CBETH_WETH_V3_POOL_ADDRESS,
        exchange=BASE_PANCAKESWAP_V3_EXCHANGE,
    )


def test_base_pancakeswap_v3_with_builtin_exchange(base_full_node_web3: Web3):
    set_web3(base_full_node_web3)

    # Exchange looked up implicitly from degenbot deployment module
    PancakeV3Pool(
        address=BASE_CBETH_WETH_V3_POOL_ADDRESS,
    )
