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


class PathRejectedError(ArbitrageError):
    """A path candidate was rejected by a path-composition policy predicate.

    Policy rejection is distinct from the Rust core's pool *admission* floor
    (``HookedPoolRejectedError`` / ``DynamicFeePoolRejectedError``, which
    subclass ``ValueError``). Policy rejects by deployment rule (token
    denylist/allowlist, hop-count, min-liquidity, duplicate-pool); admission
    rejects by correctness floor. Callers classify by type so the two
    hierarchies (``ArbitrageError`` vs ``ValueError``) stay separable.
    """


class TokenDenylistedError(PathRejectedError):
    """An intermediate or profit token is not permitted by the policy.

    Covers both denylist membership and allowlist absence. ``token`` is the
    checksummed offending token address.
    """

    def __init__(self, *, token: str) -> None:
        """Initialize the instance."""
        self.token = token
        msg = f"Token {token} is not permitted by the path policy."
        super().__init__(message=msg)


class HopCountExceededError(PathRejectedError):
    """The path exceeds the configured maximum hop count."""

    def __init__(self, *, hop_count: int, max_hops: int) -> None:
        """Initialize the instance."""
        self.hop_count = hop_count
        self.max_hops = max_hops
        msg = f"Path has {hop_count} hops, exceeding the maximum of {max_hops}."
        super().__init__(message=msg)


class HopCountInsufficientError(PathRejectedError):
    """The path is below the configured minimum hop count."""

    def __init__(self, *, hop_count: int, min_hops: int) -> None:
        """Initialize the instance."""
        self.hop_count = hop_count
        self.min_hops = min_hops
        msg = f"Path has {hop_count} hops, below the minimum of {min_hops}."
        super().__init__(message=msg)


class InsufficientLiquidityError(PathRejectedError):
    """A pool's liquidity proxy is below the configured minimum.

    ``liquidity`` is the value returned by the caller-supplied ``liquidity_of``
    extractor (the library does not encode pool-type-specific liquidity math).
    """

    def __init__(self, *, liquidity: int, min_liquidity: int) -> None:
        """Initialize the instance."""
        self.liquidity = liquidity
        self.min_liquidity = min_liquidity
        msg = f"Pool liquidity {liquidity} below the minimum of {min_liquidity}."
        super().__init__(message=msg)


class DuplicatePoolError(PathRejectedError):
    """The same pool appears more than once in the path.

    A degenerate, non-executable cycle. V2/V3 pools are keyed by address; V4
    pools by ``pool_id`` hex. ``pool`` is the duplicated pool's identity key.
    """

    def __init__(self, *, pool: str) -> None:
        """Initialize the instance."""
        self.pool = pool
        msg = f"Pool {pool} appears more than once in the path."
        super().__init__(message=msg)


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
    """Raised when a solver fails to find a profitable solution,.

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
        return (
            self.__class__,
            (self.message,),
            {"iterations": self.iterations, "method": self.method},
        )
