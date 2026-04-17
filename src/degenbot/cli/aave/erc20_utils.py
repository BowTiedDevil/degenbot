"""
ERC20 token utilities for Aave CLI.

Provides functions to fetch ERC20 token metadata.
"""

import contextlib
from typing import TYPE_CHECKING

import eth_abi.abi
import eth_abi.exceptions
from eth_typing import ChecksumAddress

from degenbot.functions import encode_function_calldata, raw_call

if TYPE_CHECKING:
    from degenbot.provider.interface import ProviderAdapter


def _try_fetch_token_string(
    provider: "ProviderAdapter",
    token_address: ChecksumAddress,
    lower_func: str,
    upper_func: str,
) -> str | None:
    """
    Try to fetch a string value from an ERC20 token, with fallback to bytes32.
    """

    for func_prototype in (lower_func, upper_func):
        with contextlib.suppress(Exception):
            result = provider.call(
                to=token_address,
                data=encode_function_calldata(
                    function_prototype=func_prototype,
                    function_arguments=None,
                ),
            )

            with contextlib.suppress(eth_abi.exceptions.DecodingError):
                (value,) = eth_abi.abi.decode(types=["string"], data=result)
                return str(value)

            # Fallback for older tokens that return bytes32
            (value,) = eth_abi.abi.decode(types=["bytes32"], data=result)
            return (
                value.decode("utf-8", errors="ignore").strip("\x00")
                if isinstance(value, bytes)
                else str(value)
            )

    return None


def _try_fetch_token_uint256(
    provider: "ProviderAdapter",
    token_address: ChecksumAddress,
    lower_func: str,
    upper_func: str,
) -> int | None:
    """
    Try to fetch a uint256 value from an ERC20 token.
    """

    for func_prototype in (lower_func, upper_func):
        with contextlib.suppress(Exception):
            result: int
            (result,) = raw_call(
                w3=provider,
                address=token_address,
                calldata=encode_function_calldata(
                    function_prototype=func_prototype,
                    function_arguments=None,
                ),
                return_types=["uint256"],
            )
            return result

    return None


def _fetch_erc20_token_metadata(
    provider: "ProviderAdapter",
    token_address: ChecksumAddress,
) -> tuple[str | None, str | None, int | None]:
    """
    Fetch ERC20 token metadata (name, symbol, decimals) from the blockchain.

    Attempts to fetch using standard ERC20 function signatures, falling back
    to uppercase versions and bytes32 decoding as needed.

    Args:
        provider: ProviderAdapter for blockchain calls
        token_address: The token contract address

    Returns:
        Tuple of (name, symbol, decimals) or (None, None, None) if all fetch attempts fail
    """

    name = _try_fetch_token_string(
        provider=provider,
        token_address=token_address,
        lower_func="name()",
        upper_func="NAME()",
    )
    symbol = _try_fetch_token_string(
        provider=provider,
        token_address=token_address,
        lower_func="symbol()",
        upper_func="SYMBOL()",
    )
    decimals = _try_fetch_token_uint256(
        provider=provider,
        token_address=token_address,
        lower_func="decimals()",
        upper_func="DECIMALS()",
    )

    return name, symbol, decimals
