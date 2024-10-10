from eth_utils.address import to_checksum_address
from web3 import Web3

from degenbot.aerodrome.managers import AerodromeV2PoolManager, AerodromeV3PoolManager
from degenbot.config import set_web3

BASE_AERODROME_V2_FACTORY = to_checksum_address("0x420DD381b31aEf6683db6B902084cB0FFECe40Da")
BASE_AERODROME_V3_FACTORY = to_checksum_address("0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A")
BASE_AERO_TOKEN = to_checksum_address("0x940181a94a35a4569e4529a3cdfb74e38fd98631")
BASE_WETH_TOKEN = to_checksum_address("0x4200000000000000000000000000000000000006")
BASE_AERO_WETH_V2_POOL = to_checksum_address("0x7f670f78B17dEC44d5Ef68a48740b6f8849cc2e6")
BASE_AERO_WETH_V3_POOL = to_checksum_address("0x82321f3BEB69f503380D6B233857d5C43562e2D0")
BASE_AERO_WETH_V3_POOL_TICK_SPACING = 200


def test_create_base_chain_aerodrome_managers(base_full_node_web3: Web3):
    set_web3(base_full_node_web3)

    aerodrome_v2_pool_manager = AerodromeV2PoolManager(factory_address=BASE_AERODROME_V2_FACTORY)
    assert aerodrome_v2_pool_manager._factory_address == BASE_AERODROME_V2_FACTORY

    aerodrome_v3_pool_manager = AerodromeV3PoolManager(factory_address=BASE_AERODROME_V3_FACTORY)
    assert aerodrome_v3_pool_manager._factory_address == BASE_AERODROME_V3_FACTORY

    aerodrome_v2_lp = aerodrome_v2_pool_manager.get_pool(BASE_AERO_WETH_V2_POOL)
    aerodrome_v2_lp_from_tokens = aerodrome_v2_pool_manager.get_pool_from_tokens(
        token_addresses=(BASE_WETH_TOKEN, BASE_AERO_TOKEN),
        stable=False,
    )
    assert aerodrome_v2_lp is aerodrome_v2_lp_from_tokens

    aerodrome_v3_lp = aerodrome_v3_pool_manager.get_pool(BASE_AERO_WETH_V3_POOL)
    aerodrome_v3_lp_from_tokens_and_tick_spacing = (
        aerodrome_v3_pool_manager.get_pool_from_tokens_and_tick_spacing(
            token_addresses=(BASE_WETH_TOKEN, BASE_AERO_TOKEN),
            tick_spacing=BASE_AERO_WETH_V3_POOL_TICK_SPACING,
        )
    )
    assert aerodrome_v3_lp is aerodrome_v3_lp_from_tokens_and_tick_spacing
