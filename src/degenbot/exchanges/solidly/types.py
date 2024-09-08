from dataclasses import dataclass
from typing import Any

from eth_typing import ChecksumAddress

from ..types import AbstractExchangeDeployment


@dataclass(slots=True, frozen=True)
class SolidlyFactoryDeployment:
    address: ChecksumAddress
    deployer: ChecksumAddress | None
    pool_init_hash: str
    pool_abi: list[Any]


@dataclass(slots=True, frozen=True)
class SolidlyExchangeDeployment(AbstractExchangeDeployment):
    factory: SolidlyFactoryDeployment
