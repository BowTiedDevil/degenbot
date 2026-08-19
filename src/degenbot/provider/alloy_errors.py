"""Translate Alloy (Rust) provider errors into degenbot-owned exceptions.

The Rust :class:`AlloyProvider` flattens every non-timeout, non-connection RPC
error into a ``RuntimeError("Provider error: ...")`` — see the
``ProviderError`` → ``PyErr`` conversion in ``rust/src/errors.rs``, which loses
the structured JSON-RPC error ``data`` and keeps only ``code`` + a formatted
string. An alloy ``eth_call`` revert is therefore surfaced as a bare
``RuntimeError``.

That asymmetry is a real bug: probing code throughout the codebase
(:mod:`degenbot.builders.type_resolution`,
:mod:`degenbot.aave.analysis`) catches the provider-layer revert type to mean
"this method is not implemented by the contract." Under the alloy backend a
reverted probe (e.g. ``factory()`` on a Curve pool, which has no such method)
propagated the bare ``RuntimeError`` and broke pool construction before the
Curve fallback in :meth:`Bot.build_pool` could trigger.

This module restores parity at the adapter seam: an alloy ``eth_call`` revert
is re-raised as the degenbot-owned :exc:`~degenbot.exceptions.ContractLogicError`
(an :exc:`~degenbot.exceptions.RpcError` subclass) so probe sites that catch
``RpcError`` behave identically across the Alloy and web3 backends. The web3
adapter performs the symmetric conversion at its own ``call``/``call_raw``
seam (catching web3's ``ContractLogicError`` and re-raising the degenbot type).

.. note::

   Revert detection here is message-based because the Rust layer discards the
   structured JSON-RPC error data before it crosses the FFI boundary. A
   future Rust-side ``ProviderError::ExecutionReverted`` variant that
   preserves ``error.data`` (the ``0x08c379a0`` / ``0x4e487b71`` revert
   selectors, or the raw revert reason) would let this classification be
   structural rather than string-based.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.exceptions import ContractLogicError

if TYPE_CHECKING:
    from degenbot.types.chain import Address

# Markers indicating an EVM execution revert inside the Rust provider's
# formatted error string. Solidity >=0.8 surfaces as the phrase
# "execution reverted" (Geth, Infura, Alchemy) or "error code 3: execution
# reverted" (Anvil); older Vyper/generic reverts surface as "Reverted". The
# ABI revert selectors are matched as a fallback for nodes that echo the
# revert reason data inline.
_REVERT_MARKERS: tuple[str, ...] = (
    "execution reverted",
    "reverted",
    "0x08c379a0",  # Error(string)
    "0x4e487b71",  # Panic(uint256)
)


def is_alloy_revert(exc: BaseException) -> bool:
    """Return whether a Rust Alloy ``RuntimeError`` represents an EVM revert.

    Returns:
        ``True`` only if ``exc`` is a ``RuntimeError`` whose message carries a
        recognized revert marker; ``False`` otherwise (so genuine transport /
        server errors propagate unchanged).

    """
    if not isinstance(exc, RuntimeError):
        return False
    message = str(exc).lower()
    return any(marker in message for marker in _REVERT_MARKERS)


def alloy_revert_error(
    exc: RuntimeError,
    *,
    to: Address | str,
) -> ContractLogicError:
    """Build a degenbot :exc:`ContractLogicError` mirroring an Alloy revert.

    Raises the degenbot-owned :exc:`~degenbot.exceptions.ContractLogicError`
    (an :exc:`~degenbot.exceptions.RpcError` subclass) so the existing probe
    sites — which now catch :exc:`RpcError` rather than ``Web3Exception`` —
    behave identically across the Alloy and web3 backends. The web3 adapter
    performs the symmetric conversion at its own ``call``/``call_raw`` seam.

    Returns:
        A :exc:`ContractLogicError` whose message embeds the original alloy
        revert text, suitable for raising at the adapter seam.

    """
    return ContractLogicError(f"eth_call to {to} reverted: {exc}")
