from typing import Dict, Optional, Union

from eth_utils import to_checksum_address
from eth_typing import ChecksumAddress
from web3 import Web3

from degenbot.config import get_web3
from degenbot.uniswap.abi import UNISWAP_V3_TICKLENS_ABI

_CONTRACT_ADDRESSES: Dict[
    int,  # Chain ID
    Dict[
        Union[str, ChecksumAddress],  # Factory address
        Union[str, ChecksumAddress],  # TickLens address
    ],
] = {
    # Ethereum Mainnet
    1: {
        # Uniswap V3
        # ref: https://docs.uniswap.org/contracts/v3/reference/deployments
        "0x1F98431c8aD98523631AE4a59f267346ea31F984": "0xbfd8137f7d1516D3ea5cA83523914859ec47F573",
        # Sushiswap V3
        # ref: https://docs.sushi.com/docs/Products/V3%20AMM/Periphery/Deployment%20Addresses
        "0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F": "0xFB70AD5a200d784E7901230E6875d91d5Fa6B68c",
    },
    # Arbitrum
    42161: {
        # Uniswap V3
        # ref: https://docs.uniswap.org/contracts/v3/reference/deployments
        "0x1F98431c8aD98523631AE4a59f267346ea31F984": "0xbfd8137f7d1516D3ea5cA83523914859ec47F573",
        # Sushiswap V3
        # ref: https://docs.sushi.com/docs/Products/V3%20AMM/Periphery/Deployment%20Addresses
        "0x1af415a1EbA07a4986a52B6f2e7dE7003D82231e": "0x8516944E89f296eb6473d79aED1Ba12088016c9e",
    },
}


class TickLens:
    def __init__(
        self,
        factory_address: ChecksumAddress,
        address: Optional[Union[str, ChecksumAddress]] = None,
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

        if address is None:
            factory_address = to_checksum_address(factory_address)
            address = to_checksum_address(
                _CONTRACT_ADDRESSES[self._w3.eth.chain_id][factory_address]
            )

        self.address = to_checksum_address(address)

        self._w3_contract = self._w3.eth.contract(
            address=self.address,
            abi=abi or UNISWAP_V3_TICKLENS_ABI,
        )
