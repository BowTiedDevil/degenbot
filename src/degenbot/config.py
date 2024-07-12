import web3
import web3.providers

from .exceptions import DegenbotError
from .logging import logger

_web3: web3.Web3
_endpoint_uri: str
_ipc_path: str


def get_web3() -> web3.Web3:
    global _web3

    try:
        return _web3
    except NameError:
        try:
            _endpoint_uri
        except NameError:
            _web3 = web3.Web3(web3.IPCProvider(_ipc_path))
            return _web3
        else:
            if "http://" in _endpoint_uri or "https://" in _endpoint_uri:
                _web3 = web3.Web3(web3.HTTPProvider(_endpoint_uri))
                return _web3
            elif "ws://" in _endpoint_uri or "wss://" in _endpoint_uri:
                _web3 = web3.Web3(web3.WebsocketProvider(_endpoint_uri))
                return _web3
            raise DegenbotError("A Web3 instance has not been provided.") from None


def set_web3(w3: web3.Web3) -> None:
    if w3.is_connected() is False:
        raise DegenbotError("Web3 object is not connected.")

    logger.info(f"Connected to Web3 provider {w3.provider}")

    global _web3
    global _endpoint_uri
    global _ipc_path

    _web3 = w3
    match w3.provider:
        case web3.HTTPProvider() | web3.WebsocketProvider():
            _endpoint_uri = w3.provider.endpoint_uri  # type: ignore[assignment]
        case web3.IPCProvider():
            _ipc_path = w3.provider.ipc_path  # type: ignore[has-type]
        case _:
            raise TypeError("Unsupported provider.")
