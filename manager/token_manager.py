from threading import Lock
from typing import Dict, Optional

from brownie import chain  # type: ignore
from web3 import Web3

from degenbot.exceptions import ManagerError
from degenbot.token import Erc20Token
from degenbot.types import HelperManager, TokenHelper

_all_tokens: Dict[
    int,
    Dict[str, TokenHelper],
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
        token_helper: TokenHelper,
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
        if chain_id is None:
            chain_id = chain.id

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

        address = Web3.toChecksumAddress(address)

        if token_helper := self._erc20tokens.get(address):
            return token_helper

        try:
            token_helper = Erc20Token(address=address, **kwargs)
        except:
            raise ManagerError(
                f"Could not create Erc20Token helper: {address=}"
            )

        with self._lock:
            self._erc20tokens[address] = token_helper

        return token_helper
