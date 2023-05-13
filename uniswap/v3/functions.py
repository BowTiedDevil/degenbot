from typing import Tuple, List, Union
from itertools import cycle

import eth_abi
from web3 import Web3


def decode_v3_path(path: bytes) -> List[Union[str, int]]:
    """
    Decode the `path` byte string used by the Uniswap V3 Router/Router2 contracts.
    `path` is a close-packed encoding of pool addresses and fees.
    """
    path_pos = 0
    decoded_path: List[Union[str, int]] = []
    # read alternating 20 and 3 byte chunks from the encoded path,
    # store each address (hex) and fee (int)
    for byte_length in cycle((20, 3)):
        if byte_length == 20:
            address = path[path_pos : path_pos + byte_length].hex()
            decoded_path.append(address)
        elif byte_length == 3:
            fee = int(
                path[path_pos : path_pos + byte_length].hex(),
                16,
            )
            decoded_path.append(fee)

        path_pos += byte_length

        if path_pos == len(path):
            break

    return decoded_path


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
