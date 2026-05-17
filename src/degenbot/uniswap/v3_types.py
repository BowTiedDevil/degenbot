import dataclasses

import pydantic

from degenbot.types.abstract import AbstractPoolState, AbstractSimulationResult
from degenbot.types.aliases import BlockNumber
from degenbot.types.concrete import PoolStateMessage
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.validation.evm_values import ValidatedInt128, ValidatedUint128, ValidatedUint256

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
    """
    Common attributes for Uniswap V2 & V3 simulations
    """




@dataclasses.dataclass(slots=True)
class UniswapV3LiquidityEvent:
    block_number: BlockNumber
    liquidity: Liquidity
    tick_lower: Tick
    tick_upper: Tick
    tx_index: int
    log_index: int


@dataclasses.dataclass(slots=True, frozen=True, eq=False)
class UniswapV3PoolExternalUpdate:
    block_number: BlockNumber
    liquidity: Liquidity
    sqrt_price_x96: SqrtPriceX96
    tick: Tick


@dataclasses.dataclass(slots=True, frozen=True, eq=False)
class UniswapV3PoolLiquidityMappingUpdate:
    block_number: BlockNumber
    liquidity: Liquidity
    tick_lower: Tick
    tick_upper: Tick


@dataclasses.dataclass(slots=True, frozen=True, kw_only=True)
class UniswapV3PoolState(AbstractPoolState):
    liquidity: Liquidity
    sqrt_price_x96: SqrtPriceX96
    tick: Tick
    tick_bitmap: dict[BitmapWord, BitmapAtWord]
    tick_data: dict[Tick, LiquidityAtTick]


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV3PoolSimulationResult(UniswapSimulationResult):
    initial_state: UniswapV3PoolState
    final_state: UniswapV3PoolState


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV3PoolStateUpdated(PoolStateMessage):
    state: UniswapV3PoolState


type InitializedTickMap = dict[BitmapWord, BitmapAtWord]
type LiquidityMap = dict[Tick, LiquidityAtTick]
