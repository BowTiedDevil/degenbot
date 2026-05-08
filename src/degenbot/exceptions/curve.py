"""Exceptions for Curve pool operations."""

from degenbot.exceptions.base import DegenbotError


class CurveError(DegenbotError):
    """Base exception for Curve pool errors."""


class MissingCurveData(CurveError):
    """Raised when on-chain data is needed but no fetcher is available."""

    def __init__(
        self,
        pool_address: str,
        data_type: str,
        message: str,
    ) -> None:
        self.pool_address = pool_address
        self.data_type = data_type
        super().__init__(message=message)
