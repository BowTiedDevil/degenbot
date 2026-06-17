import pickle
from fractions import Fraction
from typing import TYPE_CHECKING

import pytest
from hexbytes import HexBytes

from degenbot.anvil_fork import AnvilFork
from tests.helpers.v2_pool_factory import make_v2_pool
from degenbot.bot import Bot
from degenbot.camelot.abi import CAMELOT_POOL_ABI
from degenbot.camelot.pools import CamelotLiquidityPool
from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.erc20.erc20 import Erc20Token
from degenbot.exceptions.base import DegenbotValueError
from degenbot.exceptions.pool import (
    ExternalUpdateError,
    InvalidSwapInputAmount,
    LiquidityPoolError,
    NoPoolStateAvailable,
)
from degenbot.provider import ProviderAdapter
from degenbot.types.state_cache import StateCache
from degenbot.uniswap.abi import UNISWAP_V2_ROUTER_ABI
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v2_types import (
    UniswapV2PoolExternalUpdate,
    UniswapV2PoolSimulationResult,
    UniswapV2PoolState,
)
from tests.helpers.bot_factory import make_bot_with_provider

if TYPE_CHECKING:
    from web3.contract.contract import Contract

    from degenbot.types.aliases import BlockNumber


UNISWAP_V2_ROUTER02 = get_checksum_address("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D")
UNISWAP_V2_WBTC_WETH_POOL = get_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D940")
UNISWAP_V2_FACTORY_ADDRESS = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
UNISWAP_V2_FACTORY_POOL_INIT_HASH = (
    "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"
)

DAI_CONTRACT_ADDRESS = "0x6B175474E89094C44Da98b954EedeAC495271d0F"
WBTC_CONTRACT_ADDRESS = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
WETH_CONTRACT_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"

CAMELOT_WETH_USDC_LP_ADDRESS = get_checksum_address("0x84652bb2539513BAf36e225c930Fdd8eaa63CE27")
CAMELOT_MIM_USDC_LP_ADDRESS = get_checksum_address("0x68A0859de50B4Dfc6EFEbE981cA906D38Cdb0D1F")


def _make_bot(fork: AnvilFork) -> Bot:
    """Create a Bot with the fork's provider registered."""
    provider = ProviderAdapter.from_web3(fork.w3)
    return make_bot_with_provider(provider)


@pytest.fixture
def ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block(
    fork_mainnet_archive: AnvilFork,
) -> UniswapV2Pool:
    """Call this fixture passing an indirect parameter to "fork_mainnet_archive" to specify the
    forking block.
    """
    bot = _make_bot(fork_mainnet_archive)
    return bot.build_pool(UNISWAP_V2_WBTC_WETH_POOL)


@pytest.fixture
def ethereum_uniswap_v2_wbtc_weth_liquiditypool(fork_mainnet_full: AnvilFork) -> UniswapV2Pool:
    bot = _make_bot(fork_mainnet_full)
    return bot.build_pool(UNISWAP_V2_WBTC_WETH_POOL)


@pytest.fixture
def dai(bot_mainnet_full: Bot) -> Erc20Token:
    return bot_mainnet_full.build_erc20token(DAI_CONTRACT_ADDRESS)


@pytest.fixture
def wbtc(bot_mainnet_full: Bot) -> Erc20Token:
    return bot_mainnet_full.build_erc20token(WBTC_CONTRACT_ADDRESS)


@pytest.fixture
def weth(bot_mainnet_full: Bot) -> Erc20Token:
    return bot_mainnet_full.build_erc20token(WETH_CONTRACT_ADDRESS)


def test_pickle_uniswap_v2_pool(ethereum_uniswap_v2_wbtc_weth_liquiditypool: UniswapV2Pool):
    pickle.dumps(ethereum_uniswap_v2_wbtc_weth_liquiditypool)


def test_price_is_inverse_of_exchange_rate(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool: UniswapV2Pool,
):
    for token in [
        ethereum_uniswap_v2_wbtc_weth_liquiditypool.token0,
        ethereum_uniswap_v2_wbtc_weth_liquiditypool.token1,
    ]:
        assert (
            ethereum_uniswap_v2_wbtc_weth_liquiditypool.get_absolute_price(token)
            == ethereum_uniswap_v2_wbtc_weth_liquiditypool.get_absolute_exchange_rate(token) ** -1
        )


def test_nominal_rate_scaled_by_decimals(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool: UniswapV2Pool,
):
    for token in [
        ethereum_uniswap_v2_wbtc_weth_liquiditypool.token0,
        ethereum_uniswap_v2_wbtc_weth_liquiditypool.token1,
    ]:
        other_token = (
            ethereum_uniswap_v2_wbtc_weth_liquiditypool.token0
            if token == ethereum_uniswap_v2_wbtc_weth_liquiditypool.token1
            else ethereum_uniswap_v2_wbtc_weth_liquiditypool.token1
        )

        abs_rate = ethereum_uniswap_v2_wbtc_weth_liquiditypool.get_absolute_exchange_rate(token)
        nom_rate = ethereum_uniswap_v2_wbtc_weth_liquiditypool.get_nominal_exchange_rate(token)
        assert nom_rate == abs_rate * Fraction(
            10**other_token.decimals,
            10**token.decimals,
        )


def test_nominal_price_scaled_by_decimals(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool: UniswapV2Pool,
):
    for token in [
        ethereum_uniswap_v2_wbtc_weth_liquiditypool.token0,
        ethereum_uniswap_v2_wbtc_weth_liquiditypool.token1,
    ]:
        other_token = (
            ethereum_uniswap_v2_wbtc_weth_liquiditypool.token0
            if token == ethereum_uniswap_v2_wbtc_weth_liquiditypool.token1
            else ethereum_uniswap_v2_wbtc_weth_liquiditypool.token1
        )

        nom_price = ethereum_uniswap_v2_wbtc_weth_liquiditypool.get_nominal_price(token)
        abs_price = ethereum_uniswap_v2_wbtc_weth_liquiditypool.get_absolute_price(token)
        assert nom_price == abs_price * Fraction(
            10**token.decimals,
            10**other_token.decimals,
        )


@pytest.mark.skip
def test_create_camelot_v2_stable_pool(fork_arbitrum_full: AnvilFork):
    bot = _make_bot(fork_arbitrum_full)
    lp = bot.build_pool(CAMELOT_MIM_USDC_LP_ADDRESS)
    # Camelot pools are wrapped as CamelotLiquidityPool by the manager
    # For direct construction, use I/O-free constructor

    # Build tokens I/O-free first, then construct the pool
    # The pool reference comes from the bot which handles the construction
    assert isinstance(lp, (CamelotLiquidityPool, UniswapV2Pool))

    token_in = lp.token0  # MIM token
    amount_in = 1000 * 10**token_in.decimals  # nominal value of $1000

    # Test that the swap output from the pool contract matches the off-chain calculation
    w3_contract = fork_arbitrum_full.w3.eth.contract(
        address=CAMELOT_MIM_USDC_LP_ADDRESS, abi=CAMELOT_POOL_ABI
    )

    contract_amount = w3_contract.functions.getAmountOut(
        amountIn=amount_in, tokenIn=token_in.address
    ).call()
    assert contract_amount == lp.calculate_tokens_out_from_tokens_in(
        token_in=token_in,
        token_in_quantity=amount_in,
    )


def test_create_camelot_v2_pool(fork_arbitrum_full: AnvilFork):
    bot = _make_bot(fork_arbitrum_full)
    lp = bot.build_pool(CAMELOT_WETH_USDC_LP_ADDRESS)

    token_in = lp.token1
    amount_in = 1000 * 10**token_in.decimals  # nominal value of $1000

    w3_contract: Contract = fork_arbitrum_full.w3.eth.contract(
        address=CAMELOT_WETH_USDC_LP_ADDRESS, abi=CAMELOT_POOL_ABI
    )
    assert w3_contract.functions.getAmountOut(
        amountIn=amount_in, tokenIn=token_in.address
    ).call() == lp.calculate_tokens_out_from_tokens_in(
        token_in=token_in,
        token_in_quantity=amount_in,
    )


def test_dunder_methods(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool: UniswapV2Pool,
):
    str(ethereum_uniswap_v2_wbtc_weth_liquiditypool)
    hash(ethereum_uniswap_v2_wbtc_weth_liquiditypool)

    with pytest.raises(AssertionError):
        assert ethereum_uniswap_v2_wbtc_weth_liquiditypool == 69

    with pytest.raises(TypeError):
        assert ethereum_uniswap_v2_wbtc_weth_liquiditypool < 69

    with pytest.raises(TypeError):
        assert ethereum_uniswap_v2_wbtc_weth_liquiditypool > 69

    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool
        == ethereum_uniswap_v2_wbtc_weth_liquiditypool.address
    )
    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool == bytes.fromhex(
        ethereum_uniswap_v2_wbtc_weth_liquiditypool.address[2:]
    )
    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool == HexBytes(
        ethereum_uniswap_v2_wbtc_weth_liquiditypool.address
    )
    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool > ZERO_ADDRESS
    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool > HexBytes(ZERO_ADDRESS)
    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool > bytes.fromhex(ZERO_ADDRESS[2:])

    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool > ethereum_uniswap_v2_wbtc_weth_liquiditypool
    ) is False
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool
        > HexBytes(ethereum_uniswap_v2_wbtc_weth_liquiditypool.address)
    ) is False
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool
        > ethereum_uniswap_v2_wbtc_weth_liquiditypool.address
    ) is False

    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool < ethereum_uniswap_v2_wbtc_weth_liquiditypool
    ) is False
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool
        < HexBytes(ethereum_uniswap_v2_wbtc_weth_liquiditypool.address)
    ) is False
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool
        < bytes.fromhex(ethereum_uniswap_v2_wbtc_weth_liquiditypool.address[2:])
    ) is False
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool
        < ethereum_uniswap_v2_wbtc_weth_liquiditypool.address
    ) is False


def test_pickle_pool(ethereum_uniswap_v2_wbtc_weth_liquiditypool: UniswapV2Pool):
    pickle.dumps(ethereum_uniswap_v2_wbtc_weth_liquiditypool)


@pytest.mark.parametrize(
    "fork_mainnet_archive",
    [17_600_000],
    indirect=True,
)
def test_calculate_tokens_out_from_ratio_out(fork_mainnet_archive: AnvilFork):
    bot = _make_bot(fork_mainnet_archive)

    router_contract = fork_mainnet_archive.w3.eth.contract(
        address=get_checksum_address(UNISWAP_V2_ROUTER02),
        abi=UNISWAP_V2_ROUTER_ABI,
    )

    lp = bot.build_pool(UNISWAP_V2_WBTC_WETH_POOL)

    for wbtc_amount_in in [
        int(0.1 * 10**8),
        1 * 10**8,
        10 * 10**8,
    ]:
        token_in = lp.token0  # WBTC
        token_out = lp.token1  # WETH

        weth_amount_out = router_contract.functions.getAmountsOut(
            wbtc_amount_in,
            [token_in.address, token_out.address],
        ).call()[-1]

        ratio = Fraction(weth_amount_out, wbtc_amount_in)

        calculated_input = lp.calculate_tokens_in_from_ratio_out(
            token_in=token_in,
            ratio_absolute=ratio,
        )
        assert pytest.approx(calculated_input / wbtc_amount_in, rel=1e-3) == 1

    for weth_amount_in in [1 * 10**18, 10 * 10**18, 100 * 10**18]:
        token_in = lp.token1  # WETH
        token_out = lp.token0  # WBTC

        wbtc_amount_out = router_contract.functions.getAmountsOut(
            weth_amount_in,
            [token_in.address, token_out.address],
        ).call()[-1]

        ratio = Fraction(wbtc_amount_out, weth_amount_in)

        calculated_input = lp.calculate_tokens_in_from_ratio_out(
            token_in=token_in,
            ratio_absolute=ratio,
        )
        assert pytest.approx(calculated_input / weth_amount_in, rel=1e-3) == 1


@pytest.mark.parametrize(
    "fork_mainnet_archive",
    [17_600_000],  # EDIT ME
    indirect=True,
)
def test_calculate_tokens_out_from_tokens_in(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block: UniswapV2Pool,
    dai: Erc20Token,
):
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.calculate_tokens_out_from_tokens_in(
            token_in=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.token0,
            token_in_quantity=8000000000,
        )
        == 847228560678214929944
    )
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.calculate_tokens_out_from_tokens_in(
            token_in=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.token1,
            token_in_quantity=1200000000000000000000,
        )
        == 5154005339
    )

    with pytest.raises(DegenbotValueError, match="Could not identify token_in"):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.calculate_tokens_out_from_tokens_in(
            token_in=dai,
            token_in_quantity=1 * 10**18,
        )


def test_calculate_tokens_out_from_tokens_in_with_override(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool: UniswapV2Pool,
):
    # Historical reserves taken from block 17_650_000
    old_reserves0 = 16027096956
    old_reserves1 = 2602647332090181827846

    pool_state_override = UniswapV2PoolState(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        reserves_token0=old_reserves0,
        reserves_token1=old_reserves1,
        block=None,
    )

    # Overriding the state of the pool to the historical block should should return the values
    # from that historical block
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool.calculate_tokens_out_from_tokens_in(
            token_in=ethereum_uniswap_v2_wbtc_weth_liquiditypool.token0,
            token_in_quantity=8000000000,
            override_state=pool_state_override,
        )
        == 864834865217768537471
    )

    # Historical state calculation should differ from the current state calculation
    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool.calculate_tokens_out_from_tokens_in(
        token_in=ethereum_uniswap_v2_wbtc_weth_liquiditypool.token0,
        token_in_quantity=8000000000,
        override_state=pool_state_override,
    ) != ethereum_uniswap_v2_wbtc_weth_liquiditypool.calculate_tokens_out_from_tokens_in(
        token_in=ethereum_uniswap_v2_wbtc_weth_liquiditypool.token0,
        token_in_quantity=8000000000,
    )


@pytest.mark.parametrize(
    "fork_mainnet_archive",
    [17_600_000],  # EDIT ME
    indirect=True,
)
def test_calculate_tokens_in_from_tokens_out(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block: UniswapV2Pool,
):
    """Reserve values for this test are taken at block height 17,600,000
    """
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.calculate_tokens_in_from_tokens_out(
            token_out_quantity=8000000000,
            token_out=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.token0,
        )
        == 2506650866141614297072
    )

    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.calculate_tokens_in_from_tokens_out(
            token_out_quantity=1200000000000000000000,
            token_out=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.token1,
        )
        == 14245938804
    )


@pytest.mark.parametrize(
    "fork_mainnet_archive",
    [17_600_000],  # EDIT ME
    indirect=True,
)
def test_calculate_tokens_in_from_tokens_out_with_override(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block: UniswapV2Pool, dai: Erc20Token
):
    # Overridden reserve values for this test are taken at block height 17,650,000
    # token0 reserves: 16027096956
    # token1 reserves: 2602647332090181827846

    pool_state_override = UniswapV2PoolState(
        address=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.address,
        reserves_token0=16027096956,
        reserves_token1=2602647332090181827846,
        block=17_650_000,
    )

    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.calculate_tokens_in_from_tokens_out(
            token_out=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.token1,
            token_out_quantity=1200000000000000000000,
            override_state=pool_state_override,
        )
        == 13752842264
    )

    with pytest.raises(DegenbotValueError, match="Could not identify token_out"):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.calculate_tokens_in_from_tokens_out(
            token_out=dai,
            token_out_quantity=1200000000000000000000,
            override_state=pool_state_override,
        )


@pytest.mark.parametrize(
    "fork_mainnet_archive",
    [17_600_000],  # EDIT ME
    indirect=True,
)
def test_comparisons(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block: UniswapV2Pool,
    fork_mainnet_archive: AnvilFork,
):
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block == UNISWAP_V2_WBTC_WETH_POOL
    )
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block
        == UNISWAP_V2_WBTC_WETH_POOL.lower()
    )

    # Construct another pool with I/O-free constructor using the same tokens
    other_lp = make_v2_pool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        chain_id=1,
        init_hash=UNISWAP_V2_FACTORY_POOL_INIT_HASH,
        token0=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.token0,
        token1=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.token1,
        factory=UNISWAP_V2_FACTORY_ADDRESS,
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.reserves_token0,
        reserves_token1=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.reserves_token1,
    )

    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block == other_lp
    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block is not other_lp

    # sets depend on __hash__ dunder method
    _ = {ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block, other_lp}


@pytest.mark.parametrize(
    "fork_mainnet_archive",
    [17_600_000],  # EDIT ME
    indirect=True,
)
def test_reorg(ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block: UniswapV2Pool):
    # The reorg journal is now Rust-owned (ADR-005 slice 4) with a default
    # depth of 32 blocks — ample for the 10 dummy updates below, so the former
    # custom StateCache(max_depth=100) swap is no longer needed.
    starting_state = ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.state
    starting_block = ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.update_block

    first_update_block = (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.update_block + 1
    )
    last_update_block = (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.update_block + 10
    )

    starting_token0_reserves = starting_state.reserves_token0
    starting_token1_reserves = starting_state.reserves_token1

    expected_block_states: dict[int, UniswapV2PoolState] = {starting_block: starting_state}

    # Provide some dummy updates, then simulate a reorg back to the starting state
    for block_number in range(first_update_block, last_update_block + 1):
        assert block_number not in expected_block_states
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.external_update(
            update=UniswapV2PoolExternalUpdate(
                block_number=block_number,
                reserves_token0=starting_token0_reserves
                + 10_000 * (1 + block_number - first_update_block),
                reserves_token1=starting_token1_reserves
                + 10_000 * (1 + block_number - first_update_block),
            )
        )
        assert (
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.update_block
            == block_number
        )
        expected_block_states[block_number] = (
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.state
        )

    last_block_state = ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.state

    # Cannot restore to a pool state before the first
    with pytest.raises(NoPoolStateAvailable):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.restore_state_before_block(
            0
        )

    # Last state is before this block, so this will succeed but have no effect on the current state
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.restore_state_before_block(
        last_update_block + 1
    )
    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.state == last_block_state

    # Unwind the updates and compare to the stored states at previous blocks
    for block_number in range(last_update_block, first_update_block - 1, -1):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.restore_state_before_block(
            block_number
        )
        assert (
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.state
            == expected_block_states[block_number - 1]
        )

    # Verify the pool has been returned to the starting state
    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.state == starting_state


@pytest.mark.parametrize(
    "fork_mainnet_archive",
    [17_600_000],  # EDIT ME
    indirect=True,
)
def test_discard_before_finalized(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block: UniswapV2Pool,
):
    starting_state = ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.state
    starting_block = ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.update_block

    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block._py_pool is not None

    first_update_block = (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.update_block + 1
    )
    last_update_block = (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.update_block + 10
    )

    starting_token0_reserves = starting_state.reserves_token0
    starting_token1_reserves = starting_state.reserves_token1

    expected_block_states: dict[BlockNumber, UniswapV2PoolState] = {starting_block: starting_state}

    # Provide some dummy updates, then simulate a reorg back to the starting state
    for block_number in range(first_update_block, last_update_block + 1):
        assert block_number not in expected_block_states

        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.external_update(
            update=UniswapV2PoolExternalUpdate(
                block_number=block_number,
                reserves_token0=starting_token0_reserves
                + 10_000 * (1 + block_number - first_update_block),
                reserves_token1=starting_token1_reserves
                + 10_000 * (1 + block_number - first_update_block),
            )
        )
        assert (
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.update_block
            == block_number
        )
        expected_block_states[block_number] = (
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.state
        )

    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.discard_states_before_block(
        last_update_block
    )
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.update_block
        == last_update_block
    )


@pytest.mark.parametrize(
    "fork_mainnet_archive",
    [17_600_000],  # EDIT ME
    indirect=True,
)
def test_discard_earlier_than_created(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block: UniswapV2Pool,
) -> None:
    lp = ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block

    # Discarding before the registration block is a no-op (the journal is unchanged).
    journal_len_before = lp._py_pool.journal_len()
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.discard_states_before_block(
        lp.update_block - 1
    )
    assert lp._py_pool.journal_len() == journal_len_before


@pytest.mark.parametrize(
    "fork_mainnet_archive",
    [17_600_000],
    indirect=True,
)
def test_discard_after_last_update(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block: UniswapV2Pool,
) -> None:
    lp = ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block

    with pytest.raises(
        NoPoolStateAvailable, match=f"No pool state known prior to block {lp.update_block + 1}"
    ):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.discard_states_before_block(
            lp.update_block + 1
        )


@pytest.mark.parametrize(
    "fork_mainnet_archive",
    [17_600_000],
    indirect=True,
)
def test_simulations(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block: UniswapV2Pool,
):
    sim_result = UniswapV2PoolSimulationResult(
        amount0_delta=8000000000,
        amount1_delta=-847228560678214929944,
        initial_state=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.state,
        final_state=UniswapV2PoolState(
            address=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.address,
            block=None,
            reserves_token0=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.reserves_token0
            + 8000000000,
            reserves_token1=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.reserves_token1
            - 847228560678214929944,
        ),
    )

    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.simulate_exact_input_swap(
            token_in=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.token0,
            token_in_quantity=8000000000,
        )
        == sim_result
    )

    sim_result = UniswapV2PoolSimulationResult(
        amount0_delta=-5154005339,
        amount1_delta=1200000000000000000000,
        initial_state=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.state,
        final_state=UniswapV2PoolState(
            address=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.address,
            block=None,
            reserves_token0=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.reserves_token0
            - 5154005339,
            reserves_token1=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.reserves_token1
            + 1200000000000000000000,
        ),
    )

    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.simulate_exact_input_swap(
            token_in=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.token1,
            token_in_quantity=1200000000000000000000,
        )
        == sim_result
    )

    added_liquidity = 10_000_000
    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.simulate_add_liquidity(
        added_reserves_token0=added_liquidity, added_reserves_token1=added_liquidity
    ) == UniswapV2PoolSimulationResult(
        amount0_delta=added_liquidity,
        amount1_delta=added_liquidity,
        initial_state=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.state,
        final_state=UniswapV2PoolState(
            address=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.address,
            block=None,
            reserves_token0=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.reserves_token0
            + added_liquidity,
            reserves_token1=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.reserves_token1
            + added_liquidity,
        ),
    )

    removed_liquidity = 10_000_000
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.simulate_remove_liquidity(
            removed_reserves_token0=removed_liquidity, removed_reserves_token1=removed_liquidity
        )
        == UniswapV2PoolSimulationResult(
            amount0_delta=-removed_liquidity,
            amount1_delta=-removed_liquidity,
            initial_state=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.state,
            final_state=UniswapV2PoolState(
                address=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.address,
                block=None,
                reserves_token0=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.reserves_token0
                - removed_liquidity,
                reserves_token1=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.reserves_token1
                - removed_liquidity,
            ),
        )
    )


@pytest.mark.parametrize(
    "fork_mainnet_archive",
    [17_600_000],
    indirect=True,
)
def test_simulation_input_validation(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block: UniswapV2Pool,
    dai,
):
    lp = ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block
    with pytest.raises(DegenbotValueError, match=r"token_in is unknown."):
        lp.simulate_exact_input_swap(token_in=dai, token_in_quantity=1_000)
    with pytest.raises(DegenbotValueError, match=r"token_out is unknown."):
        lp.simulate_exact_output_swap(token_out=dai, token_out_quantity=1_000)


@pytest.mark.parametrize(
    "fork_mainnet_archive",
    [17_600_000],
    indirect=True,
)
def test_simulations_with_override(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block: UniswapV2Pool,
):
    pool_state_override = UniswapV2PoolState(
        address=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.address,
        block=None,
        reserves_token0=16027096956,
        reserves_token1=2602647332090181827846,
    )

    expected_sim_result = UniswapV2PoolSimulationResult(
        amount0_delta=8000000000,
        amount1_delta=-864834865217768537471,
        initial_state=pool_state_override,
        final_state=UniswapV2PoolState(
            address=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.address,
            block=17_600_000,
            # ADR-005 slice 4: simulate_with_override now builds final_state
            # from the OVERRIDE reserves (consistent with the delta computed
            # from them). Pre-slice-4 it mixed override reserves for the delta
            # with LIVE reserves for the final_state base — a latent bug.
            reserves_token0=pool_state_override.reserves_token0
            + 8000000000,
            reserves_token1=pool_state_override.reserves_token1
            - 864834865217768537471,
        ),
    )

    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.simulate_exact_input_swap(
            token_in=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.token0,
            token_in_quantity=8000000000,
            override_state=pool_state_override,
        )
        == expected_sim_result
    )

    expected_sim_result = UniswapV2PoolSimulationResult(
        amount0_delta=13752842264,
        amount1_delta=-1200000000000000000000,
        initial_state=pool_state_override,
        final_state=UniswapV2PoolState(
            address=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.address,
            block=17_600_000,
            # ADR-005 slice 4: final_state built from OVERRIDE reserves
            # (consistent with the delta). Pre-slice-4 used LIVE reserves.
            reserves_token0=pool_state_override.reserves_token0
            + 13752842264,
            reserves_token1=pool_state_override.reserves_token1
            - 1200000000000000000000,
        ),
    )

    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.simulate_exact_output_swap(
            token_out=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.token1,
            token_out_quantity=1200000000000000000000,
            override_state=pool_state_override,
        )
        == expected_sim_result
    )


@pytest.mark.parametrize(
    "fork_mainnet_archive",
    [17_600_000],
    indirect=True,
)
def test_swap_for_all(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block: UniswapV2Pool,
):
    # The last token in a pool can never be swapped for
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.calculate_tokens_out_from_tokens_in(
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.token1,
            2**256 - 1,
        )
        == ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.reserves_token0 - 1
    )
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.calculate_tokens_out_from_tokens_in(
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.token0,
            2**256 - 1,
        )
        == ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.reserves_token1 - 1
    )

    with pytest.raises(LiquidityPoolError):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.calculate_tokens_in_from_tokens_out(
            token_out=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.token0,
            token_out_quantity=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.reserves_token0,
        )

    with pytest.raises(LiquidityPoolError):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.calculate_tokens_in_from_tokens_out(
            token_out=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.token1,
            token_out_quantity=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.reserves_token1,
        )


@pytest.mark.parametrize(
    "fork_mainnet_archive",
    [17_600_000],
    indirect=True,
)
def test_zero_swaps(ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block: UniswapV2Pool):
    with pytest.raises(InvalidSwapInputAmount):
        assert (
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.calculate_tokens_out_from_tokens_in(
                ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.token0,
                0,
            )
            == 0
        )

    with pytest.raises(InvalidSwapInputAmount):
        assert (
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.calculate_tokens_out_from_tokens_in(
                ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.token1,
                0,
            )
            == 0
        )


@pytest.mark.parametrize(
    "fork_mainnet_archive",
    [17_600_000],
    indirect=True,
)
def test_late_update(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block: UniswapV2Pool,
):
    # Provide some semi-random updates
    for block_number in range(
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.update_block,
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.update_block + 5,
    ):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.external_update(
            update=UniswapV2PoolExternalUpdate(
                block_number=block_number,
                reserves_token0=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.reserves_token0
                + block_number * 10,
                reserves_token1=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.reserves_token1
                - block_number * 10,
            )
        )

    # Send a late update
    with pytest.raises(ExternalUpdateError):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.external_update(
            update=UniswapV2PoolExternalUpdate(
                block_number=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.update_block
                - 1,
                reserves_token0=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.reserves_token0
                + 1,
                reserves_token1=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.reserves_token1
                - 1,
            )
        )

    # Send a duplicate update
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.external_update(
        update=UniswapV2PoolExternalUpdate(
            block_number=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.update_block
            + 1,
            reserves_token0=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.reserves_token0,
            reserves_token1=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_historical_block.reserves_token1,
        )
    )
