from fractions import Fraction

from degenbot.exceptions import DegenbotError

"""
Exceptions defined here are raised by classes and functions in the `arbitrage` module.
"""


class ArbitrageError(DegenbotError):
    """
    Exception raised inside arbitrage helpers.
    """


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
    Raised if a pool has no liquidity for the requested operation.
    """
