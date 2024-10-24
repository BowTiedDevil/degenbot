import dataclasses
from typing import Any

from degenbot.types import AbstractPoolState, AbstractSimulationResult, Message


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapSimulationResult(AbstractSimulationResult):
    """
    Common attributes for Uniswap V2 & V3 simulations
    """

    amount0_delta: int
    amount1_delta: int
    initial_state: AbstractPoolState
    final_state: AbstractPoolState


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV2PoolState(AbstractPoolState):
    reserves_token0: int
    reserves_token1: int


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


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV3BitmapAtWord:
    bitmap: int = 0
    block: int | None = dataclasses.field(compare=False, default=None)

    def to_dict(self) -> dict[str, Any]:
        return dataclasses.asdict(self)


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV3LiquidityAtTick:
    liquidity_net: int = 0
    liquidity_gross: int = 0
    block: int | None = dataclasses.field(compare=False, default=None)

    def to_dict(self) -> dict[str, Any]:
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
        tuple[
            int,  # Liquidity
            int,  # TickLower
            int,  # TickUpper
        ]
        | None
    ) = None
    tx: str | None = dataclasses.field(compare=False, default=None)


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV3PoolState(AbstractPoolState):
    liquidity: int
    sqrt_price_x96: int
    tick: int
    tick_bitmap: dict[int, UniswapV3BitmapAtWord]
    tick_data: dict[int, UniswapV3LiquidityAtTick]


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV3PoolSimulationResult(UniswapSimulationResult):
    initial_state: UniswapV3PoolState
    final_state: UniswapV3PoolState


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV3PoolStateUpdated(Message):
    state: UniswapV3PoolState
