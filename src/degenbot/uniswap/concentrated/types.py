"""Shared types for concentrated-liquidity simulator decomposition."""

import dataclasses

import pydantic

from degenbot.types.aliases import BlockNumber
from degenbot.validation.evm_values import ValidatedInt128, ValidatedUint128, ValidatedUint256


@dataclasses.dataclass(slots=True, frozen=True)
class SwapResult:
    """Core mutable state produced by a concentrated-liquidity swap simulation.

    This is algorithmically identical for V3 and V4. Variant-specific wrappers
    (e.g. V3 five-tuple or V4 SwapDelta) are assembled from these fields.
    """

    amount0: int
    amount1: int
    sqrt_price_x96: int
    liquidity: int
    tick: int


class BitmapAtWord(pydantic.BaseModel, frozen=True):
    """Bitmap value at a tick bitmap word position."""

    bitmap: ValidatedUint256
    block: BlockNumber = 0


class LiquidityAtTick(pydantic.BaseModel, frozen=True):
    """Liquidity data at an initialized tick."""

    liquidity_net: ValidatedInt128
    liquidity_gross: ValidatedUint128
    block: BlockNumber = 0
