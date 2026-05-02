"""Shared types for concentrated-liquidity simulator decomposition."""

from __future__ import annotations

import dataclasses
from typing import Self


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

    def with_replaced(
        self,
        *,
        amount0: int | None = None,
        amount1: int | None = None,
        sqrt_price_x96: int | None = None,
        liquidity: int | None = None,
        tick: int | None = None,
    ) -> Self:
        return self.__class__(
            amount0=amount0 if amount0 is not None else self.amount0,
            amount1=amount1 if amount1 is not None else self.amount1,
            sqrt_price_x96=sqrt_price_x96 if sqrt_price_x96 is not None else self.sqrt_price_x96,
            liquidity=liquidity if liquidity is not None else self.liquidity,
            tick=tick if tick is not None else self.tick,
        )
