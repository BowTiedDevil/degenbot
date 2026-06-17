"""Test helpers for constructing Erc20Token/EtherPlaceholder companions.

Under ADR-005, ``Erc20Token`` is a Python companion over a ``PyErc20Token``
handle (token metadata is owned by the Rust ``Bot``). These helpers register
the metadata and wrap the handle, mirroring what ``Erc20Builder`` does in
production. They are idempotent (``Bot::register_token`` asserts on a
duplicate address) so they're safe to call with a shared module-level
``PyBot`` across test functions.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.erc20 import Erc20Token, EtherPlaceholder

if TYPE_CHECKING:
    from degenbot.degenbot_rs import PyBot


def make_erc20(
    py_bot: PyBot,
    address: str,
    *,
    name: str,
    symbol: str,
    decimals: int,
    chain_id: int = 1,
    oracle_address: str | None = None,
    state_cache_depth: int = 8,
) -> Erc20Token:
    """Register token metadata in the Rust ``Bot`` and wrap the ``PyErc20Token`` handle.

    Idempotent: if the address is already registered, reuses the existing
    ``PyErc20Token`` (``Bot::register_token`` asserts on duplicates).
    """
    py_token = py_bot.get_token(address)
    if py_token is None:
        py_token = py_bot.register_token(address, name, symbol, decimals, chain_id)
    return Erc20Token(py_token, oracle_address=oracle_address, state_cache_depth=state_cache_depth)


def make_ether_placeholder(
    py_bot: PyBot,
    address: str,
    *,
    chain_id: int = 1,
    state_cache_depth: int = 8,
) -> EtherPlaceholder:
    """Register ETH-placeholder metadata and wrap the handle as an ``EtherPlaceholder``."""
    py_token = py_bot.get_token(address)
    if py_token is None:
        py_token = py_bot.register_token(address, "Ether Placeholder", "ETH", 18, chain_id)
    return EtherPlaceholder(py_token, state_cache_depth=state_cache_depth)
