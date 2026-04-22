from dataclasses import dataclass
from enum import Enum
from fractions import Fraction
from typing import Union


class SolverMethod(Enum):
    MOBIUS = "mobius"
    PIECEWISE_MOBIUS = "piecewise_mobius"


@dataclass(frozen=True, slots=True)
class MobiusHopState:
    """
    Minimal pool state for the Mobius solver.

    Represents any CFMM hop whose swap function is the Mobius
    transformation y = gamma * s * x / (r + gamma * x), including
    constant-product (V2) and single-range bounded-product (V3/V4).
    """

    reserve_in: int
    reserve_out: int
    fee: Fraction

    @property
    def gamma(self) -> float:
        return 1.0 - float(self.fee)


@dataclass(frozen=True, slots=True)
class TickRangeState:
    """State of a single V3/V4 tick range."""

    tick_lower: int
    tick_upper: int
    liquidity: int
    sqrt_price_lower: int
    sqrt_price_upper: int


@dataclass(frozen=True, slots=True)
class ConcentratedLiquidityHopState(MobiusHopState):
    """
    V3/V4 hop with optional multi-range tick data.

    When tick_ranges is None or has a single entry, the hop behaves as a
    simple MobiusHopState (single-range). When tick_ranges has multiple
    entries, the solver uses piecewise-Mobius with golden section search.
    """

    liquidity: int
    sqrt_price: int
    tick_lower: int
    tick_upper: int
    tick_ranges: tuple[TickRangeState, ...] | None = None
    current_range_index: int = 0

    @property
    def has_multi_range(self) -> bool:
        return self.tick_ranges is not None and len(self.tick_ranges) > 1


HopState = Union[
    MobiusHopState,
    ConcentratedLiquidityHopState,
]


@dataclass(frozen=True, slots=True)
class MobiusSolveResult:
    """Output from the Mobius solver."""

    optimal_input: int
    profit: int
    is_profitable: bool
    method: SolverMethod
    iterations: int = 0
    error: str | None = None
