from fractions import Fraction
from typing import TYPE_CHECKING, Any

from eth_typing import ChecksumAddress

if TYPE_CHECKING:
    from degenbot.uniswap.v4_liquidity_pool import Hooks


class DegenbotError(Exception):
    """
    Base exception used as the parent class for all exceptions raised by this package.

    Calling code should catch `DegenbotError` and derived classes separately before general
    exceptions, e.g.:

    ```
    try:
        degenbot.some_function()
    except SpecificDegenbotError:
        ... # handle a specific exception
    except DegenbotError:
        ... # handle non-specific degenbot exception
    except Exception:
        ... # handle exceptions raised by 3rd party dependencies or Python built-ins
    ```

    An optional string-formatted message may be attached to the exception and retrieved by accessing
    the `.message` attribute.
    """

    message: str | None = None

    def __init__(self, message: str | None = None) -> None:
        if message:
            self.message = message
            super().__init__(message)


class DegenbotValueError(DegenbotError): ...


class DegenbotTypeError(DegenbotError): ...


# 1st level exceptions (derived from `DegenbotError`)
class ArbitrageError(DegenbotError):
    """
    Exception raised inside arbitrage helpers.
    """


class Erc20TokenError(DegenbotError):
    """
    Exception raised inside ERC-20 token helpers.
    """


class EVMRevertError(DegenbotError):
    """
    Raised when a simulated EVM contract operation would revert.
    """

    def __init__(self, error: str) -> None:
        self.error = error
        super().__init__(message=f"EVM Revert: {error}")


class ExternalServiceError(DegenbotError):
    """
    Raised on errors resulting to some call to an external service.
    """

    def __init__(self, error: str) -> None:
        self.error = error
        super().__init__(message=f"External service error: {error}")


class LiquidityPoolError(DegenbotError):
    """
    Exception raised inside liquidity pool helpers.
    """


class ManagerError(DegenbotError):
    """
    Exception raised inside manager helpers
    """


class RegistryError(DegenbotError):
    """
    Exception raised inside registries.
    """


class TransactionError(DegenbotError):
    """
    Exception raised inside transaction simulation helpers.
    """


# 2nd level exceptions for Arbitrage classes
class ArbCalculationError(ArbitrageError):
    """
    Raised when an arbitrage calculation fails.
    """


class RateOfExchangeBelowMinimum(ArbitrageError):
    """
    The rate of exchange for the path is below the minimum.
    """

    def __init__(self, rate: Fraction) -> None:
        self.rate = rate
        super().__init__(message=f"Rate of exchange {rate} below minimum.")


class InvalidSwapPathError(ArbitrageError):
    """
    Raised in arbitrage helper constructors when the provided path is invalid.
    """


class NoLiquidity(ArbitrageError):
    """
    Raised by the arbitrage helper if a pool in the path has no liquidity in the direction of the
    proposed swap.
    """


# 2nd level exceptions for Erc20Token classes
class NoPriceOracle(Erc20TokenError):
    """
    Raised when `.price` is called on a token without a price oracle.
    """

    def __init__(self) -> None:
        super().__init__(message="Token does not have a price oracle.")


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

    def __init__(self, block: int) -> None:
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


# 2nd level exceptions for Transaction classes


class DeadlineExpired(TransactionError): ...


class InsufficientOutput(TransactionError):
    def __init__(self, minimum: int, received: int):
        """
        The received amount was less than the minimum.
        """
        super().__init__(message=f"Insufficient output: {received} received, {minimum} required.")


class InsufficientInput(TransactionError):
    def __init__(self, minimum: int, deposited: int):
        """
        The deposited amount was less than the minimum.
        """
        super().__init__(message=f"Insufficient input: {deposited} deposited, {minimum} required.")


class LeftoverRouterBalance(TransactionError):
    def __init__(
        self,
        balances: dict[
            ChecksumAddress,  # token address
            int,  # balance
        ],
    ):
        self.balances = balances
        super().__init__(message="Leftover balance at router after transaction")


class PreviousBlockMismatch(TransactionError): ...


class UnknownRouterAddress(TransactionError): ...


# 2nd level exceptions for Registry classes
class RegistryAlreadyInitialized(RegistryError):
    """
    Raised by a singleton registry if a caller attempts to recreate it.
    """


# 2nd level exceptions for Uniswap Manager classes
class PoolNotAssociated(ManagerError):
    """
    Raised by a Uniswap pool manager if a requested pool address is not associated with the DEX.
    """

    def __init__(self, pool_address: str) -> None:
        super().__init__(message=f"Pool {pool_address} is not associated with this DEX")


class PoolCreationFailed(ManagerError): ...


class ManagerAlreadyInitialized(ManagerError):
    """
    Raised by a Uniswap pool manager if a caller attempts to create from a known factory address.
    """


# 2nd level exceptions for bounded value integers


class InvalidUint256(EVMRevertError):
    def __init__(self) -> None:
        super().__init__(error="Not a valid uint256")
