"""
Connection-related exceptions for the degenbot package.

This module contains exceptions related to connection issues, timeouts,
and other connectivity problems.
"""

from degenbot.exceptions.base import DegenbotError


class DegenbotConnectionError(DegenbotError):
    """
    Base exception for connection-related errors.
    """


class ConnectionTimeout(DegenbotConnectionError):
    """
    Raised when a connection attempt times out.
    """

    def __init__(self, resource: str, timeout_seconds: int | None = None) -> None:
        """
        Initialize ConnectionTimeoutError.

        Args:
            resource: The resource that failed to connect (e.g., "Web3", "IPC socket")
            timeout_seconds: The timeout duration in seconds, if known
        """
        self.resource = resource
        self.timeout_seconds = timeout_seconds

        message = f"Timed out waiting for {resource} connection"
        if timeout_seconds is not None:
            message += f" after {timeout_seconds} seconds"
        message += "."

        super().__init__(message=message)


class IPCSocketTimeout(ConnectionTimeout):
    """
    Raised when an IPC socket creation times out.
    """

    def __init__(self, timeout_seconds: int | None = None) -> None:
        super().__init__(resource="IPC socket", timeout_seconds=timeout_seconds)


class Web3ConnectionTimeout(ConnectionTimeout):
    """
    Raised when a Web3 connection times out.
    """

    def __init__(self, timeout_seconds: int | None = None) -> None:
        super().__init__(resource="Web3", timeout_seconds=timeout_seconds)
