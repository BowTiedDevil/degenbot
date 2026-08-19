"""Low-level RPC call helpers.

Thin wrappers around AlloyProvider.call() that handle
ABI encoding/decoding and block identifier resolution.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from degenbot.abi import decode, encode
from degenbot.crypto import function_selector

if TYPE_CHECKING:
    from collections.abc import Sequence

    from degenbot.provider import AlloyProvider
    from degenbot.types.chain import ChecksummedAddress
    from degenbot.types.rpc_types import BlockIdentifier


def encode_function_calldata(
    function_prototype: str,
    function_arguments: Sequence[Any] | None,
) -> bytes:
    """Encode calldata for the given function prototype with ordered arguments.

    The resulting bytes array will include the 4-byte function selector, followed by the
    ABI-encoded arguments.

    Returns:
        The encoded calldata bytes.

    """
    if function_arguments is None:
        function_arguments = ()

    return function_selector(function_prototype) + encode(
        types=extract_argument_types_from_function_prototype(function_prototype),
        args=function_arguments,
    )


def extract_argument_types_from_function_prototype(function_prototype: str) -> list[str]:
    """Extract the argument types from the function prototype.

    e.g. the argument types for the prototype 'function(address,uint256)'
    are ['address','uint256']

    Returns:
        The list of ABI type strings.

    """
    if function_args := function_prototype[
        function_prototype.find("(") + 1 : function_prototype.find(")") :
    ]:
        return function_args.split(",")

    return []


def raw_call(
    provider: AlloyProvider,
    address: ChecksummedAddress,
    calldata: bytes,
    return_types: list[str],
    block_identifier: BlockIdentifier | None = None,
) -> tuple[Any, ...]:
    """Perform an eth_call at the given address and return the decoded response.

    Args:
        provider: AlloyProvider instance
        address: Contract address to call
        calldata: Encoded function call data
        return_types: ABI types for decoding the response
        block_identifier: Block number or tag for the call

    Returns:
        Decoded response as tuple

    """
    block_num = block_identifier if isinstance(block_identifier, int) else None
    return decode(return_types, provider.call(address, calldata, block=block_num))
