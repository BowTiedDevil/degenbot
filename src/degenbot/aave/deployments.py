"""Aave V3 deployment addresses and chain-specific configuration."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

from degenbot.checksum_cache import get_checksum_address
from degenbot.types.chain import ChainId

if TYPE_CHECKING:
    from degenbot._ffi import ChecksummedAddress


@dataclass(slots=True, frozen=True)
class AaveV3Deployment:
    """AaveV3Deployment class."""

    name: str
    chain_id: ChainId
    pool_address_provider: ChecksummedAddress


EthereumMainnetAaveV3 = AaveV3Deployment(
    name="Ethereum Mainnet Aave V3",
    chain_id=ChainId.ETH,
    pool_address_provider=get_checksum_address("0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e"),
)
