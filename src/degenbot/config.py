from web3 import Web3

from .exceptions import DegenbotError
from .logging import logger

_web3: Web3


def get_web3() -> Web3:
    try:
        return _web3
    except NameError:
        raise DegenbotError("A Web3 instance has not been provided.") from None


def set_web3(w3: Web3) -> None:
    if w3.is_connected() is False:
        raise DegenbotError("Web3 object is not connected.")

    logger.info(f"Connected to Web3 provider {w3.provider}")

    global _web3
    _web3 = w3
