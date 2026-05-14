"""Crypto pool parameter detection for Curve pools.

Detects whether a Curve pool is a crypto pool by checking fee_gamma().
If fee_gamma > 0, fetches related parameters (mid_fee, out_fee, gamma,
offpeg_fee_multiplier). Each sub-parameter is individually tolerant of
reverts since not all crypto pools expose all of them.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import eth_abi.abi

from degenbot.curve.detection.types import CryptoDetectionResult
from degenbot.functions import encode_function_calldata

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.provider.interface import ProviderAdapter


def detect_crypto_params(
    provider: ProviderAdapter,
    pool_address: ChecksumAddress,
    *,
    block_identifier: int,
) -> CryptoDetectionResult:
    """Detect crypto pool parameters.

    A pool is identified as crypto if fee_gamma() > 0.
    All parameters default to None for non-crypto pools.
    """
    fee_gamma: int | None = None
    mid_fee: int | None = None
    out_fee: int | None = None
    gamma: int | None = None
    offpeg_fee_multiplier: int | None = None

    try:
        fee_gamma_result = provider.call_raw(
            {
                "to": pool_address,
                "data": encode_function_calldata(
                    function_prototype="fee_gamma()",
                    function_arguments=[],
                ),
            },
            block=block_identifier,
        )
        (fee_gamma_val,) = eth_abi.abi.decode(types=["uint256"], data=fee_gamma_result)
        if fee_gamma_val > 0:
            fee_gamma = fee_gamma_val

            # Fetch related crypto pool parameters
            try:
                mid_fee_result = provider.call_raw(
                    {
                        "to": pool_address,
                        "data": encode_function_calldata(
                            function_prototype="mid_fee()",
                            function_arguments=[],
                        ),
                    },
                    block=block_identifier,
                )
                (mid_fee_val,) = eth_abi.abi.decode(types=["uint256"], data=mid_fee_result)
                mid_fee = mid_fee_val
            except Exception:
                pass

            try:
                out_fee_result = provider.call_raw(
                    {
                        "to": pool_address,
                        "data": encode_function_calldata(
                            function_prototype="out_fee()",
                            function_arguments=[],
                        ),
                    },
                    block=block_identifier,
                )
                (out_fee_val,) = eth_abi.abi.decode(types=["uint256"], data=out_fee_result)
                out_fee = out_fee_val
            except Exception:
                pass

            try:
                gamma_result = provider.call_raw(
                    {
                        "to": pool_address,
                        "data": encode_function_calldata(
                            function_prototype="gamma()",
                            function_arguments=[],
                        ),
                    },
                    block=block_identifier,
                )
                (gamma_val,) = eth_abi.abi.decode(types=["uint256"], data=gamma_result)
                gamma = gamma_val
            except Exception:
                pass
    except Exception:
        pass

    # Fetch offpeg_fee_multiplier (used by some lending/crypto pools)
    try:
        offpeg_result = provider.call_raw(
            {
                "to": pool_address,
                "data": encode_function_calldata(
                    function_prototype="offpeg_fee_multiplier()",
                    function_arguments=[],
                ),
            },
            block=block_identifier,
        )
        (offpeg_val,) = eth_abi.abi.decode(types=["uint256"], data=offpeg_result)
        offpeg_fee_multiplier = offpeg_val
    except Exception:
        pass

    return CryptoDetectionResult(
        is_crypto=fee_gamma is not None,
        fee_gamma=fee_gamma,
        mid_fee=mid_fee,
        out_fee=out_fee,
        gamma=gamma,
        offpeg_fee_multiplier=offpeg_fee_multiplier,
    )
