"""
Core types for arbitrage optimization solvers.

Hop state types have been moved to degenbot.types.hop_types to break
the circular import between pool and arbitrage modules. They are
re-exported here for backward compatibility with a deprecation warning.
"""

from __future__ import annotations

import importlib
import warnings
from abc import ABC, abstractmethod
from dataclasses import dataclass
from enum import Enum, auto

from degenbot.types.hop_types import HopType as _HopType
from degenbot.types.hop_types import PoolInvariant as _PoolInvariant

__all__ = [  # noqa: F822
    "BalancerMultiTokenHop",
    "BalancerWeightedHop",
    "BoundedProductHop",
    "ConstantProductHop",
    "CurveStableswapHop",
    "Hop",
    "HopType",
    "PoolInvariant",
    "SolidlyStableHop",
    "SolveInput",
    "SolveResult",
    "Solver",
    "SolverMethod",
    "V3TickRangeInfo",
    "hop_factory",
]

_DEPRECATED_NAMES = frozenset({
    "BalancerMultiTokenHop",
    "BalancerWeightedHop",
    "BoundedProductHop",
    "ConstantProductHop",
    "CurveStableswapHop",
    "Hop",
    "HopType",
    "PoolInvariant",
    "SolidlyStableHop",
    "V3TickRangeInfo",
    "hop_factory",
})

_HOP_TYPES_MODULE = "degenbot.types.hop_types"


def __getattr__(name: str) -> object:
    if name in _DEPRECATED_NAMES:
        warnings.warn(
            f"Importing {name} from degenbot.arbitrage.optimizers.hop_types is deprecated. "
            f"Import from {_HOP_TYPES_MODULE} instead.",
            DeprecationWarning,
            stacklevel=2,
        )
        mod = importlib.import_module(_HOP_TYPES_MODULE)
        return getattr(mod, name)
    msg = f"module {__name__!r} has no attribute {name!r}"
    raise AttributeError(msg)


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
        return len(self.hops)

    @property
    def has_v3(self) -> bool:
        return any(h.is_v3 for h in self.hops)

    @property
    def all_v2(self) -> bool:
        return not self.has_v3

    @property
    def all_constant_product(self) -> bool:
        return all(h.invariant == _PoolInvariant.CONSTANT_PRODUCT for h in self.hops)

    @property
    def has_solidly_stable(self) -> bool:
        return any(h.invariant == _PoolInvariant.SOLIDLY_STABLE for h in self.hops)

    @property
    def has_balancer_weighted(self) -> bool:
        return any(h.invariant == _PoolInvariant.BALANCER_WEIGHTED for h in self.hops)

    @property
    def has_curve_stableswap(self) -> bool:
        return any(h.invariant == _PoolInvariant.CURVE_STABLESWAP for h in self.hops)

    @property
    def has_balancer_multi_token(self) -> bool:
        return any(h.invariant == _PoolInvariant.BALANCER_MULTI_TOKEN for h in self.hops)

    @property
    def v3_indices(self) -> tuple[int, ...]:
        return tuple(i for i, h in enumerate(self.hops) if h.is_v3)


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
    def solve(self, solve_input: SolveInput) -> SolveResult: ...

    @abstractmethod
    def supports(self, solve_input: SolveInput) -> bool: ...
