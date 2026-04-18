"""
Base classes and types for arbitrage optimizers.
"""

from abc import ABC, abstractmethod
from dataclasses import dataclass
from enum import Enum
from typing import Any


class OptimizerType(Enum):
    """Optimizer algorithm type."""

    NEWTON = "newton"
    GRADIENT_DESCENT = "gradient_descent"
    BRENT = "brent"
    CVXPY = "cvxpy"
    HYBRID = "hybrid"
    MOBIUS = "mobius"


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
    success : bool
        Whether optimization succeeded.
    optimizer_type : OptimizerType
        Which optimizer was used.
    error_message : str | None
        Error message if unsuccessful.
    """

    optimal_input: int
    profit: int
    solve_time_ms: float
    iterations: int
    success: bool
    optimizer_type: OptimizerType
    error_message: str | None = None


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
