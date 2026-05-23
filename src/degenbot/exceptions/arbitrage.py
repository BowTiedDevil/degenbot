"""Arbitrage-specific exceptions (solver, encoding, incompatible invariant)."""
from fractions import Fraction
from typing import Any

from degenbot.exceptions.base import DegenbotError

"""
Exceptions defined here are raised by classes and functions in the `arbitrage` module.
"""


class ArbitrageError(DegenbotError):
    """Exception raised inside arbitrage helpers."""


class ArbCalculationError(ArbitrageError):
    """Raised when an arbitrage calculation fails."""


class RateOfExchangeBelowMinimum(ArbitrageError):
    """The rate of exchange for the path is below the minimum."""

    def __init__(self, rate: Fraction) -> None:
        """Initialize the instance."""
        self.rate = rate
        super().__init__(message=f"Rate of exchange {rate} below minimum.")


class InvalidSwapPathError(ArbitrageError):
    """Raised in arbitrage helper constructors when the provided path is invalid."""


class NoLiquidity(ArbitrageError):
    """Raised if a pool has no liquidity for the requested operation."""


class InvalidForwardAmount(ArbitrageError):
    """InvalidForwardAmount class."""


"""InvalidForwardAmount class."""


class IncompatiblePoolInvariant(ArbitrageError):
    """Raised when a pool's invariant type is not supported for.

    arbitrage path construction (e.g. Aerodrome stable pools).
    """


class Unprofitable(ArbitrageError):
    """Unprofitable class."""


"""Unprofitable class."""


class NoSolverSolution(ArbitrageError):
    """NoSolverSolution class."""

    def __init__(self, message: str = "Solver failed to converge on a solution.") -> None:
        """Initialize the instance."""
        self.message = message
        super().__init__(message=message)

    def __reduce__(self) -> tuple[Any, ...]:
        """Return pickling information.

        Returns:
            The computed value.

        """
        # Pickling will raise an exception if a reduction method is not defined
        return self.__class__, (self.message,)


class OptimizationError(ArbitrageError):
    """Raised when an optimizer fails to find a profitable solution,.

    fails to converge, or receives invalid inputs.

    Attributes
    ----------
    message : str
        Human-readable error message explaining why optimization failed.
    iterations : int
        Number of iterations completed before failure (if applicable).
    method : str | None
        The solver method that was attempted (if applicable).

    """

    def __init__(
        self,
        message: str,
        *,
        iterations: int = 0,
        method: str | None = None,
    ) -> None:
        """Initialize the instance."""
        self.message = message
        self.iterations = iterations
        self.method = method
        super().__init__(message=message)

    def __reduce__(self) -> tuple[Any, ...]:
        """Return pickling information.

        Returns:
            The computed value.

        """
        # Pickling support for multiprocessing
        return self.__class__, (self.message,), {"iterations": self.iterations, "method": self.method}
