from threading import Lock
from typing import Optional

from eth_utils.address import to_checksum_address

from .. import config
from ..baseclasses import HelperManager
from ..erc20_token import Erc20Token
from ..exceptions import ManagerError


class Erc20TokenHelperManager(HelperManager):
    """
    A class that generates and tracks Erc20Token helpers

    The state dictionary is held using the "Borg" singleton pattern, which
    ensures that all instances of the class have access to the same state data
    """

    _state: dict = {}

    def __init__(self, chain_id: Optional[int] = None):
        chain_id = chain_id if chain_id is not None else config.get_web3().eth.chain_id

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
            raise ManagerError(f"Could not create Erc20Token helper: {address=}")

        with self._lock:
            self._erc20tokens[address] = token_helper

        return token_helper
