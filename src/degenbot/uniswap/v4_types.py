import dataclasses

import pydantic.config
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
)
from degenbot.validation.evm_values import ValidatedInt128, ValidatedUint128, ValidatedUint256

type FeeToProtocol = int
type SwapFee = int


class UniswapV4BitmapAtWord(pydantic.BaseModel, frozen=True):
    bitmap: ValidatedUint256
    block: BlockNumber = 0


class UniswapV4LiquidityAtTick(pydantic.BaseModel, frozen=True):
    liquidity_net: ValidatedInt128
    liquidity_gross: ValidatedUint128
    block: BlockNumber = 0


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


type InitializedTickMap = dict[BitmapWord, UniswapV4BitmapAtWord]
type LiquidityMap = dict[Tick, UniswapV4LiquidityAtTick]
