from degenbot.exceptions.base import DegenbotError


class EVMRevertError(DegenbotError):
    """
    Raised when a simulated EVM contract operation would revert.
    """

    def __init__(self, error: str | None = None) -> None:
        self.error = error
        if error:
            super().__init__(message=f"EVM Revert: {error}")
        else:
            super().__init__(message="EVM Revert")


class InvalidUint256(EVMRevertError):
    def __init__(self) -> None:
        super().__init__(error="Not a valid uint256")
