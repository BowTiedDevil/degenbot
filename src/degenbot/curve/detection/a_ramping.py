"""A ramping parameter detection for Curve pools.

Detects whether a Curve pool has active A coefficient ramping by
probing initial_A(), initial_A_time(), future_A(), and future_A_time().
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.abi import AbiDecodeError, decode
from degenbot.curve.detection.types import ARampingResult
from degenbot.exceptions import RpcError
from degenbot.provider.call_helpers import encode_function_calldata

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.bot import PyBotIo


def detect_a_ramping(
    io: PyBotIo,
    pool_address: ChecksumAddress,
    *,
    block_identifier: int,
) -> ARampingResult:
    """Detect A coefficient ramping parameters.

    Not all pools support initial_A()/future_A() — they're optional.
    If any call reverts, returns has_ramping=False.

    Returns:
        The computed value.

    """
    try:  # noqa:PLW0717
        initial_a_result = io.call_raw(
            {
                "to": pool_address,
                "data": encode_function_calldata(
                    function_prototype="initial_A()",
                    function_arguments=[],
                ),
            },
            block=block_identifier,
        )
        (initial_a,) = decode(["uint256"], initial_a_result)

        initial_a_time_result = io.call_raw(
            {
                "to": pool_address,
                "data": encode_function_calldata(
                    function_prototype="initial_A_time()",
                    function_arguments=[],
                ),
            },
            block=block_identifier,
        )
        (initial_a_time,) = decode(["uint256"], initial_a_time_result)

        future_a_result = io.call_raw(
            {
                "to": pool_address,
                "data": encode_function_calldata(
                    function_prototype="future_A()",
                    function_arguments=[],
                ),
            },
            block=block_identifier,
        )
        (future_a,) = decode(["uint256"], future_a_result)

        future_a_time_result = io.call_raw(
            {
                "to": pool_address,
                "data": encode_function_calldata(
                    function_prototype="future_A_time()",
                    function_arguments=[],
                ),
            },
            block=block_identifier,
        )
        (future_a_time,) = decode(["uint256"], future_a_time_result)
    except (RpcError, AbiDecodeError, ValueError):
        return ARampingResult(
            initial_a=None,
            initial_a_time=None,
            future_a=None,
            future_a_time=None,
            has_ramping=False,
        )

    return ARampingResult(
        initial_a=initial_a,
        initial_a_time=initial_a_time,
        future_a=future_a,
        future_a_time=future_a_time,
        has_ramping=True,
    )
