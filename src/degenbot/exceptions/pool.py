"""
Pool-related exceptions.

Includes exceptions for:
- Generic pool operations (LiquidityPoolError, BrokenPool, ...)
- EVM execution (EVMRevertError, InvalidUint256)
- Curve StableSwap (CurveError, MissingCurveData)
- Pool trackers (TrackerError, PoolNotAssociated, ...)
"""

from typing import TYPE_CHECKING, Any

from eth_typing import ChecksumAddress
from hexbytes import HexBytes

from degenbot.exceptions.base import DegenbotError
from degenbot.types.aliases import BlockNumber

if TYPE_CHECKING:
    from degenbot.uniswap.v4_liquidity_pool import Hooks

# --- EVM ---


class EVMRevertError(DegenbotError):
    """Raised when a simulated EVM contract operation would revert."""

    def __init__(self, error: str | None = None) -> None:
        """Initialize the instance."""
        self.error = error
        if error:
            super().__init__(message=f"EVM Revert: {error}")
        else:
            super().__init__(message="EVM Revert")


class InvalidUint256(EVMRevertError):
    """InvalidUint256 class."""

    def __init__(self) -> None:
        """Initialize the instance."""
        super().__init__(error="Not a valid uint256")


# --- Curve ---


class CurveError(DegenbotError):
    """Base exception for Curve pool errors."""


class MissingCurveData(CurveError):
    """Raised when on-chain data is needed but no fetcher is available."""

    def __init__(
        self,
        pool_address: str,
        data_type: str,
        message: str,
    ) -> None:
        """Initialize the instance."""
        self.pool_address = pool_address
        self.data_type = data_type
        super().__init__(message=message)


# --- Pool Trackers ---


class TrackerError(DegenbotError):
    """Exception raised inside pool tracker helpers."""


class PoolNotAssociated(TrackerError):
    """Raised by a pool tracker if a requested pool address is not associated with the DEX."""

    def __init__(self, pool_address: str) -> None:
        """Initialize the instance."""
        super().__init__(message=f"Pool {pool_address} is not associated with this DEX")


class PoolCreationFailed(TrackerError):
    """PoolCreationFailed class."""


"""PoolCreationFailed class."""


class TrackerAlreadyInitialized(TrackerError):
    """Raised by a pool tracker if a caller attempts to create from a known factory address."""


# --- Liquidity Pools ---


class LiquidityPoolError(DegenbotError):
    """Exception raised inside liquidity pool helpers."""


class AddressMismatch(LiquidityPoolError):
    """The expected pool address does not match the provided address."""

    def __init__(self) -> None:
        """Initialize the instance."""
        super().__init__(message="Pool address verification failed.")


class LiquidityMapWordMissing(LiquidityPoolError):
    """A word bitmap is not included in the liquidity map."""

    def __init__(self, word: int) -> None:
        """Initialize the instance."""
        self.word = word
        super().__init__(message=f"Word {word} is unknown.")


class BrokenPool(LiquidityPoolError):
    """BrokenPool class."""

    def __init__(self) -> None:
        """Raise when a pool cannot or should not be built."""
        super().__init__(message="This pool is known to be broken.")


class ExternalUpdateError(LiquidityPoolError):
    """Raised when an external update does not pass sanity checks."""


class IncompleteSwap(LiquidityPoolError):
    """Raised if a swap calculation would not consume the input or deliver the requested output."""

    def __init__(self, amount_in: int, amount_out: int) -> None:
        """Initialize the instance."""
        self.amount_in = amount_in
        self.amount_out = amount_out
        super().__init__(message="Insufficient liquidity to swap for the requested amount.")

    def __reduce__(self) -> tuple[Any, ...]:
        """Return pickling information."""
        # Pickling will raise an exception if a reduction method is not defined
        return self.__class__, (self.amount_in, self.amount_out)


class LateUpdateError(LiquidityPoolError):
    """Raised when an automatic update is attempted at a block prior to the last recorded update."""


class NoPoolStateAvailable(LiquidityPoolError):
    """
    Raised by the `restore_state_before_block` method when a previous pool state is not available.

    This can occur, e.g. if a pool was created in a block at or after a re-organization.
    """

    def __init__(self, block: BlockNumber) -> None:
        """Initialize the instance."""
        super().__init__(message=f"No pool state known prior to block {block}")


class InvalidSwapInputAmount(LiquidityPoolError):
    """InvalidSwapInputAmount class."""

    def __init__(self) -> None:
        """Raise if a swap input amount is invalid."""
        super().__init__(message="The swap input is invalid.")


class PossibleInaccurateResult(LiquidityPoolError):
    """
    Raised when a swap calculation may not match the on-chain result.

    The computed ``amount_in`` and ``amount_out`` are available on the
    exception so callers can inspect or use the approximate values
    after explicitly catching this exception.

    Subclasses add domain-specific context (hooks, stale rates, etc.).
    """

    def __init__(self, amount_in: int, amount_out: int, *, message: str) -> None:
        """Initialize the instance."""
        self.amount_in = amount_in
        self.amount_out = amount_out
        super().__init__(message=message)

    def __reduce__(self) -> tuple[Any, ...]:
        """Return pickling information."""
        # Pickling will raise an exception if a reduction method is not defined
        return self.__class__, (self.amount_in, self.amount_out)


class HookedPoolResult(PossibleInaccurateResult):
    """
    Raised when a V4 pool has active hooks that may mutate the swap result.

    The pool's ``beforeSwap`` / ``afterSwap`` hooks can modify amounts or
    revert, so the pure-math result may differ from what the contract returns.
    The set of conflicting hooks is available on the ``hooks`` attribute.
    """

    def __init__(self, amount_in: int, amount_out: int, hooks: set["Hooks"]) -> None:
        """Initialize the instance."""
        self.hooks = hooks
        super().__init__(
            amount_in=amount_in,
            amount_out=amount_out,
            message="The pool has one or more hooks that might invalidate the calculated result.",
        )

    def __reduce__(self) -> tuple[Any, ...]:
        """Return pickling information."""
        return self.__class__, (self.amount_in, self.amount_out, self.hooks)


class StaleRateResult(PossibleInaccurateResult):
    """
    Raised when a Balancer ComposableStablePool's rate cache is stale.

    ComposableStablePools with time-varying rates (e.g. bb-a-* yield tokens)
    cache rates in ``_tokenRateCaches`` and refresh them before each swap
    via ``_beforeSwapJoinExit()``. Without a live ``BalancerRateProvider``,
    the pool uses construction-time rates that become stale as blocks pass.
    The computed amounts are available but may not match on-chain execution.
    """

    def __init__(self, amount_in: int, amount_out: int) -> None:
        """Initialize the instance."""
        super().__init__(
            amount_in=amount_in,
            amount_out=amount_out,
            message="The pool's rate cache is stale; the calculated result may not match on-chain.",
        )

    def __reduce__(self) -> tuple[Any, ...]:
        """Return pickling information."""
        return self.__class__, (self.amount_in, self.amount_out)


class UnknownPool(LiquidityPoolError):
    """
    Raised by the liquidity snapshot class `update` methods when an update is provided for a pool.

    address not present in the existing snapshot. Updates of this kind can lead to inconsistent
    state, because the pool state prior to the update is unknown.
    """

    def __init__(self, pool: ChecksumAddress) -> None:
        """Initialize the instance."""
        super().__init__(message=f"A liquidity update for unknown pool {pool} was provided.")


class UnknownPoolId(LiquidityPoolError):
    """
    Raised by the liquidity snapshot class `update` methods when an update is provided for a pool.

    address not present in the existing snapshot. Updates of this kind can lead to inconsistent
    state, because the pool state prior to the update is unknown.
    """

    def __init__(self, pool_id: bytes | str) -> None:
        """Initialize the instance."""
        pool_id = HexBytes(pool_id).to_0x_hex()
        super().__init__(message=f"A liquidity update for unknown pool ID {pool_id} was provided.")
