from dataclasses import dataclass
from typing import Any, List

from eth_typing import ChecksumAddress

from ..dataclasses import AbstractExchangeDeployment


@dataclass(slots=True, frozen=True)
class UniswapFactoryDeployment:
    address: ChecksumAddress
    deployer: ChecksumAddress | None
    pool_init_hash: str
    pool_abi: List[Any]


@dataclass(slots=True, frozen=True)
class UniswapTickLensDeployment:
    address: ChecksumAddress
    abi: List[Any]


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
