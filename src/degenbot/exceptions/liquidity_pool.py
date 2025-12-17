from typing import TYPE_CHECKING, Any

from eth_typing import ChecksumAddress
from hexbytes import HexBytes

from degenbot.exceptions.base import DegenbotError
from degenbot.types.aliases import BlockNumber

if TYPE_CHECKING:
    from degenbot.uniswap.v4_liquidity_pool import Hooks


class LiquidityPoolError(DegenbotError):
    """
    Exception raised inside liquidity pool helpers.
    """


# 2nd level exceptions for Liquidity Pool classes
class AddressMismatch(LiquidityPoolError):
    """
    The expected pool address does not match the provided address.
    """

    def __init__(self) -> None:
        super().__init__(message="Pool address verification failed.")


class LiquidityMapWordMissing(LiquidityPoolError):
    """
    A word bitmap is not included in the liquidity map.
    """

    def __init__(self, word: int) -> None:
        self.word = word
        super().__init__(message=f"Word {word} is unknown.")


class BrokenPool(LiquidityPoolError):
    def __init__(self) -> None:
        """
        Raised when an pool cannot or should not be built.
        """

        super().__init__(message="This pool is known to be broken.")


class ExternalUpdateError(LiquidityPoolError):
    """
    Raised when an external update does not pass sanity checks.
    """


class IncompleteSwap(LiquidityPoolError):
    """
    Raised if a swap calculation would not consume the input or deliver the requested output.
    """

    def __init__(self, amount_in: int, amount_out: int) -> None:
        self.amount_in = amount_in
        self.amount_out = amount_out
        super().__init__(message="Insufficient liquidity to swap for the requested amount.")

    def __reduce__(self) -> tuple[Any, ...]:
        # Pickling will raise an exception if a reduction method is not defined
        return self.__class__, (self.amount_in, self.amount_out)


class LateUpdateError(LiquidityPoolError):
    """
    Raised when an automatic update is attempted at a block prior to the last recorded update.
    """


class NoPoolStateAvailable(LiquidityPoolError):
    """
    Raised by the `restore_state_before_block` method when a previous pool state is not available.
    This can occur, e.g. if a pool was created in a block at or after a re-organization.
    """

    def __init__(self, block: BlockNumber) -> None:
        super().__init__(message=f"No pool state known prior to block {block}")


class InvalidSwapInputAmount(LiquidityPoolError):
    def __init__(self) -> None:
        """
        Raised if a swap input amount is invalid.
        """

        super().__init__(message="The swap input is invalid.")


class PossibleInaccurateResult(LiquidityPoolError):
    def __init__(self, amount_in: int, amount_out: int, hooks: set["Hooks"]) -> None:
        """
        Raised if a pool has an active hook that might invalidate the calculated result.
        """

        self.amount_in = amount_in
        self.amount_out = amount_out
        self.hooks = hooks
        super().__init__(
            message="The pool has one or more hooks that might invalidate the calculated result."
        )

    def __reduce__(self) -> tuple[Any, ...]:
        # Pickling will raise an exception if a reduction method is not defined
        return self.__class__, (self.amount_in, self.amount_out, self.hooks)


class UnknownPool(LiquidityPoolError):
    """
    Raised by the liquidity snapshot class `update` methods when an update is provided for a pool
    address not present in the existing snapshot. Updates of this kind can lead to inconsistent
    state, because the pool state prior to the update is unknown.
    """

    def __init__(self, pool: ChecksumAddress) -> None:
        super().__init__(message=f"A liquidity update for unknown pool {pool} was provided.")


class UnknownPoolId(LiquidityPoolError):
    """
    Raised by the liquidity snapshot class `update` methods when an update is provided for a pool
    address not present in the existing snapshot. Updates of this kind can lead to inconsistent
    state, because the pool state prior to the update is unknown.
    """

    def __init__(self, pool_id: bytes | str) -> None:
        pool_id = HexBytes(pool_id).to_0x_hex()
        super().__init__(message=f"A liquidity update for unknown pool ID {pool_id} was provided.")
