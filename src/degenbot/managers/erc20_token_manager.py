from threading import Lock
from typing import Any

from eth_typing import ChecksumAddress
from eth_utils.address import to_checksum_address

from ..config import web3_connection_manager
from ..erc20_token import Erc20Token, EtherPlaceholder
from ..registry.all_tokens import token_registry
from ..types import AbstractManager


class Erc20TokenManager(AbstractManager):
    """
    A class that generates and tracks Erc20Token helpers

    The state dictionary is held using the "Borg" singleton pattern, which
    ensures that all instances of the class have access to the same state data
    """

    _state: dict[int, dict[str, Any]] = {}

    def __init__(
        self,
        *,
        chain_id: int | None = None,
    ) -> None:
        chain_id = chain_id if chain_id is not None else web3_connection_manager.default_chain_id

        # the internal state data for this object is held in the
        # class-level _state dictionary, keyed by the chain ID
        if self._state.get(chain_id):
            self.__dict__ = self._state[chain_id]
        else:
            self._state[chain_id] = {}
            self.__dict__ = self._state[chain_id]

            # initialize internal attributes
            self._erc20tokens: dict[ChecksumAddress, Erc20Token] = {}
            self._lock = Lock()
            self._chain_id = chain_id

    def get_erc20token(
        self,
        address: str,
        *,
        silent: bool = False,
        # accept any number of keyword arguments, which are
        # passed directly to Erc20Token without validation
        **kwargs: Any,
    ) -> Erc20Token:
        """
        Get the token object from its address
        """

        address = to_checksum_address(address)

        if token_helper := self._erc20tokens.get(address):
            return token_helper

        if token_helper := token_registry.get(token_address=address, chain_id=self._chain_id):
            return token_helper

        if address == EtherPlaceholder.address:
            token_helper = EtherPlaceholder()
        else:
            token_helper = Erc20Token(address=address, silent=silent, **kwargs)

        with self._lock:
            self._erc20tokens[address] = token_helper

        return token_helper
