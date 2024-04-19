import dataclasses

from eth_typing import ChecksumAddress

from ..baseclasses import BasePoolState, Message, UniswapSimulationResult


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV2PoolState(BasePoolState):
    pool: ChecksumAddress
    reserves_token0: int
    reserves_token1: int

    def copy(self) -> "UniswapV2PoolState":
        return UniswapV2PoolState(
            pool=self.pool,
            reserves_token0=self.reserves_token0,
            reserves_token1=self.reserves_token1,
        )


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV2PoolSimulationResult(UniswapSimulationResult):
    initial_state: UniswapV2PoolState
    final_state: UniswapV2PoolState


@dataclasses.dataclass(slots=True, eq=False)
class UniswapV2PoolExternalUpdate:
    block_number: int = dataclasses.field(compare=False)
    reserves_token0: int
    reserves_token1: int
    tx: str | None = dataclasses.field(compare=False, default=None)


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV2PoolStateUpdated(Message):
    state: UniswapV2PoolState
