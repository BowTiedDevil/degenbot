# ruff: noqa: A005

import dataclasses

import pydantic
from eth_typing import ChecksumAddress
from hexbytes import HexBytes

from degenbot.types import (
    AbstractPoolState,
    AbstractSimulationResult,
    BlockNumber,
    PoolStateMessage,
)
from degenbot.validation.evm_values import ValidatedInt128, ValidatedUint128, ValidatedUint256


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapSimulationResult(AbstractSimulationResult):
    """
    Common attributes for Uniswap V2 & V3 simulations
    """

    amount0_delta: int
    amount1_delta: int
    initial_state: AbstractPoolState
    final_state: AbstractPoolState


@dataclasses.dataclass(slots=True, frozen=True, kw_only=True)
class UniswapV2PoolState(AbstractPoolState):
    reserves_token0: int
    reserves_token1: int


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV2PoolSimulationResult(UniswapSimulationResult):
    initial_state: UniswapV2PoolState
    final_state: UniswapV2PoolState


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV2PoolExternalUpdate:
    block_number: BlockNumber
    reserves_token0: int
    reserves_token1: int


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV2PoolStateUpdated(PoolStateMessage):
    state: UniswapV2PoolState


class UniswapV3BitmapAtWord(pydantic.BaseModel, frozen=True):
    bitmap: ValidatedUint256
    block: BlockNumber = 0


class UniswapV3LiquidityAtTick(pydantic.BaseModel, frozen=True):
    liquidity_net: ValidatedInt128
    liquidity_gross: ValidatedUint128
    block: BlockNumber = 0


@dataclasses.dataclass(slots=True)
class UniswapV3LiquidityEvent:
    block_number: BlockNumber
    liquidity: int
    tick_lower: int
    tick_upper: int
    tx_index: int
    log_index: int


@dataclasses.dataclass(slots=True, frozen=True, eq=False)
class UniswapV3PoolExternalUpdate:
    block_number: BlockNumber
    liquidity: int
    sqrt_price_x96: int
    tick: int


@dataclasses.dataclass(slots=True, frozen=True, eq=False)
class UniswapV3PoolLiquidityMappingUpdate:
    block_number: BlockNumber
    liquidity: int
    tick_lower: int
    tick_upper: int


@dataclasses.dataclass(slots=True, frozen=True, kw_only=True)
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
class UniswapV3PoolStateUpdated(PoolStateMessage):
    state: UniswapV3PoolState


class UniswapV4BitmapAtWord(UniswapV3BitmapAtWord): ...


class UniswapV4LiquidityAtTick(UniswapV3LiquidityAtTick): ...


@dataclasses.dataclass(slots=True, frozen=True, kw_only=True)
class UniswapV4PoolState(AbstractPoolState):
    liquidity: int
    sqrt_price_x96: int
    tick: int
    tick_bitmap: dict[int, UniswapV4BitmapAtWord]
    tick_data: dict[int, UniswapV4LiquidityAtTick]
    id: HexBytes
    block: BlockNumber | None


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV4PoolKey:
    currency0: ChecksumAddress
    currency1: ChecksumAddress
    fee: int
    tick_spacing: int
    hooks: ChecksumAddress


class UniswapV4LiquidityEvent(UniswapV3LiquidityEvent): ...


class UniswapV4PoolExternalUpdate(UniswapV3PoolExternalUpdate): ...


class UniswapV4PoolLiquidityMappingUpdate(UniswapV3PoolLiquidityMappingUpdate): ...


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV4PoolStateUpdated(PoolStateMessage):
    state: UniswapV4PoolState
