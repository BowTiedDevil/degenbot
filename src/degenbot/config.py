__all__ = (
    "get_web3",
    "set_web3",
    "connection_manager",
)

import web3

from .exceptions import DegenbotValueError


class ConnectionManager:
    def __init__(self) -> None:
        self.connections: dict[int, web3.Web3] = dict()
        self._default_chain_id: int | None = None

    def get_web3(self, chain_id: int) -> web3.Web3:
        try:
            return self.connections[chain_id]
        except KeyError:
            raise DegenbotValueError("Chain ID does not have a registered Web3 instance.") from None

    def register_web3(self, w3: web3.Web3) -> None:
        if w3.is_connected() is False:
            raise DegenbotValueError("Web3 instance is not connected.")
        self.connections[w3.eth.chain_id] = w3

    def set_default_chain(self, chain_id: int) -> None:
        self._default_chain_id = chain_id

    @property
    def default_chain_id(self) -> int:
        if self._default_chain_id is None:
            raise DegenbotValueError("A default chain ID has not been provided.")
        return self._default_chain_id


def get_web3() -> web3.Web3:
    if connection_manager._default_chain_id is None:
        raise DegenbotValueError("A default Web3 instance has not been registered.") from None
    else:
        return connection_manager.get_web3(chain_id=connection_manager.default_chain_id)


def set_web3(w3: web3.Web3, optimize_middleware: bool = True) -> None:
    if w3.is_connected() is False:
        raise DegenbotValueError("Web3 instance is not connected.")
    connection_manager.register_web3(w3, optimize_middleware=optimize_middleware)
    connection_manager.set_default_chain(w3.eth.chain_id)


connection_manager = ConnectionManager()
