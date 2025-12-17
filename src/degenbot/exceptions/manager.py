from degenbot.exceptions.base import DegenbotError


class ManagerError(DegenbotError):
    """
    Exception raised inside manager helpers
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
