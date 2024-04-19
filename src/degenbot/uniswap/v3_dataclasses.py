import dataclasses
from typing import Any, Dict, Tuple

from eth_typing import ChecksumAddress

from ..baseclasses import BasePoolState, Message, UniswapSimulationResult


@dataclasses.dataclass(slots=True)
class UniswapV3BitmapAtWord:
    bitmap: int = 0
    block: int | None = dataclasses.field(compare=False, default=None)

    def to_dict(self) -> Dict[str, Any]:
        return dataclasses.asdict(self)


@dataclasses.dataclass(slots=True)
class UniswapV3LiquidityAtTick:
    liquidityNet: int = 0
    liquidityGross: int = 0
    block: int | None = dataclasses.field(compare=False, default=None)

    def to_dict(self) -> Dict[str, Any]:
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
    liquidity: int | None = None
    sqrt_price_x96: int | None = None
    tick: int | None = None
    liquidity_change: (
        Tuple[
            int,  # Liquidity
            int,  # TickLower
            int,  # TickUpper
        ]
        | None
    ) = None
    tx: str | None = dataclasses.field(compare=False, default=None)


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV3PoolState(BasePoolState):
    pool: ChecksumAddress
    liquidity: int
    sqrt_price_x96: int
    tick: int
    tick_bitmap: Dict[int, UniswapV3BitmapAtWord] | None = dataclasses.field(default=None)
    tick_data: Dict[int, UniswapV3LiquidityAtTick] | None = dataclasses.field(default=None)

    def copy(self) -> "UniswapV3PoolState":
        return UniswapV3PoolState(
            pool=self.pool,
            liquidity=self.liquidity,
            sqrt_price_x96=self.sqrt_price_x96,
            tick=self.tick,
            tick_bitmap=self.tick_bitmap.copy() if self.tick_bitmap is not None else None,
            tick_data=self.tick_data.copy() if self.tick_data is not None else None,
        )


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV3PoolSimulationResult(UniswapSimulationResult):
    initial_state: UniswapV3PoolState = dataclasses.field(compare=False)
    final_state: UniswapV3PoolState = dataclasses.field(compare=False)


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV3PoolStateUpdated(Message):
    state: UniswapV3PoolState
