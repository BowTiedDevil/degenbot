"""ERC-20 token with on-chain metadata and balance tracking."""

from typing import Any

from .erc20 import UNKNOWN_DECIMALS, UNKNOWN_NAME, UNKNOWN_SYMBOL, Erc20Token
from .native_token import NativeToken

#: The public token union: a token name in a generic path/pool/account input
#: or result is either an ERC-20 contract or the chain's conceptual native
#: asset (ADR-097). A raw sentinel address is never a spelling of either.
AnyToken = Erc20Token | NativeToken


def wrap_token_handle(handle: Any) -> Erc20Token | NativeToken:
    """Wrap a token companion handle as the public ``Erc20Token | NativeToken`` union.

    A generic pool token getter hands back either handle type; wrapping
    dispatches on which one it is, so no caller needs sentinel knowledge
    (ADR-097 D1).

    Returns:
        The wrapped token companion.

    """
    from degenbot._ffi import NativeToken as _NativeTokenHandle

    if isinstance(handle, _NativeTokenHandle):
        return NativeToken.from_handle(handle)
    return Erc20Token.from_handle(handle)


def token_handle(token: Erc20Token | NativeToken) -> Any:
    """The Rust companion handle behind a token companion (the FFI wire form).

    The thin adapter over the two companion seams (`_py_token` /
    `_py_native`), so a caller with the union never needs sentinel or
    type-branch knowledge to reach the handle.

    Returns:
        The token's Rust handle.

    """
    return token._py_token if isinstance(token, Erc20Token) else token._py_native


__all__ = (
    "UNKNOWN_DECIMALS",
    "UNKNOWN_NAME",
    "UNKNOWN_SYMBOL",
    "AnyToken",
    "Erc20Token",
    "NativeToken",
    "token_handle",
    "wrap_token_handle",
)
