import dataclasses

from eth_typing import ChecksumAddress

from ..types import AbstractPoolState, Message


@dataclasses.dataclass(slots=True, frozen=True)
class AerodromeV2PoolState(AbstractPoolState):
    pool: ChecksumAddress
    reserves_token0: int
    reserves_token1: int


@dataclasses.dataclass(slots=True, frozen=True)
class AerodromeV2PoolSimulationResult:
    amount0_delta: int
    amount1_delta: int
    current_state: AerodromeV2PoolState
    future_state: AerodromeV2PoolState


@dataclasses.dataclass(slots=True, frozen=True)
class AerodromeV2PoolStateUpdated(Message):
    state: AerodromeV2PoolState
