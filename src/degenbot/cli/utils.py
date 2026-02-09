from pathlib import Path
from typing import TYPE_CHECKING

from pydantic import HttpUrl, WebsocketUrl
from web3 import HTTPProvider, IPCProvider, JSONBaseProvider, LegacyWebSocketProvider, Web3

from degenbot.config import CONFIG_FILE, settings
from degenbot.connection.connection_manager import _fast_decode_rpc_response


def get_web3_from_config(*, chain_id: int, optimize: bool = True) -> Web3:
    match endpoint := settings.rpc.get(chain_id):
        case HttpUrl():
            w3 = Web3(HTTPProvider(str(endpoint)))
        case WebsocketUrl():
            w3 = Web3(LegacyWebSocketProvider(str(endpoint)))
        case Path():
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

    return w3
