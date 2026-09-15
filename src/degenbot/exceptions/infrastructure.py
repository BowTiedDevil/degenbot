"""Infrastructure and token exceptions.

Includes exceptions for:
- Database operations (BackupExists)
- Anvil fork operations (AnvilError)
- ERC-20 token operations (Erc20TokenError, NoPriceOracle)
"""

import pathlib

from degenbot.exceptions.base import DegenbotError

# --- Database ---


class BackupExists(DegenbotError):
    """Raised by `degenbot database backup` if a file exists at the target path."""

    def __init__(self, path: pathlib.Path) -> None:
        """Initialize the instance."""
        self.path = path
        super().__init__(message=f"A backup at {path} already exists.")


# --- Anvil ---


class AnvilError(DegenbotError):
    """Raised on errors resulting from failed calls to Anvil via JSON-RPC.

    This exception is specifically for errors that occur when making RPC calls
    to an Anvil instance, such as invalid method calls, parameter errors,
    or other Anvil-specific failures.
    """

    def __init__(self, method: str, error: str) -> None:
        """Initialize the instance."""
        self.method = method
        self.error = error
        super().__init__(message=f"Anvil RPC call to {method} failed: {error}")


# --- ERC-20 Tokens ---


class Erc20TokenError(DegenbotError):
    """Exception raised inside ERC-20 token helpers."""


class NoPriceOracle(Erc20TokenError):
    """Raised when `.price` is called on a token without a price oracle."""

    def __init__(self) -> None:
        """Initialize the instance."""
        super().__init__(message="Token does not have a price oracle.")
