import pickle
from fractions import Fraction
from typing import Dict

import degenbot
import pytest
import web3
from degenbot.config import set_web3
from degenbot.erc20_token import Erc20Token
from degenbot.exceptions import (
    ExternalUpdateError,
    LiquidityPoolError,
    NoPoolStateAvailable,
    ZeroSwapError,
)
from degenbot.fork.anvil_fork import AnvilFork
from degenbot.registry.all_pools import AllPools
from degenbot.uniswap.v2_dataclasses import UniswapV2PoolSimulationResult, UniswapV2PoolState
from degenbot.uniswap.v2_liquidity_pool import CamelotLiquidityPool, LiquidityPool
from eth_utils.address import to_checksum_address

UNISWAP_V2_ROUTER02 = to_checksum_address("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D")
UNISWAP_V2_WBTC_WETH_POOL = to_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D940")
UNISWAPV2_FACTORY = to_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
UNISWAPV2_FACTORY_POOL_INIT_HASH = (
    "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"
)

DAI_CONTRACT_ADDRESS = "0x6B175474E89094C44Da98b954EedeAC495271d0F"
WBTC_CONTRACT_ADDRESS = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
WETH_CONTRACT_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"


@pytest.fixture(scope="function")
def ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000(
    fork_mainnet_archive: AnvilFork,
) -> LiquidityPool:
    fork_mainnet_archive.reset(block_number=17_600_000)
    set_web3(fork_mainnet_archive.w3)
    return LiquidityPool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        update_method="external",
        factory_address=UNISWAPV2_FACTORY,
        factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
    )


@pytest.fixture(scope="function")
def ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_650_000(
    fork_mainnet_archive: AnvilFork,
) -> LiquidityPool:
    fork_mainnet_archive.reset(block_number=17_650_000)
    set_web3(fork_mainnet_archive.w3)
    return LiquidityPool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        update_method="external",
        factory_address=UNISWAPV2_FACTORY,
        factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
    )


@pytest.fixture(scope="function")
def ethereum_uniswap_v2_wbtc_weth_liquiditypool(
    fork_mainnet_archive: AnvilFork,
) -> LiquidityPool:
    set_web3(fork_mainnet_archive.w3)
    return LiquidityPool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        update_method="external",
        factory_address=UNISWAPV2_FACTORY,
        factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
    )


@pytest.fixture
def dai(ethereum_full_node_web3) -> Erc20Token:
    set_web3(ethereum_full_node_web3)
    return Erc20Token(DAI_CONTRACT_ADDRESS)


@pytest.fixture
def wbtc(ethereum_full_node_web3) -> Erc20Token:
    set_web3(ethereum_full_node_web3)
    return Erc20Token(WBTC_CONTRACT_ADDRESS)


@pytest.fixture
def weth(ethereum_full_node_web3) -> Erc20Token:
    set_web3(ethereum_full_node_web3)
    return Erc20Token(WETH_CONTRACT_ADDRESS)


@pytest.fixture
def wbtc_weth_v2_lp(fork_mainnet: AnvilFork) -> LiquidityPool:
    set_web3(fork_mainnet.w3)
    return LiquidityPool(UNISWAP_V2_WBTC_WETH_POOL)


def test_create_pool(
    ethereum_full_node_web3: web3.Web3,
    wbtc: Erc20Token,
    weth: Erc20Token,
):
    set_web3(ethereum_full_node_web3)
    LiquidityPool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        tokens=[wbtc, weth],
        name="WBTC-WETH (V2, 0.30%)",
        factory_address=UNISWAPV2_FACTORY,
        factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
    )

    # Omitting tokens
    LiquidityPool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        # tokens=[token0, token1],
        name="WBTC-WETH (V2, 0.30%)",
        factory_address=UNISWAPV2_FACTORY,
        factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
    )

    with pytest.raises(ValueError, match="Expected 2 tokens, found"):
        LiquidityPool(
            address=UNISWAP_V2_WBTC_WETH_POOL,
            tokens=[wbtc, weth, wbtc],
            name="WBTC-WETH (V2, 0.30%)",
            factory_address=UNISWAPV2_FACTORY,
            factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
        )

    with pytest.raises(ValueError, match="Expected 2 tokens, found"):
        LiquidityPool(
            address=UNISWAP_V2_WBTC_WETH_POOL,
            tokens=[wbtc],
            name="WBTC-WETH (V2, 0.30%)",
            factory_address=UNISWAPV2_FACTORY,
            factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
        )


def test_price_is_inverse_of_exchange_rate(wbtc_weth_v2_lp: LiquidityPool):
    for token in [wbtc_weth_v2_lp.token0, wbtc_weth_v2_lp.token1]:
        assert wbtc_weth_v2_lp.get_absolute_price(token) == 1 / wbtc_weth_v2_lp.get_absolute_rate(
            token
        )


def test_nominal_rate_scaled_by_decimals(wbtc_weth_v2_lp: LiquidityPool):
    for token in [wbtc_weth_v2_lp.token0, wbtc_weth_v2_lp.token1]:
        nom_rate = int(wbtc_weth_v2_lp.get_nominal_rate(token))
        abs_rate = int(wbtc_weth_v2_lp.get_absolute_rate(token))
        assert nom_rate == abs_rate // (
            10 ** (wbtc_weth_v2_lp.token1.decimals - wbtc_weth_v2_lp.token0.decimals)
        )


def test_nominal_price_scaled_by_decimals(wbtc_weth_v2_lp: LiquidityPool):
    for token in [wbtc_weth_v2_lp.token0, wbtc_weth_v2_lp.token1]:
        nom_price = int(wbtc_weth_v2_lp.get_nominal_price(token))
        abs_price = int(wbtc_weth_v2_lp.get_absolute_price(token))
        assert nom_price == abs_price // (
            10 ** (wbtc_weth_v2_lp.token1.decimals - wbtc_weth_v2_lp.token0.decimals)
        )


@pytest.mark.skip(reason="Unreliable RPC")
def test_create_camelot_v2_stable_pool(fork_arbitrum_archive: AnvilFork):
    CAMELOT_MIM_USDC_LP_ADDRESS = "0x68A0859de50B4Dfc6EFEbE981cA906D38Cdb0D1F"
    FORK_BLOCK = 153_759_000
    fork_arbitrum_archive.reset(block_number=FORK_BLOCK)

    assert fork_arbitrum_archive.w3.eth.get_block_number() == FORK_BLOCK
    set_web3(fork_arbitrum_archive.w3)

    lp = CamelotLiquidityPool(address=CAMELOT_MIM_USDC_LP_ADDRESS)
    assert lp.stable_swap is True

    token_in = lp.token0  # MIM token
    amount_in = 1000 * 10**token_in.decimals  # nominal value of $1000

    # Test that the swap output from the pool contract matches the off-chain calculation
    contract_amount = lp._w3_contract.functions.getAmountOut(
        amountIn=amount_in, tokenIn=token_in.address
    ).call()
    assert contract_amount == lp.calculate_tokens_out_from_tokens_in(
        token_in=token_in,
        token_in_quantity=amount_in,
    )
    current_reserves = lp.reserves_token0, lp.reserves_token1

    rewind_block_length = 500_000
    contract_amount_old = lp._w3_contract.functions.getAmountOut(
        amountIn=amount_in, tokenIn=token_in.address
    ).call(block_identifier=FORK_BLOCK - rewind_block_length)

    assert contract_amount != contract_amount_old

    old_reserves = lp._w3_contract.functions.getReserves().call(
        block_identifier=FORK_BLOCK - rewind_block_length
    )
    lp.state = UniswapV2PoolState(
        pool=lp.address,
        reserves_token0=old_reserves[0],
        reserves_token1=old_reserves[1],
    )

    assert contract_amount_old == lp.calculate_tokens_out_from_tokens_in(
        token_in=token_in,
        token_in_quantity=amount_in,
    )

    # Override the state and verify the overridden amounts match the first test
    assert contract_amount == lp.calculate_tokens_out_from_tokens_in(
        token_in=token_in,
        token_in_quantity=amount_in,
        override_state=UniswapV2PoolState(
            pool=lp.address,
            reserves_token0=current_reserves[0],
            reserves_token1=current_reserves[1],
        ),
    )


def test_create_camelot_v2_pool(fork_arbitrum: AnvilFork):
    CAMELOT_WETH_USDC_LP_ADDRESS = "0x84652bb2539513BAf36e225c930Fdd8eaa63CE27"
    set_web3(fork_arbitrum.w3)
    lp = CamelotLiquidityPool(address=CAMELOT_WETH_USDC_LP_ADDRESS)
    assert lp.stable_swap is False

    token_in = lp.token1
    amount_in = 1000 * 10**token_in.decimals  # nominal value of $1000

    assert lp._w3_contract.functions.getAmountOut(
        amountIn=amount_in, tokenIn=token_in.address
    ).call() == lp.calculate_tokens_out_from_tokens_in(
        token_in=token_in,
        token_in_quantity=amount_in,
    )


def test_pickle_camelot_v2_pool(fork_arbitrum: AnvilFork):
    CAMELOT_WETH_USDC_LP_ADDRESS = "0x84652bb2539513BAf36e225c930Fdd8eaa63CE27"
    set_web3(fork_arbitrum.w3)
    lp = CamelotLiquidityPool(address=CAMELOT_WETH_USDC_LP_ADDRESS)
    pickle.dumps(lp)


def test_create_empty_pool(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool: LiquidityPool,
    ethereum_full_node_web3: web3.Web3,
):
    set_web3(ethereum_full_node_web3)

    LiquidityPool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        tokens=[
            ethereum_uniswap_v2_wbtc_weth_liquiditypool.token0,
            ethereum_uniswap_v2_wbtc_weth_liquiditypool.token1,
        ],
        name="WBTC-WETH (V2, 0.30%)",
        factory_address=UNISWAPV2_FACTORY,
        factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
        empty=True,
    )

    # Create pool without tokens
    with pytest.raises(ValueError):
        LiquidityPool(
            address=UNISWAP_V2_WBTC_WETH_POOL,
            # tokens=[_pool.token0, _pool.token1],
            name="WBTC-WETH (V2, 0.30%)",
            factory_address=UNISWAPV2_FACTORY,
            factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
            empty=True,
        )

    # Create pool without factory address
    with pytest.raises(ValueError):
        LiquidityPool(
            address=UNISWAP_V2_WBTC_WETH_POOL,
            tokens=[
                ethereum_uniswap_v2_wbtc_weth_liquiditypool.token0,
                ethereum_uniswap_v2_wbtc_weth_liquiditypool.token1,
            ],
            name="WBTC-WETH (V2, 0.30%)",
            # factory_address=UNISWAPV2_FACTORY_ADDRESS,
            factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
            empty=True,
        )

    # Create pool without init hash
    with pytest.raises(ValueError):
        LiquidityPool(
            address=UNISWAP_V2_WBTC_WETH_POOL,
            tokens=[
                ethereum_uniswap_v2_wbtc_weth_liquiditypool.token0,
                ethereum_uniswap_v2_wbtc_weth_liquiditypool.token1,
            ],
            name="WBTC-WETH (V2, 0.30%)",
            factory_address=UNISWAPV2_FACTORY,
            # factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
            empty=True,
        )

    # Create pool with a malformed init hash
    bad_init_hash = UNISWAPV2_FACTORY_POOL_INIT_HASH.replace("a", "b")
    with pytest.raises(
        ValueError,
        match=f"Pool address {UNISWAP_V2_WBTC_WETH_POOL} does not match deterministic address",
    ):
        LiquidityPool(
            address=UNISWAP_V2_WBTC_WETH_POOL,
            tokens=[
                ethereum_uniswap_v2_wbtc_weth_liquiditypool.token0,
                ethereum_uniswap_v2_wbtc_weth_liquiditypool.token1,
            ],
            name="WBTC-WETH (V2, 0.30%)",
            factory_address=UNISWAPV2_FACTORY,
            factory_init_hash=bad_init_hash,
            empty=True,
        )

    # Create with non-standard fee
    LiquidityPool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        tokens=[
            ethereum_uniswap_v2_wbtc_weth_liquiditypool.token0,
            ethereum_uniswap_v2_wbtc_weth_liquiditypool.token1,
        ],
        name="WBTC-WETH (V2, 0.30%)",
        factory_address=UNISWAPV2_FACTORY,
        factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
        fee=Fraction(2, 1000),
        empty=True,
    )

    # Create with float fee
    with pytest.raises(TypeError, match="LP fee was not correctly passed!"):
        LiquidityPool(
            address=UNISWAP_V2_WBTC_WETH_POOL,
            tokens=[
                ethereum_uniswap_v2_wbtc_weth_liquiditypool.token0,
                ethereum_uniswap_v2_wbtc_weth_liquiditypool.token1,
            ],
            name="WBTC-WETH (V2, 0.30%)",
            factory_address=UNISWAPV2_FACTORY,
            factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
            fee=0.003,  # type:ignore
            empty=True,
        )

    # Create with float fee in tuple format
    with pytest.raises(TypeError, match="LP fee was not correctly passed!"):
        LiquidityPool(
            address=UNISWAP_V2_WBTC_WETH_POOL,
            tokens=[
                ethereum_uniswap_v2_wbtc_weth_liquiditypool.token0,
                ethereum_uniswap_v2_wbtc_weth_liquiditypool.token1,
            ],
            name="WBTC-WETH (V2, 0.30%)",
            factory_address=UNISWAPV2_FACTORY,
            factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
            fee=(0.003, 0.003),  # type:ignore
            empty=True,
        )

    # Create split-fee pool of differing values
    lp = LiquidityPool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        tokens=[
            ethereum_uniswap_v2_wbtc_weth_liquiditypool.token0,
            ethereum_uniswap_v2_wbtc_weth_liquiditypool.token1,
        ],
        factory_address=UNISWAPV2_FACTORY,
        factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
        fee=(Fraction(3, 1000), Fraction(5, 1000)),
        empty=True,
    )
    assert lp.fee_token0 == Fraction(3, 1000)
    assert lp.fee_token1 == Fraction(5, 1000)

    # Create split-fee pool of equal values
    lp = LiquidityPool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        tokens=[
            ethereum_uniswap_v2_wbtc_weth_liquiditypool.token0,
            ethereum_uniswap_v2_wbtc_weth_liquiditypool.token1,
        ],
        factory_address=UNISWAPV2_FACTORY,
        factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
        fee=(Fraction(6, 1000), Fraction(6, 1000)),
        empty=True,
    )
    assert lp.fee_token0 == Fraction(6, 1000)
    assert lp.fee_token1 == Fraction(6, 1000)


def test_dunder_methods(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool: LiquidityPool,
):
    ethereum_uniswap_v2_wbtc_weth_liquiditypool.__str__()
    ethereum_uniswap_v2_wbtc_weth_liquiditypool.__hash__()


def test_pickle_pool(ethereum_uniswap_v2_wbtc_weth_liquiditypool: LiquidityPool):
    pickle.dumps(ethereum_uniswap_v2_wbtc_weth_liquiditypool)


def test_calculate_tokens_out_from_ratio_out(fork_mainnet_archive: AnvilFork):
    _BLOCK_NUMBER = 17_600_000
    fork_mainnet_archive.reset(block_number=_BLOCK_NUMBER)
    set_web3(fork_mainnet_archive.w3)

    router_contract = fork_mainnet_archive.w3.eth.contract(
        address=to_checksum_address(UNISWAP_V2_ROUTER02),
        abi=degenbot.uniswap.abi.UNISWAP_V2_ROUTER_ABI,
    )

    lp = LiquidityPool(UNISWAP_V2_WBTC_WETH_POOL)

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
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: LiquidityPool,
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

    with pytest.raises(ValueError, match="Could not identify token_in"):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.calculate_tokens_out_from_tokens_in(
            token_in=dai,
            token_in_quantity=1 * 10**18,
        )


def test_calculate_tokens_out_from_tokens_in_with_override(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_650_000: LiquidityPool,
    ethereum_uniswap_v2_wbtc_weth_liquiditypool: LiquidityPool,
):
    pool_state_override = UniswapV2PoolState(
        pool=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_650_000.address,
        reserves_token0=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_650_000.reserves_token0,
        reserves_token1=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_650_000.reserves_token1,
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
    ) == ethereum_uniswap_v2_wbtc_weth_liquiditypool.calculate_tokens_out_from_tokens_in(
        token_in=ethereum_uniswap_v2_wbtc_weth_liquiditypool.token0,
        token_in_quantity=8000000000,
    )


def test_calculate_tokens_in_from_tokens_out(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: LiquidityPool,
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
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: LiquidityPool, dai: Erc20Token
):
    # Overridden reserve values for this test are taken at block height 17,650,000
    # token0 reserves: 16027096956
    # token1 reserves: 2602647332090181827846

    pool_state_override = UniswapV2PoolState(
        pool=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.address,
        reserves_token0=16027096956,
        reserves_token1=2602647332090181827846,
    )

    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.calculate_tokens_in_from_tokens_out(
            token_out=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.token1,
            token_out_quantity=1200000000000000000000,
            override_state=pool_state_override,
        )
        == 13752842264
    )

    with pytest.raises(ValueError, match="Could not identify token_out"):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.calculate_tokens_in_from_tokens_out(
            token_out=dai,
            token_out_quantity=1200000000000000000000,
            override_state=pool_state_override,
        )


def test_comparisons(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: LiquidityPool,
):
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000 == UNISWAP_V2_WBTC_WETH_POOL
    )
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000
        == UNISWAP_V2_WBTC_WETH_POOL.lower()
    )

    del AllPools(chain_id=1)[ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000]

    other_lp = LiquidityPool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        update_method="external",
        tokens=[
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.token0,
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.token1,
        ],
        name="WBTC-WETH (V2, 0.30%)",
        factory_address=UNISWAPV2_FACTORY,
        factory_init_hash=UNISWAPV2_FACTORY_POOL_INIT_HASH,
        fee=Fraction(3, 1000),
        empty=True,
    )

    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000 == other_lp
    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000 is not other_lp

    # sets depend on __hash__ dunder method
    set([ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000, other_lp])


def test_reorg(ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: LiquidityPool):
    from pprint import pprint

    starting_state = ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.state
    starting_block = ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000._update_block

    pprint(ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000._pool_state_archive)

    _FIRST_UPDATE_BLOCK = (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000._update_block + 1
    )
    _LAST_UPDATE_BLOCK = (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000._update_block + 10
    )

    starting_token0_reserves = starting_state.reserves_token0
    starting_token1_reserves = starting_state.reserves_token1

    expected_block_states: Dict[int, UniswapV2PoolState] = {starting_block: starting_state}

    # Provide some dummy updates, then simulate a reorg back to the starting state
    for block_number in range(_FIRST_UPDATE_BLOCK, _LAST_UPDATE_BLOCK + 1):
        assert block_number not in expected_block_states
        print(f"Updating at block {block_number}")
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.update_reserves(
            external_token0_reserves=starting_token0_reserves
            + 10_000 * (1 + block_number - _FIRST_UPDATE_BLOCK),
            external_token1_reserves=starting_token1_reserves
            + 10_000 * (1 + block_number - _FIRST_UPDATE_BLOCK),
            print_ratios=False,
            print_reserves=False,
            update_block=block_number,
        )
        assert (
            block_number
            in ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000._pool_state_archive
        )
        expected_block_states[block_number] = (
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.state
        )

    # pprint(ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000._pool_state_archive)
    # pprint(expected_block_states)
    last_block_state = ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.state

    # Cannot restore to a pool state before the first
    with pytest.raises(NoPoolStateAvailable):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.restore_state_before_block(
            0
        )

    # Last state is before this block, so this will succeed but have no effect on the current state
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.restore_state_before_block(
        _LAST_UPDATE_BLOCK + 1
    )
    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.state == last_block_state

    # Unwind the updates and compare to the stored states at previous blocks
    for block_number in range(_LAST_UPDATE_BLOCK, _FIRST_UPDATE_BLOCK - 1, -1):
        print(f"Restoring block before {block_number}")
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.restore_state_before_block(
            block_number
        )
        assert (
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.state
            == expected_block_states[block_number - 1]
        )

    # Verify the pool has been returned to the starting state
    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.state == starting_state


def test_simulations(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: LiquidityPool,
):
    sim_result = UniswapV2PoolSimulationResult(
        amount0_delta=8000000000,
        amount1_delta=-847228560678214929944,
        initial_state=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.state,
        final_state=UniswapV2PoolState(
            pool=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.address,
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
            pool=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.address,
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

    ADDED_LIQUIDITY = 10_000_000
    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.simulate_add_liquidity(
        added_reserves_token0=ADDED_LIQUIDITY, added_reserves_token1=ADDED_LIQUIDITY
    ) == UniswapV2PoolSimulationResult(
        amount0_delta=ADDED_LIQUIDITY,
        amount1_delta=ADDED_LIQUIDITY,
        initial_state=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.state,
        final_state=UniswapV2PoolState(
            pool=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.address,
            reserves_token0=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token0
            + ADDED_LIQUIDITY,
            reserves_token1=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token1
            + ADDED_LIQUIDITY,
        ),
    )

    REMOVED_LIQUIDITY = 10_000_000
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.simulate_remove_liquidity(
            removed_reserves_token0=REMOVED_LIQUIDITY, removed_reserves_token1=REMOVED_LIQUIDITY
        )
        == UniswapV2PoolSimulationResult(
            amount0_delta=-REMOVED_LIQUIDITY,
            amount1_delta=-REMOVED_LIQUIDITY,
            initial_state=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.state,
            final_state=UniswapV2PoolState(
                pool=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.address,
                reserves_token0=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token0
                - REMOVED_LIQUIDITY,
                reserves_token1=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token1
                - REMOVED_LIQUIDITY,
            ),
        )
    )


def test_simulations_with_override(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: LiquidityPool,
):
    pool_state_override = UniswapV2PoolState(
        pool=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.address,
        reserves_token0=16027096956,
        reserves_token1=2602647332090181827846,
    )

    expected_sim_result = UniswapV2PoolSimulationResult(
        amount0_delta=8000000000,
        amount1_delta=-864834865217768537471,
        initial_state=pool_state_override,
        final_state=UniswapV2PoolState(
            pool=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.address,
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
            pool=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.address,
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
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: LiquidityPool,
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


def test_zero_swaps(ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: LiquidityPool):
    with pytest.raises(ZeroSwapError):
        assert (
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.calculate_tokens_out_from_tokens_in(
                ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.token0,
                0,
            )
            == 0
        )

    with pytest.raises(ZeroSwapError):
        assert (
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.calculate_tokens_out_from_tokens_in(
                ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.token1,
                0,
            )
            == 0
        )


def test_polling_update(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: LiquidityPool,
    fork_mainnet_archive: AnvilFork,
):
    _BLOCK_NUMBER = 18_000_000
    fork_mainnet_archive.reset(block_number=_BLOCK_NUMBER)
    set_web3(fork_mainnet_archive.w3)
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000._update_method = "polling"
    assert ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.update_reserves() is True
    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.update_reserves() is False
    )


def test_late_update(
    ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000: LiquidityPool,
):
    # Provide some semi-random updates
    for block_number in range(
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000._update_block,
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000._update_block + 5,
    ):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.update_reserves(
            external_token0_reserves=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token0
            + block_number * 10,
            external_token1_reserves=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token1
            - block_number * 10,
            update_block=block_number,
        )

    # Send a late update
    with pytest.raises(ExternalUpdateError):
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.update_reserves(
            external_token0_reserves=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token0
            + 1,
            external_token1_reserves=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token1
            - 1,
            update_block=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000._update_block
            - 1,
        )

    with pytest.raises(ValueError):
        assert (
            ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.update_reserves(
                update_block=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000._update_block
                + 1,
            )
            is False
        )

    assert (
        ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.update_reserves(
            external_token0_reserves=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token0,
            external_token1_reserves=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000.reserves_token1,
            update_block=ethereum_uniswap_v2_wbtc_weth_liquiditypool_at_block_17_600_000._update_block
            + 1,
        )
        is False
    )
