"""Provider construction from a :class:`DegenbotConfig` RPC endpoint.

The canonical URL→provider factory (ADR-006 D5: one Bot per chain). The chain
binding itself is the Rust core's: the factory resolves the endpoint, hands
the resolved ``chain_id`` to the provider, and the core reads ``eth_chainId``
once at construction — refusing a misconfigured endpoint with a
:class:`ValueError` before any pool/token I/O runs. A Rust consumer that
constructs a provider directly gets the same refusal (see
``degenbot_rpc::provider::AlloyProvider::for_chain``).

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
    """Build a chain-bound :class:`AlloyProvider` for ``chain_id``.

    Resolves the HTTP/IPC endpoint through the standard cascade
    (:func:`degenbot.config.resolve_http_rpc_uri`): CLI arg > OS env
    ``DEGENBOT_RPC_HTTP_CHAINID_{cid}`` > caller fallback > config.toml
    ``rpc[cid]`` > raise. This is the single resolution path shared by the
    library, the ``degenbot`` click CLI, and the settlement-arbitrage example
    (see the rpc-uri-cascade migration guide, removed in the stale-docs
    cleanup `71ec78b2`), so a plain ``export`` in the devcontainer takes
    effect here too.

    The constructed provider is BOUND to ``chain_id``: the Rust core verifies
    the endpoint's ``eth_chainId`` once, and the core's
    :class:`~degenbot.exceptions.base.DegenbotValueError`-shaped
    :class:`ValueError` (naming both chain ids) propagates unchanged.

    Args:
        chain_id: The chain ID to get a provider for
        config: Optional config override; loaded from disk if not provided (also
            passed to the resolver as the config.toml layer)

    The binding check raises :class:`ValueError` (naming both chain ids) when
    the endpoint serves another chain, and the cascade raises
    :class:`RpcNotConfiguredError` — itself a :class:`ValueError` — when no
    layer supplied an endpoint.

    Returns:
        A chain-bound AlloyProvider over the resolved RPC endpoint.

    """
    if config is None:
        config = _init_config()
    from degenbot.provider import AlloyProvider

    endpoint = resolve_http_rpc_uri(chain_id, config=config)
    return AlloyProvider(endpoint, chain_id=chain_id)


async def get_async_provider_from_config(
    *,
    chain_id: int,
    config: DegenbotConfig | None = None,
) -> AsyncAlloyProvider:
    """Build a chain-bound :class:`AsyncAlloyProvider` for ``chain_id``.

    Async counterpart of :func:`get_provider_from_config`. Resolves the
    HTTP/IPC endpoint through the same cascade, then binds the provider to
    ``chain_id`` — the same one-round-trip core check the sync factory and
    every Rust consumer get, awaited on the caller's event loop.

    The binding check raises :class:`ValueError` (naming both chain ids) when
    the endpoint serves another chain, and the cascade raises
    :class:`RpcNotConfiguredError` — itself a :class:`ValueError` — when no
    layer supplied an endpoint.

    Returns:
        A chain-bound AsyncAlloyProvider over the resolved RPC endpoint.

    """
    if config is None:
        config = _init_config()
    from degenbot.provider import AsyncAlloyProvider

    endpoint = resolve_http_rpc_uri(chain_id, config=config)
    return await AsyncAlloyProvider.create(endpoint, chain_id=chain_id)
