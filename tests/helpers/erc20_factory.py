"""Test helpers for constructing Erc20Token/NativeToken companions.

Under ADR-005, ``Erc20Token`` is a Python companion over a ``Erc20Token``
handle (token metadata is owned by the Rust ``Bot``). These helpers register
the metadata and wrap the handle, mirroring what ``Erc20Builder`` does in
production. They are idempotent (``Bot::register_token`` asserts on a
duplicate address), so a re-registration inside one test is a no-op — but the
engine is still the caller's to own: pass the per-test one (a fixture), never a
module-level shared handle, whose registrations would leak across tests.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.erc20 import Erc20Token, NativeToken

if TYPE_CHECKING:
    from tests.helpers.seam import Bot


def make_erc20(
    py_bot: Bot,
    address: str,
    *,
    name: str,
    symbol: str,
    decimals: int,
    chain_id: int = 1,
    oracle_address: str | None = None,
) -> Erc20Token:
    """Register token metadata in the Rust ``Bot`` and wrap the ``Erc20Token`` handle.

    Idempotent: if the address is already registered, reuses the existing
    ``Erc20Token`` (``Bot::register_token`` asserts on duplicates).
    """
    py_token = py_bot.get_token(address)
    if py_token is None:
        py_token = py_bot.register_token(address, name, symbol, decimals, chain_id)
    return Erc20Token.from_handle(
        py_token,
        oracle_address=oracle_address,
    )


def make_native_token(
    py_bot: Bot,
    *,
    chain_id: int = 1,
) -> NativeToken:
    """Wrap the chain's conceptual ``NativeToken`` handle (ADR-097).

    Metadata comes from the core chain-facts manifest; there is no address
    and nothing to register.
    """
    return NativeToken.from_handle(py_bot.native_token(chain_id))
