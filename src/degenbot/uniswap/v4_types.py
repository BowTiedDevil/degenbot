import dataclasses

from eth_typing import ChecksumAddress
from hexbytes import HexBytes

from degenbot.types.abstract import AbstractPoolState
from degenbot.types.aliases import BlockNumber
from degenbot.types.concrete import PoolStateMessage
from degenbot.uniswap.v3_types import (
    BitmapWord,
    Liquidity,
    Pip,
    SqrtPriceX96,
    Tick,
    UniswapV3BitmapAtWord,
    UniswapV3LiquidityAtTick,
    UniswapV3LiquidityEvent,
    UniswapV3PoolExternalUpdate,
    UniswapV3PoolLiquidityMappingUpdate,
)

type FeeToProtocol = int
type SwapFee = int


class UniswapV4BitmapAtWord(UniswapV3BitmapAtWord): ...


class UniswapV4LiquidityAtTick(UniswapV3LiquidityAtTick): ...


@dataclasses.dataclass(slots=True, frozen=True, kw_only=True)
class UniswapV4PoolState(AbstractPoolState):
    liquidity: Liquidity
    sqrt_price_x96: SqrtPriceX96
    tick: Tick
    tick_bitmap: dict[BitmapWord, UniswapV4BitmapAtWord]
    tick_data: dict[Tick, UniswapV4LiquidityAtTick]
    id: HexBytes
    block: BlockNumber | None


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV4PoolKey:
    currency0: ChecksumAddress
    currency1: ChecksumAddress
    fee: Pip
    tick_spacing: int
    hooks: ChecksumAddress


class UniswapV4LiquidityEvent(UniswapV3LiquidityEvent): ...


class UniswapV4PoolExternalUpdate(UniswapV3PoolExternalUpdate): ...


class UniswapV4PoolLiquidityMappingUpdate(UniswapV3PoolLiquidityMappingUpdate): ...


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV4PoolStateUpdated(PoolStateMessage):
    state: UniswapV4PoolState


type InitializedTickMap = dict[BitmapWord, UniswapV4BitmapAtWord]
type LiquidityMap = dict[Tick, UniswapV4LiquidityAtTick]
