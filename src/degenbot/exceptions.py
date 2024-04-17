# Base exception
class DegenbotError(Exception):
    """
    Base exception, intended as a generic exception and a base class for
    for all more-specific exceptions raised by various degenbot modules
    """


class DeprecationError(ValueError):
    """
    Raised when a feature, class, method, etc. is deprecated.

    Subclasses `ValueError` instead of `Exception`, less likely to be ignored.
    """


# 1st level exceptions (derived from `DegenbotError`)
class ArbitrageError(DegenbotError):
    """
    Exception raised inside arbitrage helpers
    """


class Erc20TokenError(DegenbotError):
    """
    Exception raised inside ERC-20 token helpers
    """


class EVMRevertError(DegenbotError):
    """
    Raised when a simulated EVM contract operation would revert
    """


class ExternalServiceError(DegenbotError):
    """
    Raised on errors resulting to some call to an external service
    """


class LiquidityPoolError(DegenbotError):
    """
    Exception raised inside liquidity pool helpers
    """


class ManagerError(DegenbotError):
    """
    Exception raised inside manager helpers
    """


class TransactionError(DegenbotError):
    """
    Exception raised inside transaction simulation helpers
    """


# 2nd level exceptions for Arbitrage classes
class ArbCalculationError(ArbitrageError):
    """
    Raised when an arbitrage calculation fails
    """


class InvalidSwapPathError(ArbitrageError):
    """
    Raised in arbitrage helper constructors when the provided path is invalid
    """

    pass


class ZeroLiquidityError(ArbitrageError):
    """
    Raised by the arbitrage helper if a pool in the path has no liquidity in the direction of the proposed swap
    """


# 2nd level exceptions for Liquidity Pool classes
class BitmapWordUnavailableError(LiquidityPoolError):
    """
    Raised by the ported V3 swap function when the bitmap word is not available.
    This should be caught by the helper to perform automatic fetching, and should
    not be raised to the calling function
    """


class BrokenPool(LiquidityPoolError):
    """
    Raised when an pool cannot or should not be built.
    """


class ExternalUpdateError(LiquidityPoolError):
    """
    Raised when an external update does not pass sanity checks
    """


class InsufficientAmountOutError(LiquidityPoolError):
    """
    Raised if an exact output swap results in fewer tokens than requested
    """


class MissingTickWordError(LiquidityPoolError):
    """
    Raised by the TickBitmap library when calling for an operation on a word that
    should be available, but is not
    """


class NoPoolStateAvailable(LiquidityPoolError):
    """
    Raised by the `restore_state_before_block` method when a previous pool
    state is not available. This can occur, e.g. if a pool was created in a
    block at or after a re-organization.
    """


class ZeroSwapError(LiquidityPoolError):
    """
    Raised if a swap calculation resulted or would result in zero output
    """


# 2nd level exceptions for Transaction classes
class LedgerError(TransactionError):
    """
    Raised when the ledger does not align with the expected state
    """


# 2nd level exceptions for Uniswap Manager classes
class PoolNotAssociated(ManagerError):
    """
    Raised by a UniswapV2LiquidityPoolManager or UniswapV3LiquidityPoolManager
    class if a requested pool address is not associated with the DEX.
    """
