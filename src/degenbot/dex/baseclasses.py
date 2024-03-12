from dataclasses import dataclass
from typing import List

from eth_typing import ChainId, ChecksumAddress, HexStr


@dataclass(slots=True, frozen=True)
class BaseDexDeployment:
    name: str
    chain_id: ChainId


@dataclass(slots=True, frozen=True)
class UniswapFactoryDeployment:
    address: ChecksumAddress
    pool_init_hash: HexStr


@dataclass(slots=True, frozen=True)
class UniswapTickLensDeployment:
    address: ChecksumAddress


@dataclass(slots=True, frozen=True)
class UniswapV2DexDeployment(BaseDexDeployment):
    factory: UniswapFactoryDeployment


@dataclass(slots=True, frozen=True)
class UniswapV3DexDeployment(BaseDexDeployment):
    factory: UniswapFactoryDeployment
    tick_lens: UniswapTickLensDeployment


@dataclass(slots=True, frozen=True)
class UniswapRouterDeployment:
    address: ChecksumAddress
    name: str
    exchanges: List[UniswapV2DexDeployment | UniswapV3DexDeployment]
