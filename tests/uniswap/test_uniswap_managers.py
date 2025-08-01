import pytest
from eth_typing import ChainId

from degenbot.anvil_fork import AnvilFork
from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import set_web3
from degenbot.exceptions.manager import ManagerAlreadyInitialized, ManagerError, PoolNotAssociated
from degenbot.pancakeswap.managers import PancakeV3PoolManager
from degenbot.registry import pool_registry
from degenbot.sushiswap.managers import SushiswapV2PoolManager, SushiswapV3PoolManager
from degenbot.uniswap.deployments import (
    EthereumMainnetSushiswapV2,
    EthereumMainnetSushiswapV3,
    EthereumMainnetUniswapV2,
    EthereumMainnetUniswapV3,
    UniswapFactoryDeployment,
    UniswapV3ExchangeDeployment,
)
from degenbot.uniswap.managers import UniswapV2PoolManager, UniswapV3PoolManager
from degenbot.uniswap.v2_functions import get_v2_pools_from_token_path

MAINNET_UNISWAP_V2_FACTORY_ADDRESS = get_checksum_address(
    "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
)
MAINNET_UNISWAP_V3_FACTORY_ADDRESS = get_checksum_address(
    "0x1F98431c8aD98523631AE4a59f267346ea31F984"
)
MAINNET_SUSHISWAP_V2_FACTORY_ADDRESS = get_checksum_address(
    "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"
)
MAINNET_SUSHISWAP_V3_FACTORY_ADDRESS = get_checksum_address(
    "0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"
)

MAINNET_WETH_ADDRESS = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
MAINNET_WBTC_ADDRESS = get_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")
MAINNET_SUSHISWAPV2_WETH_WBTC_ADDRESS = get_checksum_address(
    "0xceff51756c56ceffca006cd410b03ffc46dd3a58"
)
MAINNET_UNISWAPV2_WETH_WBTC_ADDRESS = get_checksum_address(
    "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940"
)
MAINNET_UNISWAPV3_WETH_WBTC_ADDRESS = get_checksum_address(
    "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD"
)

BASE_UNISWAP_V2_FACTORY_ADDRESS = get_checksum_address("0x8909Dc15e40173Ff4699343b6eB8132c65e18eC6")
BASE_UNISWAP_V3_FACTORY_ADDRESS = get_checksum_address("0x33128a8fC17869897dcE68Ed026d694621f6FDfD")
BASE_WETH_ADDRESS = get_checksum_address("0x4200000000000000000000000000000000000006")
BASE_DEGEN_ADDRESS = get_checksum_address("0x4ed4E862860beD51a9570b96d89aF5E1B0Efefed")
BASE_UNISWAP_V2_WETH_DEGEN_ADDRESS = get_checksum_address(
    "0x7C327d692B72f60b28AecEDbcC1BA784712fE7b2"
)
BASE_UNISWAP_V3_WETH_DEGEN_ADDRESS = get_checksum_address(
    "0xc9034c3E7F58003E6ae0C8438e7c8f4598d5ACAA"
)
BASE_PANCAKESWAP_V3_FACTORY_ADDRESS = get_checksum_address(
    "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"
)
BASE_PANCAKESWAP_V3_DEPLOYER_ADDRESS = get_checksum_address(
    "0x41ff9AA7e16B8B1a8a8dc4f0eFacd93D02d071c9"
)
BASE_CBETH_WETH_V3_POOL_ADDRESS = get_checksum_address("0x257fcbae4ac6b26a02e4fc5e1a11e4174b5ce395")
BASE_CBETH_ADDRESS = get_checksum_address("0x2ae3f1ec7f1f5012cfeab0185bfc7aa3cf0dec22")

BASE_PANCAKESWAP_V3_EXCHANGE = UniswapV3ExchangeDeployment(
    name="PancakeSwap V3",
    chain_id=ChainId.BASE,
    factory=UniswapFactoryDeployment(
        address=BASE_PANCAKESWAP_V3_FACTORY_ADDRESS,
        deployer=BASE_PANCAKESWAP_V3_DEPLOYER_ADDRESS,
        pool_init_hash="0x6ce8eb472fa82df5469c6ab6d485f17c3ad13c8cd7af59b3d4a8026c5ce0f7e2",
    ),
)


def test_create_base_chain_managers(fork_base_full: AnvilFork):
    set_web3(fork_base_full.w3)

    uniswap_v2_pool_manager = UniswapV2PoolManager(factory_address=BASE_UNISWAP_V2_FACTORY_ADDRESS)
    assert uniswap_v2_pool_manager._factory_address == BASE_UNISWAP_V2_FACTORY_ADDRESS

    uniswap_v3_pool_manager = UniswapV3PoolManager(factory_address=BASE_UNISWAP_V3_FACTORY_ADDRESS)
    assert uniswap_v3_pool_manager._factory_address == BASE_UNISWAP_V3_FACTORY_ADDRESS

    # Get known pairs
    uniswap_v2_lp = uniswap_v2_pool_manager.get_pool_from_tokens(
        token_addresses=(BASE_WETH_ADDRESS, BASE_DEGEN_ADDRESS)
    )
    uniswap_v3_lp = uniswap_v3_pool_manager.get_pool_from_tokens_and_fee(
        token_addresses=(BASE_WETH_ADDRESS, BASE_DEGEN_ADDRESS),
        pool_fee=3000,
    )

    assert uniswap_v2_lp.address == BASE_UNISWAP_V2_WETH_DEGEN_ADDRESS
    assert uniswap_v3_lp.address == BASE_UNISWAP_V3_WETH_DEGEN_ADDRESS

    with pytest.raises(ManagerAlreadyInitialized):
        UniswapV2PoolManager(factory_address=BASE_UNISWAP_V2_FACTORY_ADDRESS)
    with pytest.raises(ManagerAlreadyInitialized):
        UniswapV3PoolManager(factory_address=BASE_UNISWAP_V3_FACTORY_ADDRESS)


def test_base_pancake_v3_pool_manager(fork_base_full: AnvilFork):
    set_web3(fork_base_full.w3)
    pancakev3_lp_manager = PancakeV3PoolManager(
        factory_address=BASE_PANCAKESWAP_V3_FACTORY_ADDRESS,
        deployer_address=BASE_PANCAKESWAP_V3_DEPLOYER_ADDRESS,
    )

    v3_pool = pancakev3_lp_manager.get_pool(BASE_CBETH_WETH_V3_POOL_ADDRESS)

    assert (
        pancakev3_lp_manager.get_pool_from_tokens_and_fee(
            token_addresses=(BASE_WETH_ADDRESS, BASE_CBETH_ADDRESS),
            pool_fee=100,
        )
        is v3_pool
    )


def test_base_pancake_v3_pool_manager_from_exchange(fork_base_full: AnvilFork):
    set_web3(fork_base_full.w3)
    PancakeV3PoolManager.from_exchange(BASE_PANCAKESWAP_V3_EXCHANGE)


def test_create_mainnet_managers_from_exchange(fork_mainnet_full: AnvilFork):
    set_web3(fork_mainnet_full.w3)

    UniswapV2PoolManager.from_exchange(EthereumMainnetUniswapV2)
    SushiswapV2PoolManager.from_exchange(EthereumMainnetSushiswapV2)

    UniswapV3PoolManager.from_exchange(EthereumMainnetUniswapV3)
    SushiswapV3PoolManager.from_exchange(EthereumMainnetSushiswapV3)


def test_create_mainnet_managers(fork_mainnet_full: AnvilFork):
    set_web3(fork_mainnet_full.w3)

    uniswap_v2_pool_manager = UniswapV2PoolManager(
        factory_address=MAINNET_UNISWAP_V2_FACTORY_ADDRESS
    )
    sushiswap_v2_pool_manager = SushiswapV2PoolManager(
        factory_address=MAINNET_SUSHISWAP_V2_FACTORY_ADDRESS
    )

    assert uniswap_v2_pool_manager._factory_address == MAINNET_UNISWAP_V2_FACTORY_ADDRESS
    assert sushiswap_v2_pool_manager._factory_address == MAINNET_SUSHISWAP_V2_FACTORY_ADDRESS

    # Ensure each pool manager has a unique state
    assert uniswap_v2_pool_manager.__dict__ is not sushiswap_v2_pool_manager.__dict__

    assert (
        uniswap_v2_pool_manager._untracked_pools is not sushiswap_v2_pool_manager._untracked_pools
    )

    uniswap_v3_pool_manager = UniswapV3PoolManager(
        factory_address=MAINNET_UNISWAP_V3_FACTORY_ADDRESS
    )

    # Get known pairs
    uniswap_v2_lp = uniswap_v2_pool_manager.get_pool_from_tokens(
        token_addresses=(
            MAINNET_WETH_ADDRESS,
            MAINNET_WBTC_ADDRESS,
        )
    )
    sushiswap_v2_lp = sushiswap_v2_pool_manager.get_pool_from_tokens(
        token_addresses=(
            MAINNET_WETH_ADDRESS,
            MAINNET_WBTC_ADDRESS,
        )
    )
    uniswap_v3_lp = uniswap_v3_pool_manager.get_pool_from_tokens_and_fee(
        token_addresses=(
            MAINNET_WETH_ADDRESS,
            MAINNET_WBTC_ADDRESS,
        ),
        pool_fee=3000,
    )

    assert uniswap_v2_lp.address == MAINNET_UNISWAPV2_WETH_WBTC_ADDRESS
    assert sushiswap_v2_lp.address == MAINNET_SUSHISWAPV2_WETH_WBTC_ADDRESS
    assert uniswap_v3_lp.address == MAINNET_UNISWAPV3_WETH_WBTC_ADDRESS

    # Calling get_pool at the wrong pool manager should raise an exception
    with pytest.raises(
        ManagerError, match=f"Pool {uniswap_v2_lp.address} is not associated with this DEX"
    ):
        sushiswap_v2_pool_manager.get_pool(pool_address=uniswap_v2_lp.address)

    assert uniswap_v2_lp.address in sushiswap_v2_pool_manager._untracked_pools
    assert sushiswap_v2_lp.address not in sushiswap_v2_pool_manager._untracked_pools
    with pytest.raises(PoolNotAssociated):
        sushiswap_v2_pool_manager.get_pool(pool_address=uniswap_v2_lp.address)

    with pytest.raises(
        ManagerError, match=f"Pool {sushiswap_v2_lp.address} is not associated with this DEX"
    ):
        uniswap_v2_pool_manager.get_pool(pool_address=sushiswap_v2_lp.address)
    with pytest.raises(PoolNotAssociated):
        uniswap_v2_pool_manager.get_pool(pool_address=sushiswap_v2_lp.address)
    assert sushiswap_v2_lp.address in uniswap_v2_pool_manager._untracked_pools
    assert uniswap_v2_lp.address not in uniswap_v2_pool_manager._untracked_pools


def test_manager_behavior_for_unassociated_pools(fork_mainnet_full: AnvilFork):
    set_web3(fork_mainnet_full.w3)

    uniswap_v3_pool_manager = UniswapV3PoolManager(
        factory_address=MAINNET_UNISWAP_V3_FACTORY_ADDRESS
    )
    sushiswap_v3_pool_manager = SushiswapV3PoolManager(
        factory_address=MAINNET_SUSHISWAP_V3_FACTORY_ADDRESS
    )

    uniswap_v3_pool_manager.get_pool(MAINNET_UNISWAPV3_WETH_WBTC_ADDRESS)

    assert MAINNET_UNISWAPV3_WETH_WBTC_ADDRESS not in sushiswap_v3_pool_manager._untracked_pools
    # The manager will find the registered pool first, compare the factory address, and then reject
    # it as unassociated
    with pytest.raises(PoolNotAssociated):
        sushiswap_v3_pool_manager.get_pool(MAINNET_UNISWAPV3_WETH_WBTC_ADDRESS)

    # The manager will now have this pool in its untracked set, and repeated calls can be rejected
    # faster with the short-circuit check
    assert MAINNET_UNISWAPV3_WETH_WBTC_ADDRESS in sushiswap_v3_pool_manager._untracked_pools
    with pytest.raises(PoolNotAssociated):
        sushiswap_v3_pool_manager.get_pool(MAINNET_UNISWAPV3_WETH_WBTC_ADDRESS)


def test_pool_remove_and_recreate(fork_mainnet_full: AnvilFork):
    set_web3(fork_mainnet_full.w3)

    uniswap_v2_pool_manager = UniswapV2PoolManager(
        factory_address=MAINNET_UNISWAP_V2_FACTORY_ADDRESS
    )

    v2_weth_wbtc_lp = uniswap_v2_pool_manager.get_pool_from_tokens(
        token_addresses=(
            MAINNET_WETH_ADDRESS,
            MAINNET_WBTC_ADDRESS,
        )
    )

    new_v2_weth_wbtc_lp = uniswap_v2_pool_manager.get_pool_from_tokens(
        token_addresses=(
            MAINNET_WETH_ADDRESS,
            MAINNET_WBTC_ADDRESS,
        )
    )

    # The pool manager should have found the original pool in AllPools and re-used it
    assert v2_weth_wbtc_lp is new_v2_weth_wbtc_lp

    # Remove from the pool manager and the registry
    uniswap_v2_pool_manager.remove(pool_address=new_v2_weth_wbtc_lp.address)
    pool_registry.remove(
        pool_address=new_v2_weth_wbtc_lp.address,
        chain_id=1,
    )

    # This should be a completely new pool object
    super_new_v2_weth_wbtc_lp = uniswap_v2_pool_manager.get_pool_from_tokens(
        token_addresses=(
            MAINNET_WETH_ADDRESS,
            MAINNET_WBTC_ADDRESS,
        )
    )
    assert super_new_v2_weth_wbtc_lp is not new_v2_weth_wbtc_lp
    assert super_new_v2_weth_wbtc_lp is not v2_weth_wbtc_lp

    assert (
        pool_registry.get(
            pool_address=v2_weth_wbtc_lp.address,
            chain_id=1,
        )
        is super_new_v2_weth_wbtc_lp
    )

    pool_registry.remove(
        pool_address=v2_weth_wbtc_lp.address,
        chain_id=1,
    )

    assert (
        pool_registry.get(
            pool_address=v2_weth_wbtc_lp.address,
            chain_id=1,
        )
        is not super_new_v2_weth_wbtc_lp
    )

    uniswap_v3_pool_manager = UniswapV3PoolManager(
        factory_address=MAINNET_UNISWAP_V3_FACTORY_ADDRESS
    )
    v3_pool = uniswap_v3_pool_manager.get_pool(MAINNET_UNISWAPV3_WETH_WBTC_ADDRESS)
    assert uniswap_v3_pool_manager.get_pool(MAINNET_UNISWAPV3_WETH_WBTC_ADDRESS) is v3_pool

    pool_registry.remove(pool_address=v3_pool.address, chain_id=uniswap_v3_pool_manager.chain_id)
    uniswap_v3_pool_manager.remove(v3_pool.address)

    assert uniswap_v3_pool_manager.get_pool(MAINNET_UNISWAPV3_WETH_WBTC_ADDRESS) is not v3_pool


def test_get_already_registered_pool(fork_mainnet_full: AnvilFork):
    set_web3(fork_mainnet_full.w3)

    uniswap_v2_pool_manager = UniswapV2PoolManager(
        factory_address=MAINNET_UNISWAP_V2_FACTORY_ADDRESS
    )
    v2_pool = uniswap_v2_pool_manager.get_pool(MAINNET_UNISWAPV2_WETH_WBTC_ADDRESS)
    # Remove from the pool manager, but not the registry
    uniswap_v2_pool_manager.remove(pool_address=MAINNET_UNISWAPV2_WETH_WBTC_ADDRESS)
    new_v2_pool = uniswap_v2_pool_manager.get_pool(MAINNET_UNISWAPV2_WETH_WBTC_ADDRESS)
    assert v2_pool is new_v2_pool

    uniswap_v3_pool_manager = UniswapV3PoolManager(
        factory_address=MAINNET_UNISWAP_V3_FACTORY_ADDRESS
    )
    v3_pool = uniswap_v3_pool_manager.get_pool(MAINNET_UNISWAPV3_WETH_WBTC_ADDRESS)
    # Remove from the pool manager, but not the registry
    uniswap_v3_pool_manager.remove(v3_pool.address)
    new_v3_pool = uniswap_v3_pool_manager.get_pool(MAINNET_UNISWAPV3_WETH_WBTC_ADDRESS)
    assert v3_pool is new_v3_pool


def test_get_pool_with_kwargs(fork_mainnet_full: AnvilFork):
    set_web3(fork_mainnet_full.w3)

    uniswap_v2_pool_manager = UniswapV2PoolManager(
        factory_address=MAINNET_UNISWAP_V2_FACTORY_ADDRESS
    )
    uniswap_v2_pool_manager.get_pool(MAINNET_UNISWAPV2_WETH_WBTC_ADDRESS, pool_class_kwargs={})

    uniswap_v3_pool_manager = UniswapV3PoolManager(
        factory_address=MAINNET_UNISWAP_V3_FACTORY_ADDRESS
    )
    uniswap_v3_pool_manager.get_pool(MAINNET_UNISWAPV3_WETH_WBTC_ADDRESS, pool_class_kwargs={})


def test_pools_from_token_path(fork_mainnet_full: AnvilFork) -> None:
    set_web3(fork_mainnet_full.w3)

    uniswap_v2_pool_manager = UniswapV2PoolManager(
        factory_address=MAINNET_UNISWAP_V2_FACTORY_ADDRESS
    )

    assert get_v2_pools_from_token_path(
        tx_path=[MAINNET_WBTC_ADDRESS, MAINNET_WETH_ADDRESS],
        pool_manager=uniswap_v2_pool_manager,
    ) == [
        uniswap_v2_pool_manager.get_pool_from_tokens(
            token_addresses=(MAINNET_WBTC_ADDRESS, MAINNET_WETH_ADDRESS)
        ),
    ]
