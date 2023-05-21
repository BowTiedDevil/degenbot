from typing import Iterable, Optional

import eth_abi.packed
from web3 import Web3


def generate_v2_pool_address(
    token_addresses: Iterable[str],
    factory_address: Optional[str] = None,
    init_hash: Optional[str] = None,
) -> str:
    """
    Generate the deterministic pool address from the token addresses.

    Adapted from https://github.com/Uniswap/universal-router/blob/deployed-commit/contracts/modules/uniswap/v2/UniswapV2Library.sol
    """

    if factory_address is None:
        factory_address = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"

    if init_hash is None:
        init_hash = "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"

    token_addresses = sorted([address.lower() for address in token_addresses])

    return Web3.toChecksumAddress(
        Web3.keccak(
            hexstr="0xff"
            + factory_address[2:]
            + Web3.keccak(
                eth_abi.packed.encode_packed(
                    ["address", "address"],
                    [*token_addresses],
                )
            ).hex()[2:]
            + init_hash[2:]
        )[12:]
    )
