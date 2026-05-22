"""Infrastructure and token exceptions.

Includes exceptions for:
- RPC connections (DegenbotConnectionError, ConnectionTimeout, ...)
- Data fetching (FetchingError, LogFetchingTimeout, BlockFetchingTimeout)
- Registry operations (RegistryError, RegistryAlreadyInitialized)
- Database operations (BackupExists)
- Anvil fork operations (AnvilError)
- ERC-20 token operations (Erc20TokenError, NoPriceOracle)
"""

import pathlib

from degenbot.exceptions.base import DegenbotError

# --- Connections ---


class DegenbotConnectionError(DegenbotError):
    """Base exception for connection-related errors."""


class ConnectionTimeout(DegenbotConnectionError):
    """Raised when a connection attempt times out."""

    def __init__(self, resource: str, timeout_seconds: int | None = None) -> None:
        """Initialize the instance."""
        self.resource = resource
        self.timeout_seconds = timeout_seconds

        message = f"Timed out waiting for {resource} connection"
        if timeout_seconds is not None:
            message += f" after {timeout_seconds} seconds"
        message += "."

        super().__init__(message=message)


class IPCSocketTimeout(ConnectionTimeout):
    """Raised when an IPC socket creation times out."""

    def __init__(self, timeout_seconds: int | None = None) -> None:
        """Initialize the instance."""
        super().__init__(resource="IPC socket", timeout_seconds=timeout_seconds)


class Web3ConnectionTimeout(ConnectionTimeout):
    """Raised when a Web3 connection times out."""

    def __init__(self, timeout_seconds: int | None = None) -> None:
        """Initialize the instance."""
        super().__init__(resource="Web3", timeout_seconds=timeout_seconds)


# --- Data Fetching ---


class FetchingError(DegenbotError):
    """Base exception for data fetching errors."""


class LogFetchingTimeout(FetchingError):
    """Raised when log fetching operations timeout after multiple retry attempts."""

    def __init__(self, max_retries: int) -> None:
        """Initialize the instance."""
        self.max_retries = max_retries
        super().__init__(message=f"Timed out fetching logs after {max_retries} tries.")


class BlockFetchingTimeout(FetchingError):
    """Raised when block data fetching operations timeout."""

    def __init__(self, max_retries: int) -> None:
        """Initialize the instance."""
        self.max_retries = max_retries
        super().__init__(message=f"Timed out fetching block data after {max_retries} tries.")


# --- Registries ---


class RegistryError(DegenbotError):
    """Exception raised inside registries."""


class RegistryAlreadyInitialized(RegistryError):
    """Raised by a singleton registry if a caller attempts to recreate it."""


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


# --- Subscriptions ---


class SubscriptionError(DegenbotError):
    """Base exception for subscription-related errors."""


class SubscriptionNotSupported(SubscriptionError):
    """Raised when subscribe_*() is called on a provider that doesn't support.

    eth_subscribe (e.g., HTTP transport).
    """

    def __init__(self, transport: str, rpc_url: str) -> None:
        """Initialize the instance."""
        self.transport = transport
        self.rpc_url = rpc_url
        msg = (
            f"Subscriptions require a WebSocket or IPC transport. "
            f"This provider was created with {transport} transport "
            f"('{rpc_url}'). "
            f"Use AsyncProviderAdapter with a WS/IPC URL instead."
        )
        super().__init__(message=msg)


class SubscriptionDisconnected(SubscriptionError):
    """Raised when the WS/IPC connection drops during an active subscription."""

    def __init__(self, message: str, *, rpc_url: str | None = None) -> None:
        """Initialize the instance."""
        self.rpc_url = rpc_url
        detail = message
        if rpc_url is not None:
            detail += f" (RPC URL: {rpc_url})"
        super().__init__(message=detail)
