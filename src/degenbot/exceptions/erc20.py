from degenbot.exceptions.base import DegenbotError


class Erc20TokenError(DegenbotError):
    """
    Exception raised inside ERC-20 token helpers.
    """


class NoPriceOracle(Erc20TokenError):
    """
    Raised when `.price` is called on a token without a price oracle.
    """

    def __init__(self) -> None:
        super().__init__(message="Token does not have a price oracle.")
