# Base exception
class DegenbotError(Exception):
    """
    Base exception, intended as a generic exception and a base class for
    for all more-specific exceptions raised by various degenbot modules
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


# 2nd level exceptions for Uniswap Liquidity Pool classes
class ExternalUpdateError(LiquidityPoolError):
    """
    Thrown when an external update does not pass sanity checks
    """

    pass
