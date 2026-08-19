"""I/O orchestration for Aave V3 position analysis.

A thin Python driver shell over the Rust ``degenbot-aave::analysis`` core
(ADR-005 three-layer architecture). The pure math (health-factor / LTV /
eMode / isolation / scaled-balance calc) lives in Rust; this module owns only
the I/O orchestration: resolve the DB path, fetch users / positions / prices
via Rust-backed readers, drive the per-user Rust analysis seam, and bucket the
results.

The former ``core.py`` (pure math + dataclasses) + ``protocols.py`` (typing
seams) were retired once the Rust core reached byte-identical parity (verified
by the §4.2 parity gate before deletion). The ``PositionAnalysisResult``
bucket-sorter stays here (it's trivial Python, not math).
"""

from typing import Any

from sqlalchemy.engine import Engine
from sqlalchemy.orm import Session

from degenbot._ffi import ChecksummedAddress
from degenbot._ffi.db import DatabasePositionQuery as _EnginePositionQuery
from degenbot.aave import AavePriceOracle
from degenbot.checksum_cache import get_checksum_address
from degenbot.db import UserPositionSummary, analyze_aave_user_position
from degenbot.logging import logger
from degenbot.provider import AlloyProvider


class DatabasePositionQuery:
    """PositionQuery backed by the Rust ``_EnginePositionQuery`` reader.

    Routes every read through the PyO3 seam (ADR-005). The ``session`` is
    retained for resolving the DB file path + the ``AaveV3Market`` lookup at
    the CLI boundary; reads no longer use the SQLAlchemy session.
    """

    def __init__(self, session: Session) -> None:
        """Initialize the instance.

        Args:
            session: A SQLAlchemy session (used to resolve the database file
                path + look up the ``AaveV3Market`` id at the CLI boundary).

        """
        self._session = session
        self._rust: _EnginePositionQuery | None = None

    def _db_path(self) -> str:
        """Resolve the SQLite file path the Rust reader opens.

        Returns:
            The resolved database file path.

        Raises:
            ValueError: If no file path can be resolved (e.g. an in-memory engine).

        """
        engine = self._session.get_bind()
        if isinstance(engine, Engine):
            db_path = engine.url.database
            if db_path and db_path != ":memory:":
                return db_path
        msg = "a file-backed database is required for Rust-backed position reads"
        raise ValueError(msg)

    def _handle(self) -> _EnginePositionQuery:
        if self._rust is None:
            self._rust = _EnginePositionQuery(self._db_path())
        return self._rust

    def get_users_with_debt(self, market_id: int, limit: int | None = None) -> list[dict[str, Any]]:
        """Get all users with debt positions in a market, as flat dicts.

        Returns:
            The Rust-backed row dicts (address checksummed to match the
            price-map keys the analysis seam consumes).

        """
        return [_checksum_user(r) for r in self._handle().get_users_with_debt(market_id, limit)]

    def get_collateral_positions(self, user_id: int) -> list[dict[str, Any]]:
        """Get collateral positions for a user, as flat dicts.

        Returns:
            The Rust-backed row dicts (address checksummed).

        """
        return [_checksum_collateral(r) for r in self._handle().get_collateral_positions(user_id)]

    def get_debt_positions(self, user_id: int) -> list[dict[str, Any]]:
        """Get debt positions for a user, as flat dicts.

        Returns:
            The Rust-backed row dicts (address checksummed).

        """
        return [_checksum_debt(r) for r in self._handle().get_debt_positions(user_id)]

    def get_collateral_config_map(self, user_id: int) -> dict[int, bool]:
        """Get map of asset_id to enabled status for a user.

        Returns:
            The computed value.

        """
        return self._handle().get_collateral_config_map(user_id)

    def get_oracle_address(self, market_id: int) -> ChecksummedAddress | None:
        """Get the price oracle address for a market.

        Returns:
            The computed value.

        """
        addr = self._handle().get_oracle_address(market_id)
        return get_checksum_address(addr) if addr is not None else None

    def get_asset_addresses(self, market_id: int) -> set[ChecksummedAddress]:
        """Get all unique underlying asset addresses for a market.

        Returns:
            The computed value.

        """
        return {get_checksum_address(a) for a in self._handle().get_asset_addresses(market_id)}


class OraclePriceFetcher:
    """PriceFetcher using the Aave oracle contract.

    Delegating shell over the Rust ``AavePriceOracle`` reader (the
    ``degenbot-price`` core crate, ADR-005). ``getAssetPrice(address)``
    ``eth_call`` + ``uint256`` decode run in Rust, with the same tolerant
    per-asset skip-on-error behavior.
    """

    def __init__(self, provider: AlloyProvider, oracle_address: ChecksummedAddress) -> None:
        """Initialize the instance."""
        self._provider = provider
        self._oracle_address = oracle_address

    def fetch(self, asset_addresses: set[ChecksummedAddress]) -> dict[ChecksummedAddress, int]:
        """Fetch prices for all assets from the Aave oracle.

        Returns:
            The computed value.

        """
        py_oracle = AavePriceOracle(
            self._oracle_address,
            self._provider.to_alloy_provider(),
        )
        # The Rust reader returns EIP-55 checksummed string keys; preserve the
        # input keys exactly (matching the prior Python shape).
        fetched = py_oracle.fetch(list(asset_addresses))
        return {addr: fetched[addr] for addr in asset_addresses if addr in fetched}


class PositionAnalysisResult:
    """Result of analyzing positions for liquidation risk.

    Bucket-sorter only (no math) — the per-user ``UserPositionSummary``
    objects (Rust-built via the analysis seam) are categorized by health factor
    into safe / at-risk / liquidatable lists.
    """

    def __init__(self) -> None:
        """Initialize empty buckets."""
        self.safe_users: list[UserPositionSummary] = []
        self.at_risk_users: list[UserPositionSummary] = []
        self.liquidatable_users: list[UserPositionSummary] = []

    @property
    def total_users(self) -> int:
        """Total users."""
        return len(self.safe_users) + len(self.at_risk_users) + len(self.liquidatable_users)

    @property
    def at_risk_count(self) -> int:
        """At risk count."""
        return len(self.at_risk_users)

    @property
    def liquidatable_count(self) -> int:
        """Liquidatable count."""
        return len(self.liquidatable_users)

    def categorize(
        self, summary: UserPositionSummary, health_factor_threshold: float = 1.1
    ) -> None:
        """Categorize a user summary by health factor.

        Args:
            summary: A ``UserPositionSummary`` (Rust-built).
            health_factor_threshold: The at-risk threshold (default 1.1).

        """
        hf = summary.health_factor
        if hf is not None and hf < 1.0:
            self.liquidatable_users.append(summary)
        elif hf is not None and hf < health_factor_threshold:
            self.at_risk_users.append(summary)
        else:
            self.safe_users.append(summary)

    def sort_by_risk(self) -> None:
        """Sort at-risk + liquidatable users by health factor (lowest first).

        Mirrors the Rust ``PositionAnalysisResult::sort_by_risk`` key: a
        ``None`` OR falsy (``0.0``) health factor sorts as ``+inf`` (the Python
        ``or`` treats ``0.0`` as falsy).
        """
        self.at_risk_users.sort(key=lambda x: x.health_factor or float("inf"))
        self.liquidatable_users.sort(key=lambda x: x.health_factor or float("inf"))


def analyze_positions_for_market(
    session: Session,
    market_id: int,
    health_factor_threshold: float = 1.1,
    limit: int | None = None,
    provider: AlloyProvider | None = None,
) -> PositionAnalysisResult:
    """Analyze all users-with-debt in a market for liquidation risk.

    Thin driver shell: fetches users + prices via Rust-backed readers, drives
    the per-user ``analyze_aave_user_position`` Rust seam, and buckets the
    results. The pure math (HF / LTV / scaled-balance) lives in Rust
    (``degenbot-aave::analysis``).

    Args:
        session: A SQLAlchemy session (for DB path resolution).
        market_id: The Aave V3 market id.
        health_factor_threshold: The at-risk threshold (default 1.1).
        limit: Optional cap on the number of users analyzed.
        provider: Optional RPC provider for price fetching (when ``None``,
            prices are treated as 1 — faster, but HFs are relative).

    Returns:
        The bucketed analysis result.

    """
    position_query = DatabasePositionQuery(session)

    price_map: dict[ChecksummedAddress, int] = {}
    if provider is not None:
        oracle_address = position_query.get_oracle_address(market_id)
        if oracle_address is None:
            logger.warning("Price oracle not found for market, prices will not be applied")
        else:
            price_map = OraclePriceFetcher(provider, oracle_address).fetch(
                position_query.get_asset_addresses(market_id)
            )

    users = position_query.get_users_with_debt(market_id, limit)
    logger.info(f"Analyzing {len(users)} users with debt positions")

    result = PositionAnalysisResult()

    # The price_map is keyed by checksummed ChecksummedAddress (the fetcher's
    # output); the record dicts carry checksummed addresses too (the
    # _checksum_* helpers). Both match — pass the price_map straight through.
    seam_prices: dict[str, int] | None = (
        {str(k): v for k, v in price_map.items()} if price_map else None
    )

    for user in users:
        collateral = position_query.get_collateral_positions(user["id"])
        debt = position_query.get_debt_positions(user["id"])
        cfg = position_query.get_collateral_config_map(user["id"])

        summary = analyze_aave_user_position(user, collateral, debt, cfg, seam_prices)
        result.categorize(summary, health_factor_threshold)

    result.sort_by_risk()

    logger.info(
        f"Analysis complete: {len(result.safe_users)} safe, "
        f"{len(result.at_risk_users)} at risk, "
        f"{len(result.liquidatable_users)} liquidatable",
    )

    return result


def _checksum_user(row: dict[str, Any]) -> dict[str, Any]:
    """Normalize the user address to a ChecksummedAddress.

    Returns:
        The row with its ``address`` field EIP-55 checksummed.

    """
    row = dict(row)
    row["address"] = get_checksum_address(row["address"])
    return row


def _checksum_collateral(row: dict[str, Any]) -> dict[str, Any]:
    """Normalize the underlying address to a ChecksummedAddress.

    Returns:
        The row with its ``underlying_address`` field EIP-55 checksummed.

    """
    row = dict(row)
    row["underlying_address"] = get_checksum_address(row["underlying_address"])
    return row


def _checksum_debt(row: dict[str, Any]) -> dict[str, Any]:
    """Normalize the underlying address to a ChecksummedAddress.

    Returns:
        The row with its ``underlying_address`` field EIP-55 checksummed.

    """
    row = dict(row)
    row["underlying_address"] = get_checksum_address(row["underlying_address"])
    return row
