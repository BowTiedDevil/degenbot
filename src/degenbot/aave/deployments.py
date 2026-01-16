from dataclasses import dataclass

import eth_typing
from eth_typing import ChecksumAddress

from degenbot.checksum_cache import get_checksum_address


@dataclass(slots=True, frozen=True)
class AaveV3Deployment:
    name: str
    chain_id: eth_typing.ChainId
    pool_address_provider: ChecksumAddress


EthereumMainnetAaveV3 = AaveV3Deployment(
    name="Ethereum Mainnet Aave V3",
    chain_id=eth_typing.ChainId.ETH,
    pool_address_provider=get_checksum_address("0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e"),
)
