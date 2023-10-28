from typing import Optional, Callable
from web3 import Web3

_web3: Optional[Web3] = None


def get_web3() -> Optional[Web3]:
    return _web3


def set_web3(w3: Web3):
    connected_method: Optional[Callable] = None

    for method_name in ("is_connected", "isConnected"):
        try:
            connected_method = getattr(w3, method_name)
        except AttributeError:
            pass
        else:
            break

    if connected_method is None:
        raise ValueError("Provided web3 object has no 'connected' method")

    if connected_method() is False:
        raise ValueError("Web3 object is not connected.")

    global _web3
    _web3 = w3
