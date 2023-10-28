from threading import Lock
from typing import TYPE_CHECKING, Dict, Optional

from eth_utils import to_checksum_address

from ..config import get_web3
from ..exceptions import ManagerError
from ..erc20_token import Erc20Token
from ..baseclasses import HelperManager

if TYPE_CHECKING:
    from ..baseclasses import TokenHelper

_all_tokens: Dict[
    int,
    Dict[str, "TokenHelper"],
] = {}


class AllTokens:
    def __init__(self, chain_id):
        try:
            _all_tokens[chain_id]
        except KeyError:
            _all_tokens[chain_id] = {}
        finally:
            self.tokens = _all_tokens[chain_id]

    def __delitem__(self, token_address: str):
        del self.tokens[token_address]

    def __getitem__(self, token_address: str):
        return self.tokens[token_address]

    def __setitem__(
        self,
        token_address: str,
        token_helper: "TokenHelper",
    ):
        self.tokens[token_address] = token_helper

    def __len__(self):
        return len(self.tokens)

    def get(self, token_address: str):
        try:
            return self.tokens[token_address]
        except KeyError:
            return None


class Erc20TokenHelperManager(HelperManager):
    """
    A class that generates and tracks Erc20Token helpers

    The state dictionary is held using the "Borg" singleton pattern, which
    ensures that all instances of the class have access to the same state data
    """

    _state: dict = {}

    def __init__(self, chain_id: Optional[int] = None):
        _web3 = get_web3()
        if _web3 is not None:
            self._w3 = _web3
        else:
            from brownie import web3 as brownie_web3  # type: ignore[import]

            if brownie_web3.isConnected():
                self._w3 = brownie_web3
            else:
                raise ValueError("No connected web3 object provided.")

        chain_id = chain_id or self._w3.eth.chain_id

        # the internal state data for this object is held in the
        # class-level _state dictionary, keyed by the chain ID
        if self._state.get(chain_id):
            self.__dict__ = self._state[chain_id]
        else:
            self._state[chain_id] = {}
            self.__dict__ = self._state[chain_id]

            # initialize internal attributes
            self._erc20tokens: dict = {}
            self._lock = Lock()

    def get_erc20token(
        self,
        address: str,
        # accept any number of keyword arguments, which are
        # passed directly to Erc20Token without validation
        **kwargs,
    ) -> Erc20Token:
        """
        Get the token object from its address
        """

        address = to_checksum_address(address)

        if token_helper := self._erc20tokens.get(address):
            return token_helper

        try:
            token_helper = Erc20Token(address=address, **kwargs)
        except Exception:
            raise ManagerError(
                f"Could not create Erc20Token helper: {address=}"
            )

        with self._lock:
            self._erc20tokens[address] = token_helper

        return token_helper
