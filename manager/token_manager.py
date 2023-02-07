from degenbot.base import HelperManager
from degenbot.token import Erc20Token
from web3 import Web3


class Erc20TokenHelperManager(HelperManager):
    """
    A class that generates and tracks Erc20Token helpers

    The dictionary of token helpers is held as a class attribute, so all manager
    objects reference the same state data
    """

    erc20tokens = {}

    def get_erc20token(
        self,
        address: str,
        silent: bool = False,
    ) -> Erc20Token:
        """
        Get the token object from its address
        """

        address = Web3.toChecksumAddress(address)

        if token_helper := self.erc20tokens.get(address):
            return token_helper
        else:
            token_helper = Erc20Token(address=address, silent=silent)
            self.erc20tokens[address] = token_helper
            return token_helper
