# Base exception
class DegenbotError(Exception):
    """
    Base exception, intended as a generic exception and a base class for
    for all more-specific exceptions raised by various degenbot modules
    """

    pass


class DeprecationError(ValueError):
    """
    Thrown when a feature, class, method, etc. is deprecated.
    """

    pass


# 1st level exceptions (derived from `DegenbotError`)
class ArbitrageError(DegenbotError):
    """
    Exception raised inside arbitrage helpers
    """

    pass


class Erc20TokenError(DegenbotError):
    """
    Exception raised inside ERC-20 token helpers
    """

    pass


class EVMRevertError(DegenbotError):
    """
    Thrown when a simulated EVM contract operation would revert
    """

    pass


class LiquidityPoolError(DegenbotError):
    """
    Exception raised inside liquidity pool helpers
    """

    pass


class ManagerError(DegenbotError):
    """
    Exception raised inside manager helpers
    """

    pass


class TransactionError(DegenbotError):
    """
    Exception raised inside transaction simulation helpers
    """

    pass


# 2nd level exceptions for Arbitrage classes
class ArbCalculationError(ArbitrageError):
    """
    Thrown when an arbitrage calculation fails
    """

    pass


class InvalidSwapPathError(ArbitrageError):
    """
    Thrown in arbitrage helper constructors when the provided path is invalid
    """

    pass


class ZeroLiquidityError(ArbitrageError):
    """
    Thrown by the arbitrage helper if a pool in the path has no liquidity in the direction of the proposed swap
    """

    pass


class ZeroSwapError(ArbitrageError):
    """
    Thrown by the arbitrage helper a calculated swap resulted in zero output
    """

    pass


# 2nd level exceptions for Uniswap Liquidity Pool classes


class BitmapWordUnavailableError(LiquidityPoolError):
    """
    Thrown by the ported V3 swap function when the bitmap word is not available. This should be caught by the helper to perform automatic fetching, and should not be raised to the calling function
    """

    pass


class ExternalUpdateError(LiquidityPoolError):
    """
    Thrown when an external update does not pass sanity checks
    """

    pass


class MissingTickWordError(LiquidityPoolError):
    """
    Thrown by the TickBitmap library when calling for an operation on a word that should be available, but is not
    """

    pass
