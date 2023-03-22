from degenbot.exceptions import ManagerError
from .base import Manager
from degenbot.token import Erc20Token
from web3 import Web3
from threading import Lock


class Erc20TokenHelperManager(Manager):
    """
    A class that generates and tracks Erc20Token helpers

    The dictionary of token helpers is held as a class attribute, so all manager
    objects reference the same state data
    """

    _erc20tokens = {}
    lock = Lock()

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

        with self.lock:
            self._erc20tokens[address] = token_helper

        return token_helper
