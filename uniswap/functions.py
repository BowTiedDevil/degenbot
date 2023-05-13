from typing import Tuple

import eth_abi
from web3 import Web3


def generate_v3_pool_address(token_addresses: Tuple[str, str], fee: int):
    """
    Generate the deterministic pool address from the token addresses and fee.

    Adapted from https://github.com/Uniswap/v3-periphery/blob/main/contracts/libraries/PoolAddress.sol
    """

    V3_FACTORY = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
    POOL_INIT_CODE_HASH = (
        "0xE34F199B19B2B4F47F68442619D555527D244F78A3297EA89325F843F87B8B54"
    )

    token_addresses = (min(token_addresses), max(token_addresses))

    pool_address = Web3.toChecksumAddress(
        Web3.keccak(
            hexstr=(
                "ff"
                + V3_FACTORY[2:]
                + Web3.keccak(
                    eth_abi.encode(
                        ["address", "address", "uint24"],
                        [*token_addresses, fee],
                    )
                ).hex()[2:]
                + POOL_INIT_CODE_HASH[2:]
            )
        )[-20:].hex()
    )
    return pool_address
