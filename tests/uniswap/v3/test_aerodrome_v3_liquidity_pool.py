from eth_utils.address import to_checksum_address

from degenbot import Erc20Token
from degenbot.config import set_web3
from degenbot.fork.anvil_fork import AnvilFork
from degenbot.uniswap.v3_liquidity_pool import AerodromeV3Pool

AERODROME_V3_FACTORY_ADDRESS = to_checksum_address("0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A")
CBETH_WETH_POOL_ADDRESS = to_checksum_address("0x47cA96Ea59C13F72745928887f84C9F52C3D7348")
WETH_CONTRACT_ADDRESS = to_checksum_address("0x4200000000000000000000000000000000000006")
CBETH_CONTRACT_ADDRESS = to_checksum_address("0x2Ae3F1Ec7F1F5012CFEab0185bfc7aa3cf0DEc22")


def test_aerodrome_v3_pool_creation(fork_base: AnvilFork) -> None:
    set_web3(fork_base.w3)

    AerodromeV3Pool(address=CBETH_WETH_POOL_ADDRESS)
    AerodromeV3Pool(
        address=CBETH_WETH_POOL_ADDRESS,
        factory_address=AERODROME_V3_FACTORY_ADDRESS,
    )
    AerodromeV3Pool(
        address=CBETH_WETH_POOL_ADDRESS,
        tokens=[
            Erc20Token(WETH_CONTRACT_ADDRESS),
            Erc20Token(CBETH_CONTRACT_ADDRESS),
        ],
    )
    assert (
        AerodromeV3Pool(
            address=CBETH_WETH_POOL_ADDRESS, tick_bitmap={}, tick_data={}
        ).sparse_liquidity_map
        is False
    )
