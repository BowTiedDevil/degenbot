"""Provider construction over a cascade-resolved endpoint (ADR-062 D8).

The canonical chain-bound URL-to-provider factory. The endpoint and the chain
both come from the installed typed config, and the chain binding itself is the
Rust core's: the factory hands the resolved ``chain_id`` to the provider and the
core reads ``eth_chainId`` once at construction, refusing a misconfigured
endpoint before any pool/token I/O runs. A Rust consumer that constructs a
provider directly gets the same refusal (see
``degenbot_rpc::provider::AlloyProvider::for_chain``). What is left here is
translating the core's refusal into this package's error hierarchy.

Lives in ``degenbot.provider`` (the lib layer) so both ``Bot.__init__`` and the
CLI can reach it without a lib->cli reverse dependency.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot._ffi import ChainMismatchError
from degenbot.config import resolve_chain_id, resolve_http_rpc_uri
from degenbot.exceptions.base import DegenbotValueError

if TYPE_CHECKING:
    from degenbot.provider import AlloyProvider, AsyncAlloyProvider


class ChainIdentityMismatchError(DegenbotValueError, ValueError):
    """The core's chain-binding refusal, re-homed into this package's hierarchy.

    The check is the core's invariant; only the error class is re-homed here.
    It stays a :class:`ValueError` as well as a
    :class:`~degenbot.exceptions.base.DegenbotValueError`, so a caller that
    already handled a misconfigured endpoint keeps catching it exactly as it
    caught the core's own refusal.
    """


def _chain_mismatch_message(exc: ChainMismatchError) -> str:
    """Render the core's chain-binding refusal as this package's message.

    The check is the core's invariant; the factory only re-homes the refusal so
    a caller catching ``DegenbotValueError`` keeps catching it. The
    disagreement is rendered from the typed attributes, not parsed out of the
    core's message.

    Args:
        exc: The refusal raised by the core at provider construction.

    Returns:
        The message naming the endpoint and both chain ids.

    """
    return (
        f"the endpoint {exc.endpoint} serves chain {exc.actual}, but the session "
        f"targets chain {exc.expected}: refusing to start a provider against the wrong chain"
    )


def get_provider_from_config(
    *,
    chain_id: int | str | None = None,
    node: str | None = None,
) -> AlloyProvider:
    """Build a chain-bound :class:`AlloyProvider` for the session chain.

    The endpoint resolves through the request scope of the four-layer cascade
    and the chain through the chain-id cascade, both from the installed typed
    config; ``chain_id``/``node`` are the explicit override layer when given.

    The constructed provider is BOUND to that chain: the Rust core verifies the
    endpoint's ``eth_chainId`` once, and its refusal is translated into a
    :class:`DegenbotValueError` here.

    Args:
        chain_id: The explicit chain override; resolved from the config layers
            when absent.
        node: The explicit endpoint override, classified by its own value.

    Returns:
        A chain-bound AlloyProvider over the resolved RPC endpoint.

    Raises:
        ChainIdentityMismatchError: When the endpoint serves another chain than
            the session targets. It is both a
            :class:`~degenbot.exceptions.base.DegenbotValueError` and a
            :class:`ValueError`.

    """
    from degenbot.provider import AlloyProvider

    session_chain_id = resolve_chain_id(chain_id)
    endpoint = resolve_http_rpc_uri(session_chain_id, node=node)
    try:
        return AlloyProvider(endpoint, chain_id=session_chain_id)
    except ChainMismatchError as exc:
        raise ChainIdentityMismatchError(message=_chain_mismatch_message(exc)) from exc


async def get_async_provider_from_config(
    *,
    chain_id: int | str | None = None,
    node: str | None = None,
) -> AsyncAlloyProvider:
    """Build a chain-bound :class:`AsyncAlloyProvider` for the session chain.

    Async counterpart of :func:`get_provider_from_config`: the same resolution,
    the same core check, awaited on the caller's event loop.

    Args:
        chain_id: The explicit chain override; resolved from the config layers
            when absent.
        node: The explicit endpoint override, classified by its own value.

    Returns:
        A chain-bound AsyncAlloyProvider over the resolved RPC endpoint.

    Raises:
        ChainIdentityMismatchError: When the endpoint serves another chain than
            the session targets. It is both a
            :class:`~degenbot.exceptions.base.DegenbotValueError` and a
            :class:`ValueError`.

    """
    from degenbot.provider import AsyncAlloyProvider

    session_chain_id = resolve_chain_id(chain_id)
    endpoint = resolve_http_rpc_uri(session_chain_id, node=node)
    try:
        return await AsyncAlloyProvider.create(endpoint, chain_id=session_chain_id)
    except ChainMismatchError as exc:
        raise ChainIdentityMismatchError(message=_chain_mismatch_message(exc)) from exc
