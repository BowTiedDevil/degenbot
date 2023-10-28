from typing import Optional, Union

from eth_typing import ChecksumAddress
from eth_utils import to_checksum_address

from ..config import get_web3
from .abi import UNISWAP_V3_TICKLENS_ABI


class TickLens:
    def __init__(
        self,
        address: Union[str, ChecksumAddress],
        abi: Optional[list] = None,
    ):
        _web3 = get_web3()
        if _web3 is not None:
            self._w3 = _web3
        else:
            from brownie import web3 as brownie_web3  # type: ignore[import]

            if brownie_web3.isConnected():
                self._w3 = brownie_web3
            else:
                raise ValueError("No connected web3 object provided.")

        self.address = to_checksum_address(address)

        self._w3_contract = self._w3.eth.contract(
            address=self.address,
            abi=abi or UNISWAP_V3_TICKLENS_ABI,
        )
