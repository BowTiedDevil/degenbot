# Base exception
class DegenbotError(Exception):
    """
    Base exception, intended as a generic exception and a base class for
    for all more-specific exceptions raised by various degenbot modules
    """


class DeprecationError(ValueError):
    """
    Thrown when a feature, class, method, etc. is deprecated.

    Subclasses `ValueError` instead of `Exception`, less likely to be ignored.
    """


# 1st level exceptions (derived from `DegenbotError`)
class ArbitrageError(DegenbotError):
    """
    Exception raised inside arbitrage helpers
    """


class BlockUnavailableError(DegenbotError):
    """
    Exception raised when a call for a specific block fails (trie node unavailable)
    """


class Erc20TokenError(DegenbotError):
    """
    Exception raised inside ERC-20 token helpers
    """


class EVMRevertError(DegenbotError):
    """
    Thrown when a simulated EVM contract operation would revert
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
    Thrown when an arbitrage calculation fails
    """


class InvalidSwapPathError(ArbitrageError):
    """
    Thrown in arbitrage helper constructors when the provided path is invalid
    """

    pass


class ZeroLiquidityError(ArbitrageError):
    """
    Thrown by the arbitrage helper if a pool in the path has no liquidity in the direction of the proposed swap
    """


# 2nd level exceptions for Uniswap Liquidity Pool classes
class BitmapWordUnavailableError(LiquidityPoolError):
    """
    Thrown by the ported V3 swap function when the bitmap word is not available.
    This should be caught by the helper to perform automatic fetching, and should
    not be raised to the calling function
    """


class ExternalUpdateError(LiquidityPoolError):
    """
    Thrown when an external update does not pass sanity checks
    """


class MissingTickWordError(LiquidityPoolError):
    """
    Thrown by the TickBitmap library when calling for an operation on a word that
    should be available, but is not
    """


class ZeroSwapError(LiquidityPoolError):
    """
    Thrown if a swap calculation resulted or would result in zero output
    """


# 2nd level exceptions for Transaction classes
class LedgerError(TransactionError):
    """
    Thrown when the ledger does not align with the expected state
    """


class TransactionEncodingError(TransactionError):
    """
    Thrown when a transaction input cannot be decoded using the known ABI
    """


# 2nd level exceptions for AllPools class
class PoolAlreadyExistsError(ManagerError):
    """
    Thrown by the AllPools class if a caller attempts to store a pool helper
    at an already-known address.
    """


# 2nd level exceptions for Uniswap Manager classes
class PoolNotAssociated(ManagerError):
    """
    Thrown by a UniswapV2LiquidityPoolManager or UniswapV3LiquidityPoolManager
    class if a requested pool address is not associated with the DEX.
    """
