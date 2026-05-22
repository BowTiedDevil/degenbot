"""
Protocols for position analysis I/O boundaries.

PriceFetcher and PositionQuery define the I/O seams that separate
pure calculation (core) from database and RPC access (orchestrator).
"""

from collections.abc import Sequence
from typing import Protocol, runtime_checkable

from eth_typing import ChecksumAddress

from degenbot.aave.analysis.core import (
    CollateralPositionRecord,
    DebtPositionRecord,
    UserRecord,
)


@runtime_checkable
class PriceFetcher(Protocol):
    """Fetch oracle prices for a set of asset addresses."""

    def fetch(self, asset_addresses: set[ChecksumAddress]) -> dict[ChecksumAddress, int]:
        """Fetch prices for the given asset addresses."""


@runtime_checkable
class PositionQuery(Protocol):
    """Query user positions and collateral config from the database."""

    def get_users_with_debt(self, market_id: int, limit: int | None = None) -> Sequence[UserRecord]:
        """Return users with debt."""

    def get_collateral_positions(self, user_id: int) -> Sequence[CollateralPositionRecord]:
        """Return collateral positions."""

    def get_debt_positions(self, user_id: int) -> Sequence[DebtPositionRecord]:
        """Return debt positions."""

    def get_collateral_config_map(self, user_id: int) -> dict[int, bool]:
        """Return collateral config map."""

    def get_oracle_address(self, market_id: int) -> ChecksumAddress | None:
        """Return oracle address."""

    def get_asset_addresses(self, market_id: int) -> set[ChecksumAddress]:
        """Return asset addresses."""
