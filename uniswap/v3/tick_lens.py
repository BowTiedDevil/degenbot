from abc import ABC

from brownie import Contract  # type: ignore
from web3 import Web3


class TickLens(ABC):
    def __init__(self, address="0xbfd8137f7d1516D3ea5cA83523914859ec47F573"):
        self.address: str = Web3.toChecksumAddress(address)

        try:
            self._brownie_contract = Contract(address)
        except:
            try:
                self._brownie_contract = Contract.from_explorer(
                    address=address, silent=True
                )
            except:
                raise
