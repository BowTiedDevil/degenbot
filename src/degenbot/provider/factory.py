"""Provider construction from a :class:`DegenbotConfig` RPC endpoint.

The canonical URL→:class:`ProviderAdapter` factory (ADR-006 D5: one Bot per
chain). The factory enforces that the RPC it connects to returns the
configured ``chain_id`` — a mismatch raises :class:`ValueError` (fail-fast on
a misconfigured endpoint, before any pool/token I/O runs).

Lives in ``degenbot.provider`` (the lib layer) so both ``Bot.__init__`` and
the CLI can reach it without a lib→cli reverse dependency. ``cli/utils.py``
re-exports it for backward compatibility.
"""

from __future__ import annotations

import os
from json import JSONDecodeError
from pathlib import Path
from typing import TYPE_CHECKING, cast

from ujson import loads as ujson_loads
from web3 import HTTPProvider, IPCProvider, JSONBaseProvider, LegacyWebSocketProvider, Web3

from degenbot.config import DegenbotConfig, _init_config, resolve_http_rpc_uri
from degenbot.degenbot_rs import AlloyProvider, AsyncAlloyProvider
from degenbot.provider.async_adapter import AsyncProviderAdapter
from degenbot.provider.sync_adapter import ProviderAdapter

if TYPE_CHECKING:
    from degenbot.types.rpc_types import RPCResponse


def _fast_decode_rpc_response(raw_response: bytes) -> RPCResponse:
    """Decode the JSON-RPC response using ujson.

    Returns:
        The decoded RPC response.

    Raises:
        JSONDecodeError: If the response is not valid JSON.

    """
    try:
        return cast("RPCResponse", ujson_loads(raw_response))
    except ValueError:
        # Re-raise as a dummy JSONDecodeError so web3py's exception handling works as intended.
        msg = "JSON failure"
        raise JSONDecodeError(msg, "[]", 0) from None


def _get_use_alloy_from_env() -> bool:
    env_value = os.getenv("DEGENBOT_USE_ALLOY_PROVIDER", "").lower()
    return env_value in {"true", "1", "yes", "on"}


def get_provider_from_config(
    *,
    chain_id: int,
    optimize: bool = True,
    use_alloy: bool | None = None,
    config: DegenbotConfig | None = None,
) -> ProviderAdapter:
    """Build a :class:`ProviderAdapter` for ``chain_id`` from the resolved RPC entry.

    Resolves the HTTP/IPC endpoint through the standard cascade
    (:func:`degenbot.config.resolve_http_rpc_uri`): CLI arg > OS env
    ``DEGENBOT_RPC_HTTP_CHAINID_{cid}`` > caller fallback > config.toml
    ``rpc[cid]`` > raise. This is the single resolution path shared by the
    library, the ``degenbot`` click CLI, and the backrun example (see
    ``docs/migration-guides/rpc-uri-cascade.md``), so a plain ``export`` in the
    devcontainer takes effect here too.

    Maps the resolved endpoint (HTTP/WS URL or IPC path, detected by scheme) to
    a Web3 or Alloy backend, then **enforces** the connected RPC's
    ``eth_chainId`` equals ``chain_id`` — raises :class:`ValueError` on mismatch
    (fail-fast).

    Args:
        chain_id: The chain ID to get a provider for
        optimize: Whether to optimize Web3 (removes middleware, uses fast JSON decoding)
        use_alloy: Force use of AlloyProvider (default: from env var DEGENBOT_USE_ALLOY_PROVIDER)
        config: Optional config override; loaded from disk if not provided (also
            passed to the resolver as the config.toml layer)

    Returns:
        A ProviderAdapter wrapping either Web3 or AlloyProvider

    Raises:
        ValueError: If no RPC is configured for ``chain_id`` (raised as
            :class:`RpcNotConfiguredError`, a ``ValueError`` subclass), or the
            connected RPC's chain ID does not match ``chain_id``.

    """
    if use_alloy is None:
        use_alloy = _get_use_alloy_from_env()
    if config is None:
        config = _init_config()
    endpoint = resolve_http_rpc_uri(chain_id, config=config)
    scheme = endpoint.lower()
    if scheme.startswith(("http://", "https://")):
        if use_alloy:
            alloy = AlloyProvider(endpoint)
            return ProviderAdapter.from_alloy(alloy)
        w3 = Web3(HTTPProvider(endpoint))
    elif scheme.startswith(("ws://", "wss://")):
        if use_alloy:
            alloy = AlloyProvider(endpoint)
            return ProviderAdapter.from_alloy(alloy)
        w3 = Web3(LegacyWebSocketProvider(endpoint))
    else:
        # Anything that isn't an http(s)/ws(s) URI is treated as an IPC socket
        # path (config.toml ``rpc[cid]`` accepts a ``Path``).
        ipc_path = Path(endpoint).expanduser().absolute()
        if use_alloy:
            alloy = AlloyProvider(str(ipc_path))
            return ProviderAdapter.from_alloy(alloy)
        w3 = Web3(IPCProvider(str(ipc_path)))

    if w3.eth.chain_id != chain_id:
        msg = (
            f"The chain ID ({w3.eth.chain_id}) at endpoint {endpoint} does not match "
            f"the chain ID ({chain_id}) defined in the config file."
        )
        raise ValueError(msg)

    if optimize:
        # Remove all middleware and monkey-patch the JSON decoding for RPC responses
        w3.middleware_onion.clear()
        if TYPE_CHECKING:
            assert isinstance(w3.provider, JSONBaseProvider)
        w3.provider.decode_rpc_response = _fast_decode_rpc_response  # ty:ignore[invalid-assignment]

    return ProviderAdapter.from_web3(w3)


async def get_async_provider_from_config(
    *,
    chain_id: int,
    config: DegenbotConfig | None = None,
) -> AsyncProviderAdapter:
    """Build an :class:`AsyncProviderAdapter` for ``chain_id`` from the resolved RPC entry.

    Async counterpart of :func:`get_provider_from_config`. Resolves the
    HTTP/IPC endpoint through the standard cascade
    (:func:`degenbot.config.resolve_http_rpc_uri`) — same precedence as the
    sync factory — constructs an async Alloy provider, then **enforces** the
    connected RPC's ``eth_chainId`` equals ``chain_id`` via an awaited
    ``get_chain_id()`` (async providers cannot read it synchronously) —
    raises :class:`ValueError` on mismatch (fail-fast, ADR-006 D5).

    Returns:
        An AsyncProviderAdapter wrapping a Rust AsyncAlloyProvider.

    Raises:
        ValueError: If no RPC is configured for ``chain_id`` (raised as
            :class:`RpcNotConfiguredError`, a ``ValueError`` subclass), or the
            connected RPC's chain ID does not match ``chain_id``.

    """
    if config is None:
        config = _init_config()
    endpoint = resolve_http_rpc_uri(chain_id, config=config)
    alloy = await AsyncAlloyProvider.create(endpoint)
    adapter = AsyncProviderAdapter.from_alloy(alloy)
    actual = await adapter.get_chain_id()
    if actual != chain_id:
        msg = (
            f"The chain ID ({actual}) at endpoint {endpoint} does not match "
            f"the configured chain ID ({chain_id})."
        )
        raise ValueError(msg)
    return adapter
