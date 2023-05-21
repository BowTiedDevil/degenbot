from itertools import cycle
from typing import Iterable, List, Optional, Union

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


def generate_v3_pool_address(
    token_addresses: Iterable[str],
    fee: int,
    factory_address: Optional[str] = None,
    init_hash: Optional[str] = None,
) -> str:
    """
    Generate the deterministic pool address from the token addresses and fee.

    Adapted from https://github.com/Uniswap/v3-periphery/blob/main/contracts/libraries/PoolAddress.sol
    """

    if factory_address is None:
        factory_address = "0x1F98431c8aD98523631AE4a59f267346ea31F984"

    if init_hash is None:
        init_hash = "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"

    token_addresses = sorted([address.lower() for address in token_addresses])

    return Web3.toChecksumAddress(
        Web3.keccak(
            hexstr="0xff"
            + factory_address[2:]
            + Web3.keccak(
                eth_abi.encode(
                    ["address", "address", "uint24"],
                    [*token_addresses, fee],
                )
            ).hex()[2:]
            + init_hash[2:]
        )[12:]
    )
