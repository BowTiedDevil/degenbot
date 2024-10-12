import pytest
from eth_typing import ChainId
from eth_utils.address import to_checksum_address
from web3 import Web3

from degenbot import AnvilFork, PancakeV3Pool
from degenbot.config import set_web3
from degenbot.exceptions import ManagerAlreadyInitialized, ManagerError, PoolNotAssociated
from degenbot.pancakeswap.managers import PancakeV3PoolManager
from degenbot.registry.all_pools import pool_registry
from degenbot.sushiswap.managers import SushiswapV2PoolManager
from degenbot.uniswap.deployments import UniswapFactoryDeployment, UniswapV3ExchangeDeployment
from degenbot.uniswap.managers import UniswapV2PoolManager, UniswapV3PoolManager
from degenbot.uniswap.v2_functions import get_v2_pools_from_token_path

MAINNET_UNISWAP_V2_FACTORY_ADDRESS = to_checksum_address(
    "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
)
MAINNET_UNISWAP_V3_FACTORY_ADDRESS = to_checksum_address(
    "0x1F98431c8aD98523631AE4a59f267346ea31F984"
)
MAINNET_SUSHISWAP_V2_FACTORY_ADDRESS = to_checksum_address(
    "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"
)
MAINNET_WETH_ADDRESS = to_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
MAINNET_WBTC_ADDRESS = to_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")
MAINNET_SUSHISWAPV2_WETH_WBTC_ADDRESS = to_checksum_address(
    "0xceff51756c56ceffca006cd410b03ffc46dd3a58"
)
MAINNET_UNISWAPV2_WETH_WBTC_ADDRESS = to_checksum_address(
    "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940"
)
MAINNET_UNISWAPV3_WETH_WBTC_ADDRESS = to_checksum_address(
    "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD"
)

BASE_UNISWAP_V2_FACTORY_ADDRESS = to_checksum_address("0x8909Dc15e40173Ff4699343b6eB8132c65e18eC6")
BASE_UNISWAP_V3_FACTORY_ADDRESS = to_checksum_address("0x33128a8fC17869897dcE68Ed026d694621f6FDfD")
BASE_WETH_ADDRESS = to_checksum_address("0x4200000000000000000000000000000000000006")
BASE_DEGEN_ADDRESS = to_checksum_address("0x4ed4E862860beD51a9570b96d89aF5E1B0Efefed")
BASE_UNISWAP_V2_WETH_DEGEN_ADDRESS = to_checksum_address(
    "0x7C327d692B72f60b28AecEDbcC1BA784712fE7b2"
)
BASE_UNISWAP_V3_WETH_DEGEN_ADDRESS = to_checksum_address(
    "0xc9034c3E7F58003E6ae0C8438e7c8f4598d5ACAA"
)
BASE_PANCAKESWAP_V3_FACTORY_ADDRESS = to_checksum_address(
    "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"
)
BASE_PANCAKESWAP_V3_DEPLOYER_ADDRESS = to_checksum_address(
    "0x41ff9AA7e16B8B1a8a8dc4f0eFacd93D02d071c9"
)
BASE_CBETH_WETH_V3_POOL_ADDRESS = to_checksum_address("0x257fcbae4ac6b26a02e4fc5e1a11e4174b5ce395")
BASE_CBETH_ADDRESS = to_checksum_address("0x2ae3f1ec7f1f5012cfeab0185bfc7aa3cf0dec22")

BASE_PANCAKESWAP_V3_EXCHANGE = UniswapV3ExchangeDeployment(
    name="PancakeSwap V3",
    chain_id=ChainId.BASE,
    factory=UniswapFactoryDeployment(
        address=BASE_PANCAKESWAP_V3_FACTORY_ADDRESS,
        deployer=BASE_PANCAKESWAP_V3_DEPLOYER_ADDRESS,
        pool_init_hash="0x6ce8eb472fa82df5469c6ab6d485f17c3ad13c8cd7af59b3d4a8026c5ce0f7e2",
    ),
)


def test_create_base_chain_managers(base_full_node_web3: Web3):
    set_web3(base_full_node_web3)

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


def test_base_pancake_v3_pool_manager(base_full_node_web3: Web3):
    set_web3(base_full_node_web3)
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


def test_create_mainnet_managers(ethereum_archive_node_web3: Web3):
    set_web3(ethereum_archive_node_web3)

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


def test_pool_remove_and_recreate(ethereum_archive_node_web3: Web3):
    set_web3(ethereum_archive_node_web3)

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


def test_pools_from_token_path(ethereum_archive_node_web3: Web3) -> None:
    set_web3(ethereum_archive_node_web3)

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


def test_same_block(fork_mainnet: AnvilFork):
    _BLOCK = 18493777
    fork_mainnet.reset(block_number=_BLOCK)
    set_web3(fork_mainnet.w3)

    uniswap_v2_pool_manager = UniswapV2PoolManager(
        factory_address=MAINNET_UNISWAP_V2_FACTORY_ADDRESS
    )

    v2_heyjoe_weth_lp = uniswap_v2_pool_manager.get_pool(
        pool_address="0xC928CF054fE73CaB56d753BA4b508da0F82FABFD",
        state_block=_BLOCK,
    )

    uniswap_v2_pool_manager.remove(pool_address=v2_heyjoe_weth_lp.address)
    pool_registry.remove(
        pool_address=v2_heyjoe_weth_lp.address,
        chain_id=1,
    )

    new_v2_heyjoe_weth_lp = uniswap_v2_pool_manager.get_pool(
        pool_address="0xC928CF054fE73CaB56d753BA4b508da0F82FABFD",
        state_block=_BLOCK,
    )

    assert v2_heyjoe_weth_lp is not new_v2_heyjoe_weth_lp
