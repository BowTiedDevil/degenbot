"""
Base classes and types for arbitrage optimizers.
"""

from abc import ABC, abstractmethod
from dataclasses import dataclass
from enum import Enum, auto
from typing import Any


class OptimizerType(Enum):
    """Optimizer algorithm type."""

    NEWTON = auto()
    GRADIENT_DESCENT = auto()
    BRENT = auto()
    CVXPY = auto()
    HYBRID = auto()
    MOBIUS = auto()


@dataclass(slots=True, frozen=True)
class OptimizerResult:
    """
    Result from an arbitrage optimizer.

    Attributes
    ----------
    optimal_input : int
        Optimal input amount in wei.
    profit : int
        Expected profit in wei (output - input).
    solve_time_ms : float
        Time to solve in milliseconds.
    iterations : int
        Number of iterations taken.
    optimizer_type : OptimizerType
        Which optimizer was used.
    """

    optimal_input: int
    profit: int
    solve_time_ms: float
    iterations: int
    optimizer_type: OptimizerType


class ArbitrageOptimizer(ABC):
    """Abstract base class for arbitrage optimizers."""

    @property
    @abstractmethod
    def optimizer_type(self) -> OptimizerType:
        """Return the optimizer type."""
        ...

    @abstractmethod
    def solve(
        self,
        pools: list[Any],
        input_token: Any,
        max_input: int | None = None,
    ) -> OptimizerResult:
        """
        Find optimal arbitrage input.

        Parameters
        ----------
        pools : list[Any]
            List of pools in the arbitrage path.
        input_token : Any
            The token being input (Erc20Token).
        max_input : int | None
            Maximum input amount (optional constraint).

        Returns
        -------
        OptimizerResult
            Optimization result with optimal input and profit.
        """
        ...
