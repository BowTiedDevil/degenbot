"""
Data fetching exceptions for the degenbot package.

This module contains exceptions related to data fetching operations,
particularly for blockchain log retrieval and other data access operations.
"""

from degenbot.exceptions.base import DegenbotError


class FetchingError(DegenbotError):
    """
    Base exception for data fetching errors.
    """


class LogFetchingTimeout(FetchingError):
    """
    Raised when log fetching operations timeout after multiple retry attempts.
    """

    def __init__(self, max_retries: int) -> None:
        """
        Initialize LogFetchingTimeoutError.

        Args:
            max_retries: The maximum number of retry attempts that were made
        """
        self.max_retries = max_retries
        super().__init__(message=f"Timed out fetching logs after {max_retries} tries.")


class BlockFetchingTimeout(FetchingError):
    """
    Raised when block data fetching operations timeout.
    """

    def __init__(self, max_retries: int) -> None:
        """
        Initialize BlockFetchingTimeoutError.

        Args:
            max_retries: The maximum number of retry attempts that were made
        """
        self.max_retries = max_retries
        super().__init__(message=f"Timed out fetching block data after {max_retries} tries.")
