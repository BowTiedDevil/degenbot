import os
from pathlib import Path
from typing import TYPE_CHECKING

from pydantic import HttpUrl, WebsocketUrl
from web3 import HTTPProvider, IPCProvider, JSONBaseProvider, LegacyWebSocketProvider, Web3

from degenbot.config import CONFIG_FILE, settings
from degenbot.connection.connection_manager import _fast_decode_rpc_response
from degenbot.provider import AlloyProvider
from degenbot.provider.interface import ProviderAdapter


def _get_use_alloy_from_env() -> bool:
    env_value = os.getenv("DEGENBOT_USE_ALLOY_PROVIDER", "").lower()
    return env_value in {"true", "1", "yes", "on"}


def get_web3_from_config(
    *, chain_id: int, optimize: bool = True, use_alloy: bool | None = None
) -> ProviderAdapter:
    """Get a ProviderAdapter for the given chain ID.

    Args:
        chain_id: The chain ID to get a provider for
        optimize: Whether to optimize Web3 (removes middleware, uses fast JSON decoding)
        use_alloy: Force use of AlloyProvider (default: from env var DEGENBOT_USE_ALLOY_PROVIDER)

    Returns:
        A ProviderAdapter wrapping either Web3 or AlloyProvider
    """
    if use_alloy is None:
        use_alloy = _get_use_alloy_from_env()
    match endpoint := settings.rpc.get(chain_id):
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
        w3.provider.decode_rpc_response = _fast_decode_rpc_response  # type:ignore[method-assign]

    return ProviderAdapter.from_web3(w3)
