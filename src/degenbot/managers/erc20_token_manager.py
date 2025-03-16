from threading import Lock
from typing import TYPE_CHECKING, Any, ClassVar

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress
from degenbot.cache import get_checksum_address
from degenbot.config import connection_manager
from degenbot.erc20_token import Erc20Token, EtherPlaceholder
from degenbot.registry.all_tokens import token_registry
from degenbot.types import AbstractManager


class Erc20TokenManager(AbstractManager):
    """
    A class that generates and tracks Erc20Token helpers

    The state dictionary is held using the "Borg" singleton pattern, which
    ensures that all instances of the class have access to the same state data
    """

    _state: ClassVar[dict[int, dict[str, Any]]] = {}

    def __init__(
        self,
        *,
        chain_id: int | None = None,
    ) -> None:
        chain_id = chain_id if chain_id is not None else connection_manager.default_chain_id

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

        address = get_checksum_address(address)

        if token_helper := self._erc20tokens.get(address):
            return token_helper

        if token_helper := token_registry.get(token_address=address, chain_id=self._chain_id):
            return token_helper

        if address in EtherPlaceholder.addresses:
            token_helper = EtherPlaceholder(
                address,
                chain_id=self._chain_id,
            )
        else:
            token_helper = Erc20Token(
                address,
                chain_id=self._chain_id,
                silent=silent,
                **kwargs,
            )

        with self._lock:
            self._erc20tokens[address] = token_helper

        return token_helper
