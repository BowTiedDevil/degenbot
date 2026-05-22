"""Core types for arbitrage optimization solvers.

Hop state types (Hop, HopType, etc.) are in degenbot.types.hop_types.
"""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass
from enum import Enum, auto

from degenbot.types.hop_types import BoundedProductHop as _BoundedProductHop
from degenbot.types.hop_types import HopType as _HopType
from degenbot.types.hop_types import PoolInvariant as _PoolInvariant

__all__ = [
    "SolveInput",
    "SolveResult",
    "Solver",
    "SolverMethod",
]


class SolverMethod(Enum):
    """Solver algorithm used to produce a result."""

    MOBIUS = auto()
    NEWTON = auto()
    PIECEWISE_MOBIUS = auto()
    SOLIDLY_STABLE = auto()
    BALANCER_MULTI_TOKEN = auto()
    BRENT = auto()


@dataclass(frozen=True, slots=True)
class SolveInput:
    """Unified input for all solvers."""

    hops: tuple[_HopType, ...]
    max_input: int | None = None

    @property
    def num_hops(self) -> int:
        """Return num hops."""
        return len(self.hops)

    @property
    def has_bounded_product(self) -> bool:
        """Check if bounded product."""
        return any(isinstance(h, _BoundedProductHop) for h in self.hops)

    @property
    def all_constant_product(self) -> bool:
        """Determine all constant product."""
        return not self.has_bounded_product

    @property
    def has_solidly_stable(self) -> bool:
        """Check if solidly stable."""
        return any(h.invariant == _PoolInvariant.SOLIDLY_STABLE for h in self.hops)

    @property
    def has_balancer_weighted(self) -> bool:
        """Check if balancer weighted."""
        return any(h.invariant == _PoolInvariant.BALANCER_WEIGHTED for h in self.hops)

    @property
    def has_curve_stableswap(self) -> bool:
        """Check if curve stableswap."""
        return any(h.invariant == _PoolInvariant.CURVE_STABLESWAP for h in self.hops)

    @property
    def has_balancer_multi_token(self) -> bool:
        """Check if balancer multi token."""
        return any(h.invariant == _PoolInvariant.BALANCER_MULTI_TOKEN for h in self.hops)

    @property
    def has_balancer_stableswap(self) -> bool:
        """Check if balancer stableswap."""
        return any(h.invariant == _PoolInvariant.BALANCER_STABLESWAP for h in self.hops)

    @property
    def bounded_product_indices(self) -> tuple[int, ...]:
        """Bounded product indices."""
        return tuple(i for i, h in enumerate(self.hops) if isinstance(h, _BoundedProductHop))


@dataclass(frozen=True, slots=True)
class SolveResult:
    """Unified output from all solvers."""

    optimal_input: int
    profit: int
    iterations: int
    method: SolverMethod
    solve_time_ns: int = 0


class Solver(ABC):
    """Abstract base class for arbitrage solvers."""

    MIN_HOPS = 2

    @abstractmethod
    def solve(self, solve_input: SolveInput) -> SolveResult:
        """Solve for the optimal arbitrage input."""
        ...

    @abstractmethod
    def supports(self, solve_input: SolveInput) -> bool:
        """Check whether this solver supports the given input."""
        ...