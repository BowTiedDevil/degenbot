import dataclasses
from typing import TYPE_CHECKING
from ..baseclasses import BasePoolState, UniswapSimulationResult

if TYPE_CHECKING:
    from .v2_liquidity_pool import LiquidityPool


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV2PoolState(BasePoolState):
    pool: "LiquidityPool"
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
    current_state: UniswapV2PoolState
    future_state: UniswapV2PoolState


@dataclasses.dataclass(slots=True, eq=False)
class UniswapV2PoolExternalUpdate:
    block_number: int = dataclasses.field(compare=False)
    reserves_token0: int
    reserves_token1: int
    tx: str | None = dataclasses.field(compare=False, default=None)
