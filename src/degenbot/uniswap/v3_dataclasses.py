import dataclasses
from typing import TYPE_CHECKING, Dict, Optional, Tuple

if TYPE_CHECKING:
    # only necessary for the type hint
    from .v3_liquidity_pool import V3LiquidityPool


@dataclasses.dataclass(slots=True)
class UniswapV3BitmapAtWord:
    bitmap: int = 0
    block: Optional[int] = dataclasses.field(compare=False, default=None)

    def to_dict(self):
        return dataclasses.asdict(self)


@dataclasses.dataclass(slots=True)
class UniswapV3LiquidityAtTick:
    liquidityNet: int = 0
    liquidityGross: int = 0
    block: Optional[int] = dataclasses.field(compare=False, default=None)

    def to_dict(self):
        return dataclasses.asdict(self)


@dataclasses.dataclass(slots=True)
class UniswapV3LiquidityEvent:
    block_number: int
    liquidity: int
    tick_lower: int
    tick_upper: int
    tx_index: int


@dataclasses.dataclass(slots=True, eq=False)
class UniswapV3PoolExternalUpdate:
    block_number: int = dataclasses.field(compare=False)
    liquidity: Optional[int] = None
    sqrt_price_x96: Optional[int] = None
    tick: Optional[int] = None
    liquidity_change: Optional[
        Tuple[
            int,  # Liquidity
            int,  # TickLower
            int,  # TickUpper
        ]
    ] = None
    tx: Optional[str] = dataclasses.field(compare=False, default=None)


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV3PoolState:
    pool: "V3LiquidityPool"
    liquidity: int
    sqrt_price_x96: int
    tick: int
    tick_bitmap: Optional[Dict[int, UniswapV3BitmapAtWord]] = dataclasses.field(default=None)
    tick_data: Optional[Dict[int, UniswapV3LiquidityAtTick]] = dataclasses.field(default=None)


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV3PoolSimulationResult:
    amount0_delta: int
    amount1_delta: int
    current_state: UniswapV3PoolState = dataclasses.field(compare=False)
    future_state: UniswapV3PoolState = dataclasses.field(compare=False)
