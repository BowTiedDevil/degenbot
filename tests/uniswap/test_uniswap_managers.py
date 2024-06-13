import pytest
from degenbot.config import set_web3
from degenbot.dex.uniswap import FACTORY_ADDRESSES, TICKLENS_ADDRESSES
from degenbot.exceptions import ManagerError, PoolNotAssociated
from degenbot.fork.anvil_fork import AnvilFork
from degenbot.registry.all_pools import AllPools
from degenbot.uniswap.managers import (
    UniswapLiquidityPoolManager,
    UniswapV2LiquidityPoolManager,
    UniswapV3LiquidityPoolManager,
)
from degenbot.uniswap.v2_functions import get_v2_pools_from_token_path
from eth_utils.address import to_checksum_address
from web3 import Web3

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


def test_create_base_managers(base_full_node_web3: Web3):
    set_web3(base_full_node_web3)

    UniswapLiquidityPoolManager.add_pool_init_hash(
        chain_id=base_full_node_web3.eth.chain_id,
        factory_address=BASE_UNISWAP_V2_FACTORY_ADDRESS,
        pool_init_hash="0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f",
    )

    uniswap_v2_pool_manager = UniswapV2LiquidityPoolManager(
        factory_address=BASE_UNISWAP_V2_FACTORY_ADDRESS
    )

    assert uniswap_v2_pool_manager._factory_address == BASE_UNISWAP_V2_FACTORY_ADDRESS

    # Create a pool manager with an invalid address
    with pytest.raises(
        ManagerError,
        match=f"Pool manager could not be initialized from unknown factory address {BASE_WETH_ADDRESS}. Add the factory address and pool init hash with `add_factory`, followed by `add_pool_init_hash`",
    ):
        UniswapV2LiquidityPoolManager(factory_address=BASE_WETH_ADDRESS)

    UniswapLiquidityPoolManager.add_pool_init_hash(
        chain_id=base_full_node_web3.eth.chain_id,
        factory_address=BASE_UNISWAP_V3_FACTORY_ADDRESS,
        pool_init_hash="0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
    )

    uniswap_v3_pool_manager = UniswapV3LiquidityPoolManager(
        factory_address=BASE_UNISWAP_V3_FACTORY_ADDRESS
    )

    # Get known pairs
    uniswap_v2_lp = uniswap_v2_pool_manager.get_pool(
        token_addresses=(BASE_WETH_ADDRESS, BASE_DEGEN_ADDRESS)
    )
    uniswap_v3_lp = uniswap_v3_pool_manager.get_pool(
        token_addresses=(BASE_WETH_ADDRESS, BASE_DEGEN_ADDRESS),
        pool_fee=3000,
    )

    assert uniswap_v2_lp.address == BASE_UNISWAP_V2_WETH_DEGEN_ADDRESS
    assert uniswap_v3_lp.address == BASE_UNISWAP_V3_WETH_DEGEN_ADDRESS

    # Create one-off pool managers and verify they return the same object
    assert (
        UniswapV2LiquidityPoolManager(factory_address=BASE_UNISWAP_V2_FACTORY_ADDRESS).get_pool(
            token_addresses=(
                BASE_WETH_ADDRESS,
                BASE_DEGEN_ADDRESS,
            )
        )
        is uniswap_v2_lp
    )
    assert (
        UniswapV3LiquidityPoolManager(factory_address=BASE_UNISWAP_V3_FACTORY_ADDRESS).get_pool(
            token_addresses=(
                BASE_WETH_ADDRESS,
                BASE_DEGEN_ADDRESS,
            ),
            pool_fee=3000,
        )
        is uniswap_v3_lp
    )


def test_base_pancakeswap_v3(base_full_node_web3: Web3):
    BASE_PANCAKESWAP_V3_FACTORY = to_checksum_address("0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865")
    UniswapLiquidityPoolManager.add_pool_init_hash(
        chain_id=base_full_node_web3.eth.chain_id,
        factory_address=BASE_PANCAKESWAP_V3_FACTORY,
        pool_init_hash="0x6ce8eb472fa82df5469c6ab6d485f17c3ad13c8cd7af59b3d4a8026c5ce0f7e2",
    )
    assert BASE_PANCAKESWAP_V3_FACTORY in FACTORY_ADDRESSES[base_full_node_web3.eth.chain_id]
    UniswapLiquidityPoolManager.add_ticklens(
        chain_id=base_full_node_web3.eth.chain_id,
        factory_address=BASE_PANCAKESWAP_V3_FACTORY,
        ticklens_address="0x9a489505a00cE272eAa5e07Dba6491314CaE3796",
    )
    assert (
        "0x9a489505a00cE272eAa5e07Dba6491314CaE3796"
        in TICKLENS_ADDRESSES[base_full_node_web3.eth.chain_id][BASE_PANCAKESWAP_V3_FACTORY]
    )
    pancakev3_lp_manager = UniswapV3LiquidityPoolManager(
        factory_address=BASE_PANCAKESWAP_V3_FACTORY
    )


def test_create_mainnet_managers(ethereum_full_node_web3: Web3):
    set_web3(ethereum_full_node_web3)

    uniswap_v2_pool_manager = UniswapV2LiquidityPoolManager(
        factory_address=MAINNET_UNISWAP_V2_FACTORY_ADDRESS
    )
    sushiswap_v2_pool_manager = UniswapV2LiquidityPoolManager(
        factory_address=MAINNET_SUSHISWAP_V2_FACTORY_ADDRESS
    )

    assert uniswap_v2_pool_manager._factory_address == MAINNET_UNISWAP_V2_FACTORY_ADDRESS
    assert sushiswap_v2_pool_manager._factory_address == MAINNET_SUSHISWAP_V2_FACTORY_ADDRESS

    # Create a pool manager with an invalid address
    with pytest.raises(
        ManagerError,
        match=f"Pool manager could not be initialized from unknown factory address {MAINNET_WETH_ADDRESS}. Add the factory address and pool init hash with `add_factory`, followed by `add_pool_init_hash`",
    ):
        UniswapV2LiquidityPoolManager(factory_address=MAINNET_WETH_ADDRESS)

    # Ensure each pool manager has a unique state
    assert uniswap_v2_pool_manager.__dict__ is not sushiswap_v2_pool_manager.__dict__

    assert (
        uniswap_v2_pool_manager._untracked_pools is not sushiswap_v2_pool_manager._untracked_pools
    )

    uniswap_v3_pool_manager = UniswapV3LiquidityPoolManager(
        factory_address=MAINNET_UNISWAP_V3_FACTORY_ADDRESS
    )

    # Get known pairs
    uniswap_v2_lp = uniswap_v2_pool_manager.get_pool(
        token_addresses=(
            MAINNET_WETH_ADDRESS,
            MAINNET_WBTC_ADDRESS,
        )
    )
    sushiswap_v2_lp = sushiswap_v2_pool_manager.get_pool(
        token_addresses=(
            MAINNET_WETH_ADDRESS,
            MAINNET_WBTC_ADDRESS,
        )
    )
    uniswap_v3_lp = uniswap_v3_pool_manager.get_pool(
        token_addresses=(
            MAINNET_WETH_ADDRESS,
            MAINNET_WBTC_ADDRESS,
        ),
        pool_fee=3000,
    )

    assert uniswap_v2_lp.address == MAINNET_UNISWAPV2_WETH_WBTC_ADDRESS
    assert sushiswap_v2_lp.address == MAINNET_SUSHISWAPV2_WETH_WBTC_ADDRESS
    assert uniswap_v3_lp.address == MAINNET_UNISWAPV3_WETH_WBTC_ADDRESS

    # Create one-off pool managers and verify they return the same object
    assert (
        UniswapV2LiquidityPoolManager(factory_address=MAINNET_UNISWAP_V2_FACTORY_ADDRESS).get_pool(
            token_addresses=(
                MAINNET_WETH_ADDRESS,
                MAINNET_WBTC_ADDRESS,
            )
        )
        is uniswap_v2_lp
    )
    assert (
        UniswapV2LiquidityPoolManager(
            factory_address=MAINNET_SUSHISWAP_V2_FACTORY_ADDRESS
        ).get_pool(
            token_addresses=(
                MAINNET_WETH_ADDRESS,
                MAINNET_WBTC_ADDRESS,
            )
        )
        is sushiswap_v2_lp
    )
    assert (
        UniswapV3LiquidityPoolManager(factory_address=MAINNET_UNISWAP_V3_FACTORY_ADDRESS).get_pool(
            token_addresses=(
                MAINNET_WETH_ADDRESS,
                MAINNET_WBTC_ADDRESS,
            ),
            pool_fee=3000,
        )
        is uniswap_v3_lp
    )

    # Calling get_pool at the wrong pool manager should raise an exception
    with pytest.raises(
        ManagerError, match=f"Pool {uniswap_v2_lp.address} is not associated with this DEX"
    ):
        UniswapV2LiquidityPoolManager(
            factory_address=MAINNET_SUSHISWAP_V2_FACTORY_ADDRESS
        ).get_pool(pool_address=uniswap_v2_lp.address)
    assert uniswap_v2_lp.address in sushiswap_v2_pool_manager._untracked_pools
    assert sushiswap_v2_lp.address not in sushiswap_v2_pool_manager._untracked_pools
    with pytest.raises(PoolNotAssociated):
        UniswapV2LiquidityPoolManager(
            factory_address=MAINNET_SUSHISWAP_V2_FACTORY_ADDRESS
        ).get_pool(pool_address=uniswap_v2_lp.address)

    with pytest.raises(
        ManagerError, match=f"Pool {sushiswap_v2_lp.address} is not associated with this DEX"
    ):
        UniswapV2LiquidityPoolManager(factory_address=MAINNET_UNISWAP_V2_FACTORY_ADDRESS).get_pool(
            pool_address=sushiswap_v2_lp.address
        )
    with pytest.raises(PoolNotAssociated):
        UniswapV2LiquidityPoolManager(factory_address=MAINNET_UNISWAP_V2_FACTORY_ADDRESS).get_pool(
            pool_address=sushiswap_v2_lp.address
        )
    assert sushiswap_v2_lp.address in uniswap_v2_pool_manager._untracked_pools
    assert uniswap_v2_lp.address not in uniswap_v2_pool_manager._untracked_pools


def test_pool_remove_and_recreate(ethereum_full_node_web3: Web3):
    set_web3(ethereum_full_node_web3)

    uniswap_v2_pool_manager = UniswapV2LiquidityPoolManager(
        factory_address=MAINNET_UNISWAP_V2_FACTORY_ADDRESS
    )

    v2_weth_wbtc_lp = uniswap_v2_pool_manager.get_pool(
        token_addresses=(
            MAINNET_WETH_ADDRESS,
            MAINNET_WBTC_ADDRESS,
        )
    )

    # Redundant but provides test coverage of the __setitem__ method warning if a pool is recreated
    AllPools(chain_id=1)[v2_weth_wbtc_lp.address] = v2_weth_wbtc_lp

    # Remove the pool from the manager
    del uniswap_v2_pool_manager[v2_weth_wbtc_lp]

    new_v2_weth_wbtc_lp = uniswap_v2_pool_manager.get_pool(
        token_addresses=(
            MAINNET_WETH_ADDRESS,
            MAINNET_WBTC_ADDRESS,
        )
    )

    # The pool manager should have found the original pool in AllPools and re-used it
    assert v2_weth_wbtc_lp is new_v2_weth_wbtc_lp

    # Remove from the manager and the AllPools tracker
    del uniswap_v2_pool_manager[new_v2_weth_wbtc_lp]
    del AllPools(chain_id=1)[new_v2_weth_wbtc_lp]

    # This should be a completely new pool object
    super_new_v2_weth_wbtc_lp = uniswap_v2_pool_manager.get_pool(
        token_addresses=(
            MAINNET_WETH_ADDRESS,
            MAINNET_WBTC_ADDRESS,
        )
    )
    assert super_new_v2_weth_wbtc_lp is not new_v2_weth_wbtc_lp
    assert super_new_v2_weth_wbtc_lp is not v2_weth_wbtc_lp

    assert AllPools(chain_id=1).get(v2_weth_wbtc_lp.address) is super_new_v2_weth_wbtc_lp
    len(AllPools(chain_id=1))
    AllPools(chain_id=1)[super_new_v2_weth_wbtc_lp.address]
    del AllPools(chain_id=1)[super_new_v2_weth_wbtc_lp.address]


def test_pools_from_token_path(ethereum_full_node_web3: Web3) -> None:
    set_web3(ethereum_full_node_web3)

    uniswap_v2_pool_manager = UniswapV2LiquidityPoolManager(
        factory_address=MAINNET_UNISWAP_V2_FACTORY_ADDRESS
    )

    assert get_v2_pools_from_token_path(
        tx_path=[MAINNET_WBTC_ADDRESS, MAINNET_WETH_ADDRESS],
        pool_manager=uniswap_v2_pool_manager,
    ) == [
        uniswap_v2_pool_manager.get_pool(
            token_addresses=(MAINNET_WBTC_ADDRESS, MAINNET_WETH_ADDRESS)
        ),
    ]


def test_same_block(fork_mainnet_archive: AnvilFork):
    _BLOCK = 18493777
    fork_mainnet_archive.reset(block_number=_BLOCK)
    set_web3(fork_mainnet_archive.w3)

    uniswap_v2_pool_manager = UniswapV2LiquidityPoolManager(
        factory_address=MAINNET_UNISWAP_V2_FACTORY_ADDRESS
    )

    v2_heyjoe_weth_lp = uniswap_v2_pool_manager.get_pool(
        pool_address="0xC928CF054fE73CaB56d753BA4b508da0F82FABFD",
        state_block=_BLOCK,
    )

    del uniswap_v2_pool_manager[v2_heyjoe_weth_lp]
    del AllPools(chain_id=1)[v2_heyjoe_weth_lp]

    new_v2_heyjoe_weth_lp = uniswap_v2_pool_manager.get_pool(
        pool_address="0xC928CF054fE73CaB56d753BA4b508da0F82FABFD",
        state_block=_BLOCK,
    )

    assert v2_heyjoe_weth_lp is not new_v2_heyjoe_weth_lp
