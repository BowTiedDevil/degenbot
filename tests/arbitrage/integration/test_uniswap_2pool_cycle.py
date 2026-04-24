import dataclasses

import pytest

from degenbot.anvil_fork import AnvilFork
from degenbot.arbitrage.uniswap_2pool_cycle_testing import _UniswapTwoPoolCycleTesting
from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import set_web3
from degenbot.erc20.erc20 import Erc20Token
from degenbot.erc20.ether_placeholder import EtherPlaceholder
from degenbot.exceptions.arbitrage import RateOfExchangeBelowMinimum
from degenbot.registry import pool_registry, token_registry
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool

# Token addresses
NATIVE_ADDRESS = get_checksum_address("0x0000000000000000000000000000000000000000")
WBTC_ADDRESS = get_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")
WETH_ADDRESS = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")

# Uniswap V2 & V3 WBTC-WETH pools
WBTC_WETH_V2_POOL_ADDRESS = "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940"
WBTC_WETH_V3_POOL_ADDRESS = "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD"

# Uniswap V4
V4_POOL_MANAGER = get_checksum_address("0x000000000004444c5dc75cB358380D2e3dE08A90")
V4_STATE_VIEW_ADDRESS = get_checksum_address("0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227")

# Uniswap V4 WBTC-Ether pool
WBTC_ETH_V4_POOL_ID = "0x54c72c46df32f2cc455e84e41e191b26ed73a29452cdd3d82f511097af9f427e"
WBTC_ETH_V4_POOL_FEE = 3000
WBTC_ETH_V4_POOL_TICK_SPACING = 60
WBTC_ETH_V4_POOL_HOOKS = "0x0000000000000000000000000000000000000000"


@pytest.fixture
def wbtc(fork_mainnet_full: AnvilFork) -> Erc20Token:
    set_web3(fork_mainnet_full.w3)

    token = token_registry.get(
        chain_id=fork_mainnet_full.w3.eth.chain_id,
        token_address=WBTC_ADDRESS,
    )
    if token is None:
        token = Erc20Token(WBTC_ADDRESS)
    return token


@pytest.fixture
def weth(fork_mainnet_full: AnvilFork) -> Erc20Token:
    set_web3(fork_mainnet_full.w3)

    token = token_registry.get(
        chain_id=fork_mainnet_full.w3.eth.chain_id,
        token_address=WETH_ADDRESS,
    )
    if token is None:
        token = Erc20Token(WETH_ADDRESS)
    return token


@pytest.fixture
def ether_placeholder(fork_mainnet_full: AnvilFork) -> Erc20Token:
    set_web3(fork_mainnet_full.w3)

    token = token_registry.get(
        chain_id=fork_mainnet_full.w3.eth.chain_id,
        token_address=NATIVE_ADDRESS,
    )
    if token is None:
        token = EtherPlaceholder(NATIVE_ADDRESS)
    return token


@pytest.fixture
def wbtc_weth_v2_lp(fork_mainnet_full: AnvilFork) -> UniswapV2Pool:
    set_web3(fork_mainnet_full.w3)

    pool = pool_registry.get(
        chain_id=fork_mainnet_full.w3.eth.chain_id,
        pool_address=WBTC_WETH_V2_POOL_ADDRESS,
    )
    if pool is None:
        pool = UniswapV2Pool(WBTC_WETH_V2_POOL_ADDRESS)
    assert isinstance(pool, UniswapV2Pool)
    return pool


@pytest.fixture
def wbtc_weth_v3_lp(fork_mainnet_full: AnvilFork) -> UniswapV3Pool:
    set_web3(fork_mainnet_full.w3)

    pool = pool_registry.get(
        chain_id=fork_mainnet_full.w3.eth.chain_id,
        pool_address=WBTC_WETH_V3_POOL_ADDRESS,
    )
    if pool is None:
        pool = UniswapV3Pool(WBTC_WETH_V3_POOL_ADDRESS)
    assert isinstance(pool, UniswapV3Pool)
    return pool


@pytest.fixture
def wbtc_ether_v4_lp(fork_mainnet_full: AnvilFork) -> UniswapV4Pool:
    set_web3(fork_mainnet_full.w3)

    pool = pool_registry.get(
        chain_id=fork_mainnet_full.w3.eth.chain_id,
        pool_address=V4_POOL_MANAGER,
        pool_id=WBTC_ETH_V4_POOL_ID,
    )
    if pool is None:
        pool = UniswapV4Pool(
            pool_id=WBTC_ETH_V4_POOL_ID,
            pool_manager_address=V4_POOL_MANAGER,
            state_view_address=V4_STATE_VIEW_ADDRESS,
            tokens=(WBTC_ADDRESS, NATIVE_ADDRESS),
            fee=WBTC_ETH_V4_POOL_FEE,
            tick_spacing=WBTC_ETH_V4_POOL_TICK_SPACING,
        )
    assert isinstance(pool, UniswapV4Pool)
    return pool


@pytest.fixture
def arb_v4_v2(
    wbtc_ether_v4_lp: UniswapV3Pool,
    wbtc_weth_v2_lp: UniswapV2Pool,
    ether_placeholder: EtherPlaceholder,
):
    return _UniswapTwoPoolCycleTesting(
        id="V4 -> V2",
        input_token=ether_placeholder,
        swap_pools=[
            wbtc_ether_v4_lp,
            wbtc_weth_v2_lp,
        ],
        max_input=100 * 10**18,
    )


@pytest.fixture
def arb_v2_v4(
    wbtc_weth_v2_lp: UniswapV2Pool,
    wbtc_ether_v4_lp: UniswapV3Pool,
    weth: Erc20Token,
):
    return _UniswapTwoPoolCycleTesting(
        id="V2 -> V4",
        input_token=weth,
        swap_pools=[
            wbtc_weth_v2_lp,
            wbtc_ether_v4_lp,
        ],
        max_input=100 * 10**18,
    )


def test_create_arb_with_either_token_input_or_pools_in_any_order(
    wbtc_weth_v2_lp: UniswapV2Pool,
    wbtc_ether_v4_lp: UniswapV4Pool,
    ether_placeholder: Erc20Token,
    weth: Erc20Token,
):
    arb = _UniswapTwoPoolCycleTesting(
        id="test_arb",
        input_token=ether_placeholder,
        swap_pools=[
            wbtc_ether_v4_lp,
            wbtc_weth_v2_lp,
        ],
        max_input=100 * 10**18,
    )
    assert arb.swap_pools[0] is wbtc_ether_v4_lp
    assert arb.swap_pools[1] is wbtc_weth_v2_lp

    _UniswapTwoPoolCycleTesting(
        id="test_arb",
        input_token=ether_placeholder,
        swap_pools=[
            wbtc_ether_v4_lp,
            wbtc_weth_v2_lp,
        ],
        max_input=100 * 10**18,
    )
    assert arb.swap_pools[0] is wbtc_ether_v4_lp
    assert arb.swap_pools[1] is wbtc_weth_v2_lp

    arb = _UniswapTwoPoolCycleTesting(
        id="test_arb",
        input_token=weth,
        swap_pools=[
            wbtc_weth_v2_lp,
            wbtc_ether_v4_lp,
        ],
        max_input=100 * 10**18,
    )
    assert arb.swap_pools[0] is wbtc_weth_v2_lp
    assert arb.swap_pools[1] is wbtc_ether_v4_lp

    arb = _UniswapTwoPoolCycleTesting(
        id="test_arb",
        input_token=weth,
        swap_pools=[
            wbtc_weth_v2_lp,
            wbtc_ether_v4_lp,
        ],
        max_input=100 * 10**18,
    )
    assert arb.swap_pools[0] is wbtc_weth_v2_lp
    assert arb.swap_pools[1] is wbtc_ether_v4_lp


def test_v2_v4_calculation(
    arb_v2_v4: _UniswapTwoPoolCycleTesting,
    weth: Erc20Token,
    ether_placeholder: EtherPlaceholder,
):
    arb = arb_v2_v4
    v2_pool, v4_pool = arb.swap_pools

    assert isinstance(v2_pool, UniswapV2Pool)
    assert isinstance(v4_pool, UniswapV4Pool)

    # Manipulate the V2 reserves by donating WBTC to create an opportunity:
    # The Ether/WBTC exchange rate at the V4 pool is higher than the WETH/WBTC exchange rate at
    # the V2 pool, because its WBTC value is depressed by the surplus.
    # So WETH/Ether can be cycled at a profit:
    #   - trade WETH -> WBTC  @ V2 (sell "overpriced" WETH for "underpriced" WBTC)
    #   - trade WBTC -> Ether @ V4 (sell "overpriced" WBTC for "underpriced" Ether)

    assert v2_pool.token0 == WBTC_ADDRESS
    wbtc_donation_amount = 10 * 10**v2_pool.token0.decimals
    v2_pool_state = dataclasses.replace(
        v2_pool.state,
        reserves_token0=v2_pool.reserves_token0 + wbtc_donation_amount,
    )

    v2_exchange_rate = v2_pool.get_absolute_exchange_rate(token=weth, override_state=v2_pool_state)
    v4_exchange_rate = v4_pool.get_absolute_exchange_rate(token=ether_placeholder)
    assert v4_exchange_rate > v2_exchange_rate

    arb.calculate(state_overrides={v2_pool: v2_pool_state})


def test_v2_v4_calculation_rejects_unprofitable_opportunity(arb_v2_v4: _UniswapTwoPoolCycleTesting):
    arb = arb_v2_v4

    v2_pool, _ = arb.swap_pools
    assert isinstance(v2_pool, UniswapV2Pool)

    # Manipulate the V2 reserves by donating 100 WETH, which creates a V2 ROE > V4 ROE opportunity
    assert v2_pool.token1 == WETH_ADDRESS
    donation_amount = 10 * 10**v2_pool.token1.decimals
    v2_pool_state = dataclasses.replace(
        v2_pool.state,
        reserves_token1=v2_pool.reserves_token1 + donation_amount,
    )

    # The arbitrage path flow is opposite of the opportunity, so the calculation should raise an
    # exception
    with pytest.raises(RateOfExchangeBelowMinimum):
        arb.calculate(state_overrides={v2_pool: v2_pool_state})


def test_v4_v2_calculation(
    arb_v4_v2: _UniswapTwoPoolCycleTesting,
    weth: Erc20Token,
    ether_placeholder: EtherPlaceholder,
):
    arb = arb_v4_v2
    v4_pool, v2_pool = arb.swap_pools

    assert isinstance(v2_pool, UniswapV2Pool)
    assert isinstance(v4_pool, UniswapV4Pool)

    # Manipulate the V2 reserves by donating WETH to create an opportunity:
    # The WETH/WBTC exchange rate at the V2 pool is higher than the Ether/WBTC exchange rate at
    # the V4 pool, because its WETH value is depressed by the surplus.
    # So WETH/Ether can be cycled at a profit:
    #   - trade Ether -> WBTC @ V4 (sell "overpriced" Ether for "underpriced" WBTC)
    #   - trade WBTC  -> WETH @ V2 (sell "overpriced" WBTC  for "underpriced" WETH)

    assert v2_pool.token1 == WETH_ADDRESS
    weth_donation_amount = 100 * 10**v2_pool.token1.decimals
    v2_pool_state = dataclasses.replace(
        v2_pool.state,
        reserves_token1=v2_pool.reserves_token1 + weth_donation_amount,
    )

    v2_exchange_rate = v2_pool.get_absolute_exchange_rate(token=weth, override_state=v2_pool_state)
    v4_exchange_rate = v4_pool.get_absolute_exchange_rate(token=ether_placeholder)
    assert v2_exchange_rate > v4_exchange_rate

    arb.calculate(state_overrides={v2_pool: v2_pool_state})


def test_v4_v2_calculation_rejects_unprofitable_opportunity(arb_v4_v2: _UniswapTwoPoolCycleTesting):
    arb = arb_v4_v2

    _, v2_pool = arb.swap_pools

    assert isinstance(v2_pool, UniswapV2Pool)

    # Manipulate the V2 reserves by donating WBTC, which creates a V4 ROE > V2 ROE opportunity
    assert v2_pool.token0 == WBTC_ADDRESS
    wbtc_donation_amount = 10 * 10**v2_pool.token0.decimals
    v2_pool_state = dataclasses.replace(
        v2_pool.state,
        reserves_token0=v2_pool.reserves_token0 + wbtc_donation_amount,
    )

    # The arbitrage path flow is opposite of the opportunity, so the calculation should raise an
    # exception
    with pytest.raises(RateOfExchangeBelowMinimum):
        arb.calculate(state_overrides={v2_pool: v2_pool_state})
