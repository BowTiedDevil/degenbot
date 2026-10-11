"""Shared types for concentrated-liquidity simulator decomposition."""

import dataclasses

import pydantic

from degenbot.types.aliases import BlockNumber
from degenbot.validation.evm_values import ValidatedInt128, ValidatedUint128


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


class LiquidityAtTick(pydantic.BaseModel, frozen=True):
    """Liquidity data at an initialized tick.

    ``block`` is the block at which this tick was last mutated/confirmed: the
    event block, the registration seed block, or the bulk-sync block. ``0`` is
    a legal stamp (a chain can launch at genesis), never a sentinel.
    """

    liquidity_net: ValidatedInt128
    liquidity_gross: ValidatedUint128
    block: BlockNumber
