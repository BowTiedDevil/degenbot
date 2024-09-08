__all__ = (
    "get_web3",
    "set_web3",
    "web3_connection_manager",
)

import web3
from eth_typing import ChainId

from .exceptions import DegenbotError


class Web3ConnectionManager:
    def __init__(self) -> None:
        self.connections: dict[int, web3.Web3] = dict()
        self._default_chain_id: int = ChainId.ETH

    def register(self, w3: web3.Web3) -> None:
        if not self.connections:
            self._default_chain_id = w3.eth.chain_id
        self.connections[w3.eth.chain_id] = w3

    def get(self, chain_id: int) -> web3.Web3 | None:
        return self.connections.get(chain_id)


def get_web3(chain_id: int | None = None) -> web3.Web3:
    if chain_id is None:
        chain_id = web3_connection_manager._default_chain_id
    if (w3 := web3_connection_manager.get(chain_id)) is not None:
        return w3
    raise DegenbotError(f"A Web3 instance has not been registered for chain ID {chain_id}.")


def set_web3(w3: web3.Web3) -> None:
    if w3.is_connected() is False:
        raise DegenbotError("Web3 object is not connected.")
    web3_connection_manager.register(w3)


web3_connection_manager = Web3ConnectionManager()
