from dataclasses import dataclass
from typing import List

from eth_typing import ChainId, ChecksumAddress, HexStr


@dataclass(slots=True, frozen=True)
class BaseDex:
    name: str
    chain: ChainId


@dataclass(slots=True, frozen=True)
class UniswapFactory:
    address: ChecksumAddress
    pool_init_hash: HexStr


@dataclass(slots=True, frozen=True)
class UniswapV2Dex(BaseDex):
    factory: UniswapFactory


@dataclass(slots=True, frozen=True)
class UniswapV3Dex(BaseDex):
    factory: UniswapFactory
    tick_lens: ChecksumAddress


@dataclass(slots=True, frozen=True)
class UniswapRouter:
    address: ChecksumAddress
    name: str
    dex: List[UniswapV2Dex | UniswapV3Dex]
