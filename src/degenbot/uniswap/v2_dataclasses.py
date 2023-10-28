import dataclasses
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .v2_liquidity_pool import LiquidityPool


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV2PoolState:
    pool: "LiquidityPool"
    reserves_token0: int
    reserves_token1: int


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV2PoolSimulationResult:
    amount0_delta: int
    amount1_delta: int
    current_state: UniswapV2PoolState
    future_state: UniswapV2PoolState
