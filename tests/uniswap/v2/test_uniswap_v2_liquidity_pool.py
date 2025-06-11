import pickle
from fractions import Fraction
from typing import TYPE_CHECKING, cast

import pytest
from eth_typing import BlockNumber
from hexbytes import HexBytes

import degenbot.uniswap.deployments
from degenbot import AnvilFork, CamelotLiquidityPool, Erc20Token, UniswapV2Pool, set_web3
from degenbot.cache import get_checksum_address
from degenbot.camelot.abi import CAMELOT_POOL_ABI
from degenbot.constants import ZERO_ADDRESS
from degenbot.exceptions import (
    AddressMismatch,
    DegenbotValueError,
    ExternalUpdateError,
    InvalidSwapInputAmount,
    LateUpdateError,
    LiquidityPoolError,
    NoPoolStateAvailable,
)
from degenbot.functions import encode_function_calldata, raw_call
from degenbot.pancakeswap.pools import PancakeV2Pool
from degenbot.registry.all_pools import pool_registry
from degenbot.uniswap.abi import UNISWAP_V2_ROUTER_ABI
from degenbot.uniswap.deployments import FACTORY_DEPLOYMENTS
from degenbot.uniswap.types import (
    UniswapV2PoolExternalUpdate,
    UniswapV2PoolSimulationResult,
    UniswapV2PoolState,
)
from degenbot.uniswap.v2_liquidity_pool import UnregisteredLiquidityPool

if TYPE_CHECKING:
    from web3.contract.contract import Contract

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


@pytest.fixture
def ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000(
    fork_mainnet_archive: AnvilFork,
) -> UniswapV2Pool:
    fork_mainnet_archive.reset(block_number=17_600_000)
    set_web3(fork_mainnet_archive.w3)
    return UniswapV2Pool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        init_hash=UNISWAP_V2_FACTORY_POOL_INIT_HASH,
        state_cache_depth=512,  # set high to ensure cache can hold all items for reorg tests
    )


@pytest.fixture
def ethereum_uniswap_v2_wbtc_weth_liquiditypool_reserves_at_block_17_650_000(
    fork_mainnet_archive: AnvilFork,
) -> tuple[int, int]:
    fork_mainnet_archive.reset(block_number=17_650_000)
    set_web3(fork_mainnet_archive.w3)
    reserves_token0, reserves_token1, *_ = raw_call(
        w3=fork_mainnet_archive.w3,
        address=UNISWAP_V2_WBTC_WETH_POOL,
        calldata=encode_function_calldata(
            function_prototype="getReserves()",
            function_arguments=None,
        ),
        return_types=["uint256", "uint256"],
    )
    return reserves_token0, reserves_token1


@pytest.fixture
def ethereum_uniswap_v2_wbtc_weth_liquiditypool(fork_mainnet_full: AnvilFork) -> UniswapV2Pool:
    set_web3(fork_mainnet_full.w3)
    return UniswapV2Pool(address=UNISWAP_V2_WBTC_WETH_POOL)


@pytest.fixture
def dai(fork_mainnet_full: AnvilFork) -> Erc20Token:
    set_web3(fork_mainnet_full.w3)
    return Erc20Token(DAI_CONTRACT_ADDRESS)


@pytest.fixture
def wbtc(fork_mainnet_full: AnvilFork) -> Erc20Token:
    set_web3(fork_mainnet_full.w3)
    return Erc20Token(WBTC_CONTRACT_ADDRESS)


@pytest.fixture
def weth(fork_mainnet_full: AnvilFork) -> Erc20Token:
    set_web3(fork_mainnet_full.w3)
    return Erc20Token(WETH_CONTRACT_ADDRESS)


def test_create_pool(fork_mainnet_full: AnvilFork):
    set_web3(fork_mainnet_full.w3)

    UniswapV2Pool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        init_hash=UNISWAP_V2_FACTORY_POOL_INIT_HASH,
    )
    pool_registry.remove(
        pool_address=UNISWAP_V2_WBTC_WETH_POOL,
        chain_id=fork_mainnet_full.w3.eth.chain_id,
    )

    # Omitting init hash
    UniswapV2Pool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        # init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
    )


def test_create_pancake_v2_pool(fork_base_full: AnvilFork):
    set_web3(fork_base_full.w3)
    PancakeV2Pool("0x92363F9817f92a7ae0592A4cb29959A88d885cc8")


def test_from_exchange_deployment(fork_mainnet_full: AnvilFork):
    set_web3(fork_mainnet_full.w3)

    # Delete the preset deployment for this factory so the test uses the provided override instead
    # of preferring the known valid deployment data
    factory_deployment = FACTORY_DEPLOYMENTS[fork_mainnet_full.w3.eth.chain_id][
        UNISWAP_V2_FACTORY_ADDRESS
    ]
    del FACTORY_DEPLOYMENTS[fork_mainnet_full.w3.eth.chain_id][UNISWAP_V2_FACTORY_ADDRESS]

    UniswapV2Pool.from_exchange(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        exchange=degenbot.uniswap.deployments.EthereumMainnetUniswapV2,
    )

    # Restore the preset deployment
    FACTORY_DEPLOYMENTS[fork_mainnet_full.w3.eth.chain_id][UNISWAP_V2_FACTORY_ADDRESS] = (
        factory_deployment
    )


def test_price_is_inverse_of_exchange_rate(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool: UniswapV2Pool,
):
    for token in [
        ethereum_uniswap_v2_wbtc_weth_liquiditypool.token0,
        ethereum_uniswap_v2_wbtc_weth_liquiditypool.token1,
    ]:
        assert ethereum_uniswap_v2_wbtc_weth_liquiditypool.get_absolute_price(
            token
        ) == 1 / ethereum_uniswap_v2_wbtc_weth_liquiditypool.get_absolute_exchange_rate(token)


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


def test_create_camelot_v2_stable_pool(fork_arbitrum_full: AnvilFork):
    set_web3(fork_arbitrum_full.w3)

    lp = CamelotLiquidityPool(address=CAMELOT_MIM_USDC_LP_ADDRESS)
    assert lp.stable_swap is True

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
    set_web3(fork_arbitrum_full.w3)

    lp = CamelotLiquidityPool(address=CAMELOT_WETH_USDC_LP_ADDRESS)
    assert lp.stable_swap is False

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


def test_pickle_camelot_v2_pool(fork_arbitrum_full: AnvilFork):
    set_web3(fork_arbitrum_full.w3)
    lp = CamelotLiquidityPool(address=CAMELOT_WETH_USDC_LP_ADDRESS)
    pickle.dumps(lp)


def test_create_nonstandard_pools(
    fork_mainnet_full: AnvilFork,
    weth: Erc20Token,
    wbtc: Erc20Token,
):
    set_web3(fork_mainnet_full.w3)

    lp = UnregisteredLiquidityPool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        tokens=[weth, wbtc],
    )
    assert lp.address == UNISWAP_V2_WBTC_WETH_POOL
    assert lp.tokens == (wbtc, weth)
    assert lp.fee_token0 == lp.fee_token1 == Fraction(3, 1000)

    lp = UnregisteredLiquidityPool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        tokens=[weth, wbtc],
        fee=[Fraction(2, 1000), Fraction(6, 1000)],
    )
    assert lp.fee_token0 == Fraction(2, 1000)
    assert lp.fee_token1 == Fraction(6, 1000)

    # Delete the preset deployment for this factory so the test uses the provided override instead
    # of preferring the known valid deployment data
    factory_deployment = FACTORY_DEPLOYMENTS[fork_mainnet_full.w3.eth.chain_id][
        UNISWAP_V2_FACTORY_ADDRESS
    ]
    del FACTORY_DEPLOYMENTS[fork_mainnet_full.w3.eth.chain_id][UNISWAP_V2_FACTORY_ADDRESS]

    # Create pool with a malformed init hash
    bad_init_hash = UNISWAP_V2_FACTORY_POOL_INIT_HASH.replace("a", "b")
    with pytest.raises(AddressMismatch, match="Pool address verification failed"):
        UniswapV2Pool(
            address=UNISWAP_V2_WBTC_WETH_POOL,
            init_hash=bad_init_hash,
        )

    # Restore the preset deployment
    FACTORY_DEPLOYMENTS[fork_mainnet_full.w3.eth.chain_id][UNISWAP_V2_FACTORY_ADDRESS] = (
        factory_deployment
    )

    # Create with non-standard fee
    _lp = UniswapV2Pool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        init_hash=UNISWAP_V2_FACTORY_POOL_INIT_HASH,
        fee=Fraction(2, 1000),
    )
    assert _lp.fee_token0 == Fraction(2, 1000)
    assert _lp.fee_token1 == Fraction(2, 1000)
    pool_registry.remove(
        pool_address=_lp.address,
        chain_id=fork_mainnet_full.w3.eth.chain_id,
    )

    # Create split-fee pool of differing values
    _lp = UniswapV2Pool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        fee=(Fraction(3, 1000), Fraction(5, 1000)),
    )
    assert _lp.fee_token0 == Fraction(3, 1000)
    assert _lp.fee_token1 == Fraction(5, 1000)
    pool_registry.remove(
        pool_address=_lp.address,
        chain_id=fork_mainnet_full.w3.eth.chain_id,
    )

    # Create split-fee pool of equal values
    _lp = UniswapV2Pool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        fee=(Fraction(6, 1000), Fraction(6, 1000)),
    )
    assert _lp.fee_token0 == Fraction(6, 1000)
    assert _lp.fee_token1 == Fraction(6, 1000)


def test_dunder_methods(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool: UniswapV2Pool,
):
    ethereum_uniswap_v2_wbtc_weth_liquiditypool.__str__()
    ethereum_uniswap_v2_wbtc_weth_liquiditypool.__hash__()

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
        ethereum_uniswap_v2_wbtc_weth_liquiditypool > ethereum_uniswap_v2_wbtc_weth_liquiditypool  # noqa: PLR0124
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
        ethereum_uniswap_v2_wbtc_weth_liquiditypool < ethereum_uniswap_v2_wbtc_weth_liquiditypool  # noqa: PLR0124
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


def test_calculate_tokens_out_from_ratio_out(fork_mainnet_archive: AnvilFork):
    block_number = 17_600_000
    fork_mainnet_archive.reset(block_number=block_number)
    set_web3(fork_mainnet_archive.w3)

    router_contract = fork_mainnet_archive.w3.eth.contract(
        address=get_checksum_address(UNISWAP_V2_ROUTER02),
        abi=UNISWAP_V2_ROUTER_ABI,
    )

    lp = UniswapV2Pool(UNISWAP_V2_WBTC_WETH_POOL)

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


def test_calculate_tokens_out_from_tokens_in(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: UniswapV2Pool,
    dai: Erc20Token,
):
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.calculate_tokens_out_from_tokens_in(
            token_in=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.token0,
            token_in_quantity=8000000000,
        )
        == 847228560678214929944
    )
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.calculate_tokens_out_from_tokens_in(
            token_in=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.token1,
            token_in_quantity=1200000000000000000000,
        )
        == 5154005339
    )

    with pytest.raises(DegenbotValueError, match="Could not identify token_in"):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.calculate_tokens_out_from_tokens_in(
            token_in=dai,
            token_in_quantity=1 * 10**18,
        )


def test_calculate_tokens_out_from_tokens_in_with_override(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool: UniswapV2Pool,
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_reserves_at_block_17_650_000: tuple[int, int],
):
    reserves0, reserves1 = ethereum_uniswap_v2_wbtc_weth_liquiditypool_reserves_at_block_17_650_000
    pool_state_override = UniswapV2PoolState(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        reserves_token0=reserves0,
        reserves_token1=reserves1,
        block=None,
    )
    assert pool_state_override.reserves_token0 == 16027096956
    assert pool_state_override.reserves_token1 == 2602647332090181827846

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


def test_calculate_tokens_in_from_tokens_out(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: UniswapV2Pool,
):
    """
    Reserve values for this test are taken at block height 17,600,000
    """

    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.calculate_tokens_in_from_tokens_out(
            token_out_quantity=8000000000,
            token_out=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.token0,
        )
        == 2506650866141614297072
    )

    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.calculate_tokens_in_from_tokens_out(
            token_out_quantity=1200000000000000000000,
            token_out=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.token1,
        )
        == 14245938804
    )


def test_calculate_tokens_in_from_tokens_out_with_override(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: UniswapV2Pool, dai: Erc20Token
):
    # Overridden reserve values for this test are taken at block height 17,650,000
    # token0 reserves: 16027096956
    # token1 reserves: 2602647332090181827846

    pool_state_override = UniswapV2PoolState(
        address=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.address,
        reserves_token0=16027096956,
        reserves_token1=2602647332090181827846,
        block=cast("BlockNumber", 17_650_000),
    )

    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.calculate_tokens_in_from_tokens_out(
            token_out=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.token1,
            token_out_quantity=1200000000000000000000,
            override_state=pool_state_override,
        )
        == 13752842264
    )

    with pytest.raises(DegenbotValueError, match="Could not identify token_out"):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.calculate_tokens_in_from_tokens_out(
            token_out=dai,
            token_out_quantity=1200000000000000000000,
            override_state=pool_state_override,
        )


def test_comparisons(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: UniswapV2Pool,
):
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000 == UNISWAP_V2_WBTC_WETH_POOL
    )
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000
        == UNISWAP_V2_WBTC_WETH_POOL.lower()
    )

    pool_registry.remove(
        pool_address=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.address,
        chain_id=1,
    )

    other_lp = UniswapV2Pool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        init_hash=UNISWAP_V2_FACTORY_POOL_INIT_HASH,
        fee=Fraction(3, 1000),
    )

    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000 == other_lp
    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000 is not other_lp

    # sets depend on __hash__ dunder method
    set([ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000, other_lp])


def test_reorg(ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: UniswapV2Pool):
    starting_state = ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.state
    starting_block = ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.update_block

    first_update_block = (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.update_block + 1
    )
    last_update_block = (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.update_block + 10
    )

    starting_token0_reserves = starting_state.reserves_token0
    starting_token1_reserves = starting_state.reserves_token1

    expected_block_states: dict[int, UniswapV2PoolState] = {starting_block: starting_state}

    # Provide some dummy updates, then simulate a reorg back to the starting state
    for block_number in range(first_update_block, last_update_block + 1):
        block_number = cast("BlockNumber", block_number)
        assert block_number not in expected_block_states
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.external_update(
            update=UniswapV2PoolExternalUpdate(
                block_number=block_number,
                reserves_token0=starting_token0_reserves
                + 10_000 * (1 + block_number - first_update_block),
                reserves_token1=starting_token1_reserves
                + 10_000 * (1 + block_number - first_update_block),
            )
        )
        assert (
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000._state_cache is not None
        )
        assert (
            block_number
            in ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000._state_cache
        )
        expected_block_states[block_number] = (
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.state
        )

    last_block_state = ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.state

    # Cannot restore to a pool state before the first
    with pytest.raises(NoPoolStateAvailable):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.restore_state_before_block(
            0
        )

    # Last state is before this block, so this will succeed but have no effect on the current state
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.restore_state_before_block(
        last_update_block + 1
    )
    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.state == last_block_state

    # Unwind the updates and compare to the stored states at previous blocks
    for block_number in range(last_update_block, first_update_block - 1, -1):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.restore_state_before_block(
            block_number
        )
        assert (
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.state
            == expected_block_states[block_number - 1]
        )

    # Verify the pool has been returned to the starting state
    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.state == starting_state


def test_discard_before_finalized(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: UniswapV2Pool,
):
    starting_state = ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.state
    starting_block = ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.update_block

    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000._state_cache is not None

    first_update_block = (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.update_block + 1
    )
    last_update_block = (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.update_block + 10
    )

    starting_token0_reserves = starting_state.reserves_token0
    starting_token1_reserves = starting_state.reserves_token1

    expected_block_states: dict[BlockNumber, UniswapV2PoolState] = {starting_block: starting_state}

    # Provide some dummy updates, then simulate a reorg back to the starting state
    for block_number in range(first_update_block, last_update_block + 1):
        block_number = cast("BlockNumber", block_number)
        assert block_number not in expected_block_states

        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.external_update(
            update=UniswapV2PoolExternalUpdate(
                block_number=block_number,
                reserves_token0=starting_token0_reserves
                + 10_000 * (1 + block_number - first_update_block),
                reserves_token1=starting_token1_reserves
                + 10_000 * (1 + block_number - first_update_block),
            )
        )
        assert (
            block_number
            in ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000._state_cache
        )
        expected_block_states[block_number] = (
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.state
        )

    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.discard_states_before_block(
        last_update_block
    )
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000._state_cache.keys()
        == set([last_update_block])
    )


def test_discard_earlier_than_created(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: UniswapV2Pool,
) -> None:
    lp = ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000

    assert lp._state_cache is not None
    state_before_discard = lp._state_cache.copy()
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.discard_states_before_block(
        lp.update_block - 1
    )
    assert lp._state_cache == state_before_discard


def test_discard_after_last_update(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: UniswapV2Pool,
) -> None:
    lp = ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000

    with pytest.raises(
        NoPoolStateAvailable, match=f"No pool state known prior to block {lp.update_block + 1}"
    ):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.discard_states_before_block(
            lp.update_block + 1
        )


def test_simulations(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: UniswapV2Pool,
):
    sim_result = UniswapV2PoolSimulationResult(
        amount0_delta=8000000000,
        amount1_delta=-847228560678214929944,
        initial_state=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.state,
        final_state=UniswapV2PoolState(
            address=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.address,
            block=None,
            reserves_token0=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token0
            + 8000000000,
            reserves_token1=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token1
            - 847228560678214929944,
        ),
    )

    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.simulate_exact_input_swap(
            token_in=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.token0,
            token_in_quantity=8000000000,
        )
        == sim_result
    )

    sim_result = UniswapV2PoolSimulationResult(
        amount0_delta=-5154005339,
        amount1_delta=1200000000000000000000,
        initial_state=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.state,
        final_state=UniswapV2PoolState(
            address=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.address,
            block=None,
            reserves_token0=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token0
            - 5154005339,
            reserves_token1=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token1
            + 1200000000000000000000,
        ),
    )

    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.simulate_exact_input_swap(
            token_in=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.token1,
            token_in_quantity=1200000000000000000000,
        )
        == sim_result
    )

    added_liquidity = 10_000_000
    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.simulate_add_liquidity(
        added_reserves_token0=added_liquidity, added_reserves_token1=added_liquidity
    ) == UniswapV2PoolSimulationResult(
        amount0_delta=added_liquidity,
        amount1_delta=added_liquidity,
        initial_state=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.state,
        final_state=UniswapV2PoolState(
            address=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.address,
            block=None,
            reserves_token0=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token0
            + added_liquidity,
            reserves_token1=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token1
            + added_liquidity,
        ),
    )

    removed_liquidity = 10_000_000
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.simulate_remove_liquidity(
            removed_reserves_token0=removed_liquidity, removed_reserves_token1=removed_liquidity
        )
        == UniswapV2PoolSimulationResult(
            amount0_delta=-removed_liquidity,
            amount1_delta=-removed_liquidity,
            initial_state=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.state,
            final_state=UniswapV2PoolState(
                address=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.address,
                block=None,
                reserves_token0=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token0
                - removed_liquidity,
                reserves_token1=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token1
                - removed_liquidity,
            ),
        )
    )


def test_simulation_input_validation(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: UniswapV2Pool,
    dai,
):
    lp = ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000
    with pytest.raises(DegenbotValueError, match="token_in is unknown."):
        lp.simulate_exact_input_swap(token_in=dai, token_in_quantity=1_000)
    with pytest.raises(DegenbotValueError, match="token_out is unknown."):
        lp.simulate_exact_output_swap(token_out=dai, token_out_quantity=1_000)


def test_simulations_with_override(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: UniswapV2Pool,
):
    pool_state_override = UniswapV2PoolState(
        address=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.address,
        block=None,
        reserves_token0=16027096956,
        reserves_token1=2602647332090181827846,
    )

    expected_sim_result = UniswapV2PoolSimulationResult(
        amount0_delta=8000000000,
        amount1_delta=-864834865217768537471,
        initial_state=pool_state_override,
        final_state=UniswapV2PoolState(
            address=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.address,
            block=cast("BlockNumber", 17_600_000),
            reserves_token0=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token0
            + 8000000000,
            reserves_token1=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token1
            - 864834865217768537471,
        ),
    )

    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.simulate_exact_input_swap(
            token_in=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.token0,
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
            address=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.address,
            block=cast("BlockNumber", 17_600_000),
            reserves_token0=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token0
            + 13752842264,
            reserves_token1=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token1
            - 1200000000000000000000,
        ),
    )

    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.simulate_exact_output_swap(
            token_out=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.token1,
            token_out_quantity=1200000000000000000000,
            override_state=pool_state_override,
        )
        == expected_sim_result
    )


def test_swap_for_all(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: UniswapV2Pool,
):
    # The last token in a pool can never be swapped for
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.calculate_tokens_out_from_tokens_in(
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.token1,
            2**256 - 1,
        )
        == ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token0 - 1
    )
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.calculate_tokens_out_from_tokens_in(
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.token0,
            2**256 - 1,
        )
        == ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token1 - 1
    )

    with pytest.raises(LiquidityPoolError):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.calculate_tokens_in_from_tokens_out(
            token_out=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.token0,
            token_out_quantity=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token0,
        )

    with pytest.raises(LiquidityPoolError):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.calculate_tokens_in_from_tokens_out(
            token_out=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.token1,
            token_out_quantity=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token1,
        )


def test_zero_swaps(ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: UniswapV2Pool):
    with pytest.raises(InvalidSwapInputAmount):
        assert (
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.calculate_tokens_out_from_tokens_in(
                ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.token0,
                0,
            )
            == 0
        )

    with pytest.raises(InvalidSwapInputAmount):
        assert (
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.calculate_tokens_out_from_tokens_in(
                ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.token1,
                0,
            )
            == 0
        )


def test_auto_update(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: UniswapV2Pool,
    fork_mainnet_archive: AnvilFork,
):
    block_number = 18_000_000
    fork_mainnet_archive.reset(block_number=block_number)
    set_web3(fork_mainnet_archive.w3)
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.auto_update()
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.auto_update()

    # Attempt an update in the past
    with pytest.raises(LateUpdateError):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.auto_update(
            block_number=block_number - 10
        )


def test_late_update(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: UniswapV2Pool,
):
    # Provide some semi-random updates
    for block_number in range(
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.update_block,
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.update_block + 5,
    ):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.external_update(
            update=UniswapV2PoolExternalUpdate(
                block_number=BlockNumber(block_number),
                reserves_token0=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token0
                + block_number * 10,
                reserves_token1=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token1
                - block_number * 10,
            )
        )

    # Send a late update
    with pytest.raises(ExternalUpdateError):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.external_update(
            update=UniswapV2PoolExternalUpdate(
                block_number=BlockNumber(
                    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.update_block - 1
                ),
                reserves_token0=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token0
                + 1,
                reserves_token1=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token1
                - 1,
            )
        )

    # Send a duplicate update
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.external_update(
        update=UniswapV2PoolExternalUpdate(
            block_number=BlockNumber(
                ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.update_block + 1
            ),
            reserves_token0=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token0,
            reserves_token1=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token1,
        )
    )
