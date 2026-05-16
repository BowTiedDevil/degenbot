"""Metapool detection for Curve pools.

Detects whether a Curve pool is a metapool by querying the Curve
registry/factory for is_meta(). If detected, resolves the base pool
address and underlying coin addresses.

The detector only detects — it returns MetapoolDetectionResult. The
builder handles the recursive base pool build and token construction.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import eth_abi.abi
from eth_abi.exceptions import DecodingError
from web3.exceptions import Web3Exception

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.detection.types import MetapoolDetectionResult
from degenbot.provider.call_helpers import encode_function_calldata

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.provider.interface import ProviderAdapter


# 3Crv LP token — used as fallback base pool detection
_THREE_CRV_LP_TOKEN_ADDRESS = "0x6c3F90f043a72FA612Cbac8115ee7e52bDE6E490"
_THREE_CRV_POOL_ADDRESS = get_checksum_address("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7")


def detect_metapool(
    provider: ProviderAdapter,
    pool_address: ChecksumAddress,
    token_addresses: tuple[ChecksumAddress, ...],
    *,
    registry_addresses: tuple[ChecksumAddress, ...],
    block_identifier: int,
) -> MetapoolDetectionResult:
    """Detect whether a Curve pool is a metapool and resolve base pool info.

    Checks Curve registry and factory via is_meta(), then resolves
    base pool address and underlying coins.
    """
    for registry_address in registry_addresses:
        try:
            is_meta_result = provider.call_raw(
                {
                    "to": registry_address,
                    "data": encode_function_calldata(
                        function_prototype="is_meta(address)",
                        function_arguments=[pool_address],
                    ),
                },
                block=block_identifier,
            )
            (is_meta,) = eth_abi.abi.decode(types=["bool"], data=is_meta_result)
            if not is_meta:
                # Try next registry
                continue

            # Get base pool address from the pool contract itself
            base_pool_address = _resolve_base_pool_address(
                provider, pool_address, token_addresses, registry_address, block_identifier
            )

            # Get underlying coins from registry
            underlying_coins_result = provider.call_raw(
                {
                    "to": registry_address,
                    "data": encode_function_calldata(
                        function_prototype="get_underlying_coins(address)",
                        function_arguments=[pool_address],
                    ),
                },
                block=block_identifier,
            )
            underlying_addresses = eth_abi.abi.decode(
                types=["address[8]"], data=underlying_coins_result
            )[0]

            # Filter out zero addresses
            tokens_underlying: list[ChecksumAddress] = []
            for addr in underlying_addresses:
                if int(addr, 16) == 0:
                    break
                tokens_underlying.append(get_checksum_address(addr))

            # Found metapool info, stop checking other registries
            return MetapoolDetectionResult(
                is_meta=True,
                base_pool_address=base_pool_address,
                tokens_underlying=tuple(tokens_underlying),
            )
        except (Web3Exception, DecodingError, ValueError):
            continue

    return MetapoolDetectionResult(
        is_meta=False,
        base_pool_address=None,
        tokens_underlying=None,
    )


def _resolve_base_pool_address(
    provider: ProviderAdapter,
    pool_address: ChecksumAddress,
    token_addresses: tuple[ChecksumAddress, ...],
    registry_address: ChecksumAddress,
    block_identifier: int,
) -> ChecksumAddress | None:
    """Resolve the base pool address, trying multiple methods in order."""
    # Try base_pool() on the pool contract
    try:
        base_pool_result = provider.call_raw(
            {
                "to": pool_address,
                "data": encode_function_calldata(
                    function_prototype="base_pool()",
                    function_arguments=[],
                ),
            },
            block=block_identifier,
        )
        (base_pool_address,) = eth_abi.abi.decode(types=["address"], data=base_pool_result)
        return get_checksum_address(base_pool_address)
    except (Web3Exception, DecodingError, ValueError):
        pass

    # Try get_base_pool() on the registry
    try:
        base_pool_result = provider.call_raw(
            {
                "to": registry_address,
                "data": encode_function_calldata(
                    function_prototype="get_base_pool(address)",
                    function_arguments=[pool_address],
                ),
            },
            block=block_identifier,
        )
        (base_pool_address,) = eth_abi.abi.decode(types=["address"], data=base_pool_result)
        return get_checksum_address(base_pool_address)
    except (Web3Exception, DecodingError, ValueError):
        pass

    # Last resort: if the pool's second token is the 3Crv LP token,
    # use the tripool as the base pool
    if (
        len(token_addresses) >= 2
        and token_addresses[1].lower() == _THREE_CRV_LP_TOKEN_ADDRESS.lower()
    ):
        return _THREE_CRV_POOL_ADDRESS

    return None
