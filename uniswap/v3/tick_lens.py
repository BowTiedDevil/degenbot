from abc import ABC
from typing import Optional

from brownie import Contract  # type: ignore
from web3 import Web3

from .abi import UNISWAP_V3_TICKLENS_ABI

_MAINNET_ADDRESS = "0xbfd8137f7d1516D3ea5cA83523914859ec47F573"


class TickLens(ABC):
    def __init__(
        self,
        address: Optional[str] = None,
        abi: Optional[list] = None,
    ):
        if address is None:
            address = _MAINNET_ADDRESS

        self.address: str = Web3.toChecksumAddress(address)

        if abi is None:
            abi = UNISWAP_V3_TICKLENS_ABI

        self._brownie_contract = Contract.from_abi(
            name="TickLens",
            address=address,
            abi=abi,
        )
