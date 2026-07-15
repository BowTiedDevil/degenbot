"""Provider construction from a :class:`DegenbotConfig` RPC endpoint.

The canonical URL→provider factory (ADR-006 D5: one Bot per chain). The
factory enforces that the RPC it connects to returns the configured
``chain_id`` — a mismatch raises :class:`ValueError` (fail-fast on a
misconfigured endpoint, before any pool/token I/O runs).

The factory is alloy-only: it builds a Rust-backed
:class:`~degenbot._ffi.AlloyProvider` (HTTP/WS/IPC, scheme-detected)
unconditionally and verifies ``eth_chainId`` against ``chain_id``.

Lives in ``degenbot.provider`` (the lib layer) so both ``Bot.__init__`` and
the CLI can reach it without a lib→cli reverse dependency. ``cli/utils.py``
re-exports it for backward compatibility.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.config import DegenbotConfig, _init_config, resolve_http_rpc_uri

if TYPE_CHECKING:
    from degenbot.provider import AlloyProvider, AsyncAlloyProvider


def get_provider_from_config(
    *,
    chain_id: int,
    config: DegenbotConfig | None = None,
) -> AlloyProvider:
    """Build an :class:`AlloyProvider` for ``chain_id`` from the resolved RPC entry.

    Resolves the HTTP/IPC endpoint through the standard cascade
    (:func:`degenbot.config.resolve_http_rpc_uri`): CLI arg > OS env
    ``DEGENBOT_RPC_HTTP_CHAINID_{cid}`` > caller fallback > config.toml
    ``rpc[cid]`` > raise. This is the single resolution path shared by the
    library, the ``degenbot`` click CLI, and the backrun example (see
    ``docs/migration-guides/rpc-uri-cascade.md``), so a plain ``export`` in the
    devcontainer takes effect here too.

    Constructs an :class:`~degenbot._ffi.AlloyProvider` over the
    resolved endpoint (HTTP/WS/IPC, detected by scheme), then **enforces**
    the connected RPC's ``eth_chainId`` equals ``chain_id`` — raises
    :class:`ValueError` on mismatch (fail-fast).

    Args:
        chain_id: The chain ID to get a provider for
        config: Optional config override; loaded from disk if not provided (also
            passed to the resolver as the config.toml layer)

    Returns:
        An AlloyProvider over the resolved RPC endpoint.

    Raises:
        ValueError: If no RPC is configured for ``chain_id`` (raised as
            :class:`RpcNotConfiguredError`, a ``ValueError`` subclass), or the
            connected RPC's chain ID does not match ``chain_id``.

    """
    if config is None:
        config = _init_config()
    from degenbot.provider import AlloyProvider

    endpoint = resolve_http_rpc_uri(chain_id, config=config)
    alloy = AlloyProvider(endpoint)
    actual = alloy.get_chain_id()
    if actual != chain_id:
        msg = (
            f"The chain ID ({actual}) at endpoint {endpoint} does not match "
            f"the chain ID ({chain_id}) defined in the config file."
        )
        raise ValueError(msg)
    return alloy


async def get_async_provider_from_config(
    *,
    chain_id: int,
    config: DegenbotConfig | None = None,
) -> AsyncAlloyProvider:
    """Build an :class:`AsyncAlloyProvider` for ``chain_id`` from the resolved RPC entry.

    Async counterpart of :func:`get_provider_from_config`. Resolves the
    HTTP/IPC endpoint through the standard cascade
    (:func:`degenbot.config.resolve_http_rpc_uri`) — same precedence as the
    sync factory — constructs an async Alloy provider, then **enforces** the
    connected RPC's ``eth_chainId`` equals ``chain_id`` via an awaited
    ``get_chain_id()`` (async providers cannot read it synchronously) —
    raises :class:`ValueError` on mismatch (fail-fast, ADR-006 D5).

    Returns:
        An AsyncAlloyProvider over the resolved RPC endpoint.

    Raises:
        ValueError: If no RPC is configured for ``chain_id`` (raised as
            :class:`RpcNotConfiguredError`, a ``ValueError`` subclass), or the
            connected RPC's chain ID does not match ``chain_id``.

    """
    if config is None:
        config = _init_config()
    from degenbot.provider import AsyncAlloyProvider

    endpoint = resolve_http_rpc_uri(chain_id, config=config)
    alloy = await AsyncAlloyProvider.create(endpoint)
    actual = await alloy.get_chain_id()
    if actual != chain_id:
        msg = (
            f"The chain ID ({actual}) at endpoint {endpoint} does not match "
            f"the configured chain ID ({chain_id})."
        )
        raise ValueError(msg)
    return alloy
