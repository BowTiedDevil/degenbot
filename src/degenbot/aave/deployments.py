"""Aave V3 deployment addresses and chain-specific configuration."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

import eth_typing

from degenbot.checksum_cache import get_checksum_address

if TYPE_CHECKING:
    from degenbot._ffi import ChecksummedAddress


@dataclass(slots=True, frozen=True)
class AaveV3Deployment:
    """AaveV3Deployment class."""

    name: str
    chain_id: eth_typing.ChainId
    pool_address_provider: ChecksummedAddress


EthereumMainnetAaveV3 = AaveV3Deployment(
    name="Ethereum Mainnet Aave V3",
    chain_id=eth_typing.ChainId.ETH,
    pool_address_provider=get_checksum_address("0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e"),
)
