import sys
from typing import Callable, Optional

from web3 import Web3

from .logging import logger

_web3: Web3


def get_web3() -> Web3:
    return _web3


def set_web3(w3: Web3):
    connected_method: Optional[Callable] = None

    for method_name in ("is_connected", "isConnected"):  # pragma: no cover
        try:
            connected_method = getattr(w3, method_name)
        except AttributeError:
            pass
        else:
            break

    if connected_method is None:  # pragma: no cover
        raise ValueError("Provided web3 object has no 'connected' method")

    if connected_method() is False:  # pragma: no cover
        raise ValueError("Web3 object is not connected.")

    logger.info(f"Connected to Web3 provider {w3.provider}")

    global _web3
    _web3 = w3


if "brownie" in sys.modules:  # pragma: no cover
    logger.info("Brownie detected. Degenbot will attempt to use its Web3 object...")
    from brownie import web3 as brownie_web3  # type: ignore[import]

    set_web3(brownie_web3)

else:
    logger.info("Attempting to use Web3 AutoProvider")
    try:
        set_web3(Web3())
    except Exception as e:
        logger.error(e)
        logger.info(
            "Could not establish Web3 connection using AutoProvider. Provide a Web3 instance to set_web3() before use"
        )
