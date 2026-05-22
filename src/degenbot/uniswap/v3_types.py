"""Uniswap V3-specific data types and state definitions."""
import dataclasses

from degenbot.types.abstract import AbstractPoolState, AbstractSimulationResult
from degenbot.types.aliases import BlockNumber
from degenbot.types.concrete import PoolStateMessage
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick

type BitmapWord = int
type Pip = int  # V3 pool fees are expressed in pips equaling one hundredth of 1%
type Liquidity = int
type LiquidityGross = int
type LiquidityNet = int
type SqrtPriceX96 = int
type Tick = int
type TickBitmap = int


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapSimulationResult(AbstractSimulationResult):
    """Common attributes for Uniswap V2 & V3 simulations."""


@dataclasses.dataclass(slots=True)
class UniswapV3LiquidityEvent:
    """UniswapV3LiquidityEvent class."""

    block_number: BlockNumber
    liquidity: Liquidity
    tick_lower: Tick
    tick_upper: Tick
    tx_index: int
    log_index: int


@dataclasses.dataclass(slots=True, frozen=True, eq=False)
class UniswapV3PoolExternalUpdate:
    """UniswapV3PoolExternalUpdate class."""

    block_number: BlockNumber
    liquidity: Liquidity
    sqrt_price_x96: SqrtPriceX96
    tick: Tick


@dataclasses.dataclass(slots=True, frozen=True, eq=False)
class UniswapV3PoolLiquidityMappingUpdate:
    """UniswapV3PoolLiquidityMappingUpdate class."""

    block_number: BlockNumber
    liquidity: Liquidity
    tick_lower: Tick
    tick_upper: Tick


@dataclasses.dataclass(slots=True, frozen=True, kw_only=True)
class UniswapV3PoolState(AbstractPoolState):
    """UniswapV3PoolState class."""

    liquidity: Liquidity
    sqrt_price_x96: SqrtPriceX96
    tick: Tick
    tick_bitmap: dict[BitmapWord, BitmapAtWord]
    tick_data: dict[Tick, LiquidityAtTick]


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV3PoolSimulationResult(UniswapSimulationResult):
    """UniswapV3PoolSimulationResult class."""

    initial_state: UniswapV3PoolState
    final_state: UniswapV3PoolState


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV3PoolStateUpdated(PoolStateMessage):
    """UniswapV3PoolStateUpdated class."""

    state: UniswapV3PoolState


type InitializedTickMap = dict[BitmapWord, BitmapAtWord]
type LiquidityMap = dict[Tick, LiquidityAtTick]
