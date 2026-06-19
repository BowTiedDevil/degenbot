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

from pydantic import HttpUrl, WebsocketUrl
from ujson import loads as ujson_loads
from web3 import HTTPProvider, IPCProvider, JSONBaseProvider, LegacyWebSocketProvider, Web3

from degenbot.config import CONFIG_FILE, DegenbotConfig, _init_config
from degenbot.degenbot_rs import AlloyProvider, AsyncAlloyProvider
from degenbot.provider.async_adapter import AsyncProviderAdapter
from degenbot.provider.sync_adapter import ProviderAdapter

if TYPE_CHECKING:
    from web3.types import RPCResponse


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
    """Build a :class:`ProviderAdapter` for ``chain_id`` from the config's RPC entry.

    Maps the configured endpoint (HTTP/WS URL or IPC path) to a Web3 or Alloy
    backend, then **enforces** the connected RPC's ``eth_chainId`` equals
    ``chain_id`` — raises :class:`ValueError` on mismatch (fail-fast).

    Args:
        chain_id: The chain ID to get a provider for
        optimize: Whether to optimize Web3 (removes middleware, uses fast JSON decoding)
        use_alloy: Force use of AlloyProvider (default: from env var DEGENBOT_USE_ALLOY_PROVIDER)
        config: Optional config override; loaded from disk if not provided

    Returns:
        A ProviderAdapter wrapping either Web3 or AlloyProvider

    Raises:
        ValueError: If no RPC is configured for ``chain_id``, or the connected
            RPC's chain ID does not match ``chain_id``.

    """
    if use_alloy is None:
        use_alloy = _get_use_alloy_from_env()
    if config is None:
        config = _init_config()
    match endpoint := config.rpc.get(chain_id):
        case HttpUrl():
            if use_alloy:
                alloy = AlloyProvider(str(endpoint))
                return ProviderAdapter.from_alloy(alloy)
            w3 = Web3(HTTPProvider(str(endpoint)))
        case WebsocketUrl():
            if use_alloy:
                alloy = AlloyProvider(str(endpoint))
                return ProviderAdapter.from_alloy(alloy)
            w3 = Web3(LegacyWebSocketProvider(str(endpoint)))
        case Path():
            if use_alloy:
                alloy = AlloyProvider(str(endpoint))
                return ProviderAdapter.from_alloy(alloy)
            w3 = Web3(IPCProvider(str(endpoint)))
        case None:
            msg = f"Chain ID {chain_id} does not have an RPC defined in config file {CONFIG_FILE}"
            raise ValueError(msg)

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
    """Build an :class:`AsyncProviderAdapter` for ``chain_id`` from the config's RPC entry.

    Async counterpart of :func:`get_provider_from_config`. Constructs an async
    Alloy provider from the configured endpoint, then **enforces** the
    connected RPC's ``eth_chainId`` equals ``chain_id`` via an awaited
    ``get_chain_id()`` (async providers cannot read it synchronously) —
    raises :class:`ValueError` on mismatch (fail-fast, ADR-006 D5).

    Returns:
        An AsyncProviderAdapter wrapping a Rust AsyncAlloyProvider.

    Raises:
        ValueError: If no RPC is configured for ``chain_id``, or the connected
            RPC's chain ID does not match ``chain_id``.

    """
    if config is None:
        config = _init_config()
    endpoint = config.rpc.get(chain_id)
    if endpoint is None:
        msg = f"Chain ID {chain_id} does not have an RPC defined in config file {CONFIG_FILE}"
        raise ValueError(msg)
    alloy = await AsyncAlloyProvider.create(str(endpoint))
    adapter = AsyncProviderAdapter.from_alloy(alloy)
    actual = await adapter.get_chain_id()
    if actual != chain_id:
        msg = (
            f"The chain ID ({actual}) at endpoint {endpoint} does not match "
            f"the configured chain ID ({chain_id})."
        )
        raise ValueError(msg)
    return adapter
