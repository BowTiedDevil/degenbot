"""Lending token detection for Curve pools.

Detects cTokens (via isCToken()) and yTokens (via token()) in a Curve
pool, and computes precision multiplier overrides for cTokens based on
the underlying token's decimals.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import eth_abi.abi

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.detection.types import LendingDetectionResult
from degenbot.functions import encode_function_calldata

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.provider.interface import ProviderAdapter
    from degenbot.tokens.erc20_token import Erc20Token


def detect_lending_tokens(
    provider: ProviderAdapter,
    pool_address: ChecksumAddress,
    token_addresses: tuple[ChecksumAddress, ...],
    tokens: tuple[Erc20Token, ...],
    *,
    block_identifier: int,
) -> LendingDetectionResult:
    """Detect lending tokens (cTokens, yTokens) and compute precision multipliers.

    For lending tokens, precision_multipliers must be based on the
    UNDERLYING token decimals, not the wrapped token decimals.
    e.g., cDAI has 8 decimals, but DAI has 18, so precision_multiplier = 10^0 = 1
    cUSDC has 8 decimals, but USDC has 6, so precision_multiplier = 10^12
    """
    use_lending: list[bool] = []
    precision_multiplier_overrides: dict[int, int] = {}  # index -> override value

    for idx, token_addr in enumerate(token_addresses):
        is_lending = False
        checksummed_addr = get_checksum_address(token_addr)

        # Check if token is a cToken using isCToken()
        try:
            is_ctoken_result = provider.call_raw(
                {
                    "to": checksummed_addr,
                    "data": encode_function_calldata(
                        function_prototype="isCToken()",
                        function_arguments=[],
                    ),
                },
                block=block_identifier,
            )
            (is_c,) = eth_abi.abi.decode(types=["bool"], data=is_ctoken_result)
            if is_c:
                is_lending = True
                # cToken: get underlying token decimals via underlying() method
                try:
                    underlying_result = provider.call_raw(
                        {
                            "to": checksummed_addr,
                            "data": encode_function_calldata(
                                function_prototype="underlying()",
                                function_arguments=[],
                            ),
                        },
                        block=block_identifier,
                    )
                    (underlying_addr,) = eth_abi.abi.decode(
                        types=["address"], data=underlying_result
                    )
                    underlying_addr = get_checksum_address(underlying_addr)
                    # Fetch underlying decimals
                    try:
                        underlying_dec_result = provider.call_raw(
                            {
                                "to": underlying_addr,
                                "data": encode_function_calldata(
                                    function_prototype="decimals()",
                                    function_arguments=[],
                                ),
                            },
                            block=block_identifier,
                        )
                        (underlying_dec,) = eth_abi.abi.decode(
                            types=["uint8"], data=underlying_dec_result
                        )
                        # Override precision_multiplier to use underlying decimals
                        precision_multiplier_overrides[idx] = 10 ** (18 - underlying_dec)
                    except Exception:
                        pass
                except Exception:
                    pass
        except Exception:
            pass

        # Check if token is a yToken (has token() method returning underlying)
        if not is_lending:
            try:
                ytoken_result = provider.call_raw(
                    {
                        "to": checksummed_addr,
                        "data": encode_function_calldata(
                            function_prototype="token()",
                            function_arguments=[],
                        ),
                    },
                    block=block_identifier,
                )
                (underlying_addr,) = eth_abi.abi.decode(types=["address"], data=ytoken_result)
                # Verify the underlying is a valid address (not zero)
                if int(underlying_addr, 16) != 0:
                    is_lending = True
                    # yToken: typically has same decimals as underlying, no override needed
            except Exception:
                pass

        use_lending.append(is_lending)

    # Compute precision multipliers tuple if overrides exist
    precision_multipliers: tuple[int, ...] | None = None
    if precision_multiplier_overrides or any(use_lending):
        precision_multipliers = tuple(
            precision_multiplier_overrides.get(i, 10 ** (18 - tokens[i].decimals))
            for i in range(len(tokens))
        )

    return LendingDetectionResult(
        use_lending=tuple(use_lending),
        precision_multipliers=precision_multipliers,
    )
