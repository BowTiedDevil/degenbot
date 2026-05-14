"""Protocols for position analysis I/O boundaries.

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
    """Fetch oracle prices for a set of asset addresses.

    The closure handles I/O (rpc, cache, etc.). The core module
    receives a simple dict mapping addresses to prices.
    """

    def fetch(self, asset_addresses: set[ChecksumAddress]) -> dict[ChecksumAddress, int]: ...


@runtime_checkable
class PositionQuery(Protocol):
    """Query user positions and collateral config from the database.

    The closure handles SQLAlchemy session management. The core
    module receives plain records with no ORM references.
    """

    def get_users_with_debt(
        self, market_id: int, limit: int | None = None
    ) -> Sequence[UserRecord]: ...

    def get_collateral_positions(self, user_id: int) -> Sequence[CollateralPositionRecord]: ...

    def get_debt_positions(self, user_id: int) -> Sequence[DebtPositionRecord]: ...

    def get_collateral_config_map(self, user_id: int) -> dict[int, bool]: ...

    def get_oracle_address(self, market_id: int) -> ChecksumAddress | None: ...

    def get_asset_addresses(self, market_id: int) -> set[ChecksumAddress]: ...
