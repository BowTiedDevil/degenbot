from dataclasses import dataclass
from typing import List

from eth_typing import ChecksumAddress, HexStr

from ..dataclasses import AbstractExchangeDeployment


@dataclass(slots=True, frozen=True)
class UniswapFactoryDeployment:
    address: ChecksumAddress
    pool_init_hash: HexStr


@dataclass(slots=True, frozen=True)
class UniswapTickLensDeployment:
    address: ChecksumAddress


@dataclass(slots=True, frozen=True)
class UniswapV2ExchangeDeployment(AbstractExchangeDeployment):
    factory: UniswapFactoryDeployment


@dataclass(slots=True, frozen=True)
class UniswapV3ExchangeDeployment(AbstractExchangeDeployment):
    factory: UniswapFactoryDeployment
    tick_lens: UniswapTickLensDeployment


@dataclass(slots=True, frozen=True)
class UniswapRouterDeployment:
    address: ChecksumAddress
    chain_id: int
    name: str
    exchanges: List[UniswapV2ExchangeDeployment | UniswapV3ExchangeDeployment]
