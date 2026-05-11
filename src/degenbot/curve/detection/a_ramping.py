"""A ramping parameter detection for Curve pools.

Detects whether a Curve pool has active A coefficient ramping by
probing initial_A(), initial_A_time(), future_A(), and future_A_time().
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

import eth_abi.abi

from degenbot.curve.detection.types import ARampingResult
from degenbot.functions import encode_function_calldata

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress


def detect_a_ramping(
    w3: Any,
    pool_address: ChecksumAddress,
    *,
    block_identifier: int,
) -> ARampingResult:
    """Detect A coefficient ramping parameters.

    Not all pools support initial_A()/future_A() — they're optional.
    If any call reverts, returns has_ramping=False.
    """
    try:
        initial_a_result = w3.eth.call(
            {
                "to": pool_address,
                "data": encode_function_calldata(
                    function_prototype="initial_A()",
                    function_arguments=[],
                ),
            },
            block_identifier=block_identifier,
        )
        (initial_a,) = eth_abi.abi.decode(types=["uint256"], data=initial_a_result)

        initial_a_time_result = w3.eth.call(
            {
                "to": pool_address,
                "data": encode_function_calldata(
                    function_prototype="initial_A_time()",
                    function_arguments=[],
                ),
            },
            block_identifier=block_identifier,
        )
        (initial_a_time,) = eth_abi.abi.decode(types=["uint256"], data=initial_a_time_result)

        future_a_result = w3.eth.call(
            {
                "to": pool_address,
                "data": encode_function_calldata(
                    function_prototype="future_A()",
                    function_arguments=[],
                ),
            },
            block_identifier=block_identifier,
        )
        (future_a,) = eth_abi.abi.decode(types=["uint256"], data=future_a_result)

        future_a_time_result = w3.eth.call(
            {
                "to": pool_address,
                "data": encode_function_calldata(
                    function_prototype="future_A_time()",
                    function_arguments=[],
                ),
            },
            block_identifier=block_identifier,
        )
        (future_a_time,) = eth_abi.abi.decode(types=["uint256"], data=future_a_time_result)
    except Exception:
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
