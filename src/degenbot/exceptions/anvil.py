from degenbot.exceptions.base import DegenbotError


class AnvilError(DegenbotError):
    """
    Raised on errors resulting from failed calls to Anvil via JSON-RPC.

    This exception is specifically for errors that occur when making RPC calls
    to an Anvil instance, such as invalid method calls, parameter errors,
    or other Anvil-specific failures.
    """

    def __init__(self, method: str, error: str) -> None:
        """
        Initialize the AnvilError.

        Args:
            method: The RPC method that was called
            error: The error message returned by Anvil
        """
        self.method = method
        self.error = error
        super().__init__(message=f"Anvil RPC call to {method} failed: {error}")
