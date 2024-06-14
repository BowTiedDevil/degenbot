from fractions import Fraction
from itertools import cycle
from typing import Callable, Iterable, Iterator, List, Tuple

import eth_abi.abi
from eth_typing import ChecksumAddress
from eth_utils.address import to_checksum_address
from hexbytes import HexBytes
from web3 import Web3


def decode_v3_path(path: bytes) -> List[ChecksumAddress | int]:
    """
    Decode the `path` bytes used by the Uniswap V3 Router/Router2 contracts. `path` is a
    close-packed encoding of 20 byte pool addresses, interleaved with 3 byte fees.
    """
    ADDRESS_BYTES = 20
    FEE_BYTES = 3

    def _extract_address(chunk: bytes) -> ChecksumAddress:
        return to_checksum_address(chunk)

    def _extract_fee(chunk: bytes) -> int:
        return int.from_bytes(chunk, byteorder="big")

    if any(
        [
            len(path) < ADDRESS_BYTES + FEE_BYTES + ADDRESS_BYTES,
            len(path) % (ADDRESS_BYTES + FEE_BYTES) != ADDRESS_BYTES,
        ]
    ):  # pragma: no cover
        raise ValueError("Invalid path.")

    chunk_length_and_decoder_function: Iterator[
        Tuple[
            int,
            Callable[
                [bytes],
                ChecksumAddress | int,
            ],
        ]
    ] = cycle(
        [
            (ADDRESS_BYTES, _extract_address),
            (FEE_BYTES, _extract_fee),
        ]
    )

    path_offset = 0
    decoded_path: List[ChecksumAddress | int] = []
    while path_offset != len(path):
        byte_length, extraction_func = next(chunk_length_and_decoder_function)
        chunk = HexBytes(path[path_offset : path_offset + byte_length])
        decoded_path.append(extraction_func(chunk))
        path_offset += byte_length

    return decoded_path


def exchange_rate_from_sqrt_price_x96(sqrt_price_x96: int) -> Fraction:
    # ref: https://blog.uniswap.org/uniswap-v3-math-primer
    return Fraction(sqrt_price_x96**2, 2**192)


def generate_v3_pool_address(
    token_addresses: Iterable[str],
    fee: int,
    factory_or_deployer_address: str,
    init_hash: str,
) -> ChecksumAddress:
    """
    Generate the deterministic pool address from the token addresses and fee.

    Adapted from https://github.com/Uniswap/v3-periphery/blob/main/contracts/libraries/PoolAddress.sol
    """

    token_addresses = sorted([address.lower() for address in token_addresses])

    return to_checksum_address(
        Web3.keccak(
            HexBytes(0xFF)
            + HexBytes(factory_or_deployer_address)
            + Web3.keccak(
                eth_abi.abi.encode(
                    types=("address", "address", "uint24"),
                    args=(*token_addresses, fee),
                )
            )
            + HexBytes(init_hash)
        )[-20:]  # last 20 bytes of the keccak hash becomes the pool address
    )
