import dataclasses

from eth_typing import ChecksumAddress
from hexbytes import HexBytes

from degenbot.types.abstract import AbstractPoolState
from degenbot.types.aliases import BlockNumber
from degenbot.types.concrete import PoolStateMessage
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.v3_types import (
    BitmapWord,
    Liquidity,
    Pip,
    SqrtPriceX96,
    Tick,
)

type FeeToProtocol = int
type SwapFee = int


@dataclasses.dataclass(slots=True, frozen=True, kw_only=True)
class UniswapV4PoolState(AbstractPoolState):
    liquidity: Liquidity
    sqrt_price_x96: SqrtPriceX96
    tick: Tick
    tick_bitmap: dict[BitmapWord, BitmapAtWord]
    tick_data: dict[Tick, LiquidityAtTick]
    id: HexBytes
    block: BlockNumber | None


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV4PoolKey:
    currency0: ChecksumAddress
    currency1: ChecksumAddress
    fee: Pip
    tick_spacing: int
    hooks: ChecksumAddress


@dataclasses.dataclass(slots=True)
class UniswapV4LiquidityEvent:
    block_number: BlockNumber
    liquidity: Liquidity
    tick_lower: Tick
    tick_upper: Tick
    tx_index: int
    log_index: int


@dataclasses.dataclass(slots=True, frozen=True, eq=False)
class UniswapV4PoolExternalUpdate:
    block_number: BlockNumber
    liquidity: Liquidity
    sqrt_price_x96: SqrtPriceX96
    tick: Tick


@dataclasses.dataclass(slots=True, frozen=True, eq=False)
class UniswapV4PoolLiquidityMappingUpdate:
    block_number: BlockNumber
    liquidity: Liquidity
    tick_lower: Tick
    tick_upper: Tick


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV4PoolStateUpdated(PoolStateMessage):
    state: UniswapV4PoolState


type InitializedTickMap = dict[BitmapWord, BitmapAtWord]
type LiquidityMap = dict[Tick, LiquidityAtTick]
