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

from degenbot.config import resolve_chain_id, resolve_http_rpc_uri
from degenbot.exceptions.base import DegenbotValueError
from degenbot.provider import ChainMismatchError
from degenbot.provider.async_provider import AsyncAlloyProvider
from degenbot.provider.sync import AlloyProvider

if TYPE_CHECKING:
    from collections.abc import Callable
    from typing import Any


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
    # The refusal's typed attributes are set by the Rust core and are absent
    # from the generated stub (the stubtest-allowlist class of generator gaps).
    return (
        f"the endpoint {exc.endpoint} serves chain {exc.actual}, but the session "  # ty: ignore[unresolved-attribute]
        f"targets chain {exc.expected}: refusing to start a provider against the wrong chain"  # ty: ignore[unresolved-attribute]
    )


def get_provider_from_config(
    *,
    chain_id: int | str | None = None,
    node: str | None = None,
    resolve_uri: Callable[..., str] = resolve_http_rpc_uri,
    provider_factory: Callable[..., AlloyProvider] | None = None,
    chain_mismatch_error: type[ChainMismatchError] = ChainMismatchError,
) -> AlloyProvider:
    """Build a chain-bound :class:`AlloyProvider` for the session chain.

    The endpoint resolves through the request scope of the four-layer cascade
    and the chain through the chain-id cascade, both from the installed typed
    config; ``chain_id``/``node`` are the explicit override layer when given.

    The constructed provider is BOUND to that chain: the Rust core verifies the
    endpoint's ``eth_chainId`` once, and its refusal is translated into a
    :class:`DegenbotValueError` here.

    ``resolve_uri``/``provider_factory``/``chain_mismatch_error`` are the DI
    seams (tests inject a recording resolver, a stand-in constructor, and a
    constructible stand-in for the PyO3 refusal class); omitted kwargs keep
    the production bindings.

    Args:
        chain_id: The explicit chain override; resolved from the config layers
            when absent.
        node: The explicit endpoint override, classified by its own value.
        resolve_uri: The endpoint resolver called as
            ``resolve_uri(session_chain_id, node=node)``.
        provider_factory: The provider constructor called as
            ``provider_factory(endpoint, chain_id=session_chain_id)``.
        chain_mismatch_error: The exception class the core's refusal is
            caught as before re-homing.

    Returns:
        A chain-bound AlloyProvider over the resolved RPC endpoint.

    Raises:
        ChainIdentityMismatchError: When the endpoint serves another chain than
            the session targets. It is both a
            :class:`~degenbot.exceptions.base.DegenbotValueError` and a
            :class:`ValueError`.

    """
    session_chain_id = resolve_chain_id(chain_id)
    endpoint = resolve_uri(session_chain_id, node=node)
    if provider_factory is None:
        provider_factory = AlloyProvider
    try:
        return provider_factory(endpoint, chain_id=session_chain_id)
    except chain_mismatch_error as exc:
        raise ChainIdentityMismatchError(message=_chain_mismatch_message(exc)) from exc


async def get_async_provider_from_config(
    *,
    chain_id: int | str | None = None,
    node: str | None = None,
    resolve_uri: Callable[..., str] = resolve_http_rpc_uri,
    provider_factory: Callable[..., Any] | None = None,
    chain_mismatch_error: type[ChainMismatchError] = ChainMismatchError,
) -> AsyncAlloyProvider:
    """Build a chain-bound :class:`AsyncAlloyProvider` for the session chain.

    Async counterpart of :func:`get_provider_from_config`: the same resolution,
    the same core check, awaited on the caller's event loop. The DI seams are
    the sync factory's; an omitted ``provider_factory`` resolves to
    ``AsyncAlloyProvider.create``.

    Args:
        chain_id: The explicit chain override; resolved from the config layers
            when absent.
        node: The explicit endpoint override, classified by its own value.
        resolve_uri: The endpoint resolver called as
            ``resolve_uri(session_chain_id, node=node)``.
        provider_factory: The awaited provider constructor called as
            ``provider_factory(endpoint, chain_id=session_chain_id)``.
        chain_mismatch_error: The exception class the core's refusal is
            caught as before re-homing.

    Returns:
        A chain-bound AsyncAlloyProvider over the resolved RPC endpoint.

    Raises:
        ChainIdentityMismatchError: When the endpoint serves another chain than
            the session targets. It is both a
            :class:`~degenbot.exceptions.base.DegenbotValueError` and a
            :class:`ValueError`.

    """
    session_chain_id = resolve_chain_id(chain_id)
    endpoint = resolve_uri(session_chain_id, node=node)
    if provider_factory is None:
        provider_factory = AsyncAlloyProvider.create
    try:
        return await provider_factory(endpoint, chain_id=session_chain_id)
    except chain_mismatch_error as exc:
        raise ChainIdentityMismatchError(message=_chain_mismatch_message(exc)) from exc
