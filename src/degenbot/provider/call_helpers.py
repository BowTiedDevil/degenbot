"""Low-level RPC call helpers.

Thin wrappers around ProviderAdapter.call() that handle
ABI encoding/decoding and block identifier resolution.
"""

from collections.abc import Sequence
from typing import Any

import eth_abi.abi
from eth_typing import ChecksumAddress
from eth_utils.crypto import keccak
from web3.types import BlockIdentifier

from degenbot.provider import ProviderAdapter
from degenbot.provider.interface import AsyncProviderAdapter


def encode_function_calldata(
    function_prototype: str, function_arguments: Sequence[Any] | None
) -> bytes:
    """Encode the calldata to execute a call to the given function prototype, with ordered arguments.

    The resulting bytes array will include the 4-byte function selector, followed by the
    ABI-encoded arguments.
    """
    if function_arguments is None:
        function_arguments = ()

    return keccak(text=function_prototype)[:4] + eth_abi.abi.encode(
        types=extract_argument_types_from_function_prototype(function_prototype),
        args=function_arguments,
    )


def extract_argument_types_from_function_prototype(function_prototype: str) -> list[str]:
    """Extract the argument types from the function prototype.

    e.g. the argument types for the prototype 'function(address,uint256)' are ['address','uint256']
    """
    if function_args := function_prototype[
        function_prototype.find("(") + 1 : function_prototype.find(")") :
    ]:
        return function_args.split(",")

    return []


def raw_call(
    provider: ProviderAdapter,
    address: ChecksumAddress,
    calldata: bytes,
    return_types: list[str],
    block_identifier: BlockIdentifier | None = None,
) -> tuple[Any, ...]:
    """Perform an eth_call at the given address and return the decoded response.

    Args:
        provider: ProviderAdapter instance
        address: Contract address to call
        calldata: Encoded function call data
        return_types: ABI types for decoding the response
        block_identifier: Block number or tag for the call

    Returns:
        Decoded response as tuple

    """
    block_num = block_identifier if isinstance(block_identifier, int) else None
    return eth_abi.abi.decode(
        types=return_types,
        data=provider.call(to=address, data=calldata, block=block_num),
    )


async def async_raw_call(
    provider: AsyncProviderAdapter,
    address: ChecksumAddress,
    calldata: bytes,
    return_types: list[str],
    block_identifier: BlockIdentifier | None = None,
) -> tuple[Any, ...]:
    """Perform an async eth_call at the given address and return the decoded response.

    Same as raw_call but uses await provider.call().
    """
    block_num = block_identifier if isinstance(block_identifier, int) else None
    return eth_abi.abi.decode(
        types=return_types,
        data=await provider.call(to=address, data=calldata, block=block_num),
    )
