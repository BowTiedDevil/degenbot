"""LP token address discovery for Curve pools.

Finds the LP token address associated with a Curve pool by querying
the Curve registry and factory via get_lp_token().
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.abi import AbiDecodeError, decode
from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS as _ZERO_ADDRESS
from degenbot.exceptions import RpcError
from degenbot.provider.call_helpers import encode_function_calldata

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.bot import PyBotIo


def find_lp_token(
    io: PyBotIo,
    pool_address: ChecksumAddress,
    *,
    registry_addresses: tuple[ChecksumAddress, ...],
    block_identifier: int,
) -> ChecksumAddress | None:
    """Find the LP token address for a Curve pool.

    Iterates the Curve registry and factory addresses, calling
    get_lp_token(address) until a non-zero address is returned.
    Returns None if no LP token is found.

    Returns:
        The computed value.

    """
    for registry_address in registry_addresses:
        try:
            lp_token_result = io.call_raw(
                {
                    "to": registry_address,
                    "data": encode_function_calldata(
                        function_prototype="get_lp_token(address)",
                        function_arguments=[pool_address],
                    ),
                },
                block=block_identifier,
            )
            (lp_token_addr,) = decode(["address"], lp_token_result)
            if lp_token_addr != _ZERO_ADDRESS:
                return get_checksum_address(lp_token_addr)
        except (RpcError, AbiDecodeError, ValueError):
            continue

    return None
