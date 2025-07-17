from eth_typing import ChecksumAddress

from degenbot.exceptions import DegenbotError

"""
Exceptions defined here are raised by classes and functions in the `transaction` module.
"""


class TransactionError(DegenbotError):
    """
    Exception raised inside transaction simulation helpers.
    """


class DeadlineExpired(TransactionError): ...


class InsufficientOutput(TransactionError):
    def __init__(self, minimum: int, received: int) -> None:
        """
        The received amount was less than the minimum.
        """
        super().__init__(message=f"Insufficient output: {received} received, {minimum} required.")


class InsufficientInput(TransactionError):
    def __init__(self, minimum: int, deposited: int) -> None:
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
    ) -> None:
        self.balances = balances
        super().__init__(message="Leftover balance at router after transaction")


class PreviousBlockMismatch(TransactionError): ...


class UnknownRouterAddress(TransactionError): ...
