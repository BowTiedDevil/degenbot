from dataclasses import dataclass

from eth_typing import ChecksumAddress

from degenbot.types.abstract import AbstractExchangeDeployment, AbstractPoolState
from degenbot.types.concrete import PoolStateMessage
from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate
from degenbot.uniswap.v3_types import UniswapV3PoolState


@dataclass(slots=True, frozen=True)
class SolidlyFactoryDeployment:
    address: ChecksumAddress
    deployer: ChecksumAddress | None
    pool_init_hash: str


@dataclass(slots=True, frozen=True)
class SolidlyExchangeDeployment(AbstractExchangeDeployment):
    factory: SolidlyFactoryDeployment


class AerodromeV2PoolExternalUpdate(UniswapV2PoolExternalUpdate): ...


@dataclass(slots=True, frozen=True)
class AerodromeV2PoolState(AbstractPoolState):
    reserves_token0: int
    reserves_token1: int


@dataclass(slots=True, frozen=True)
class AerodromeV2PoolSimulationResult:
    amount0_delta: int
    amount1_delta: int
    current_state: AerodromeV2PoolState
    future_state: AerodromeV2PoolState


@dataclass(slots=True, frozen=True)
class AerodromeV2PoolStateUpdated(PoolStateMessage):
    state: AerodromeV2PoolState


class AerodromeV3PoolState(UniswapV3PoolState): ...


@dataclass(slots=True, frozen=True)
class AerodromeV3PoolStateUpdated(PoolStateMessage):
    state: AerodromeV3PoolState
