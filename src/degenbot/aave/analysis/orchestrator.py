"""Orchestrator that assembles data and delegates to core functions.

This is where I/O lives: database queries and oracle price fetching.
The orchestrator converts ORM objects to flat records before passing
them to core.
"""

from collections.abc import Sequence
from typing import Any

from eth_typing import ChecksumAddress
from sqlalchemy.engine import Engine
from sqlalchemy.orm import Session

from degenbot._ffi import PyAavePriceOracle, PyDatabasePositionQuery
from degenbot.aave.analysis.core import (
    CollateralPositionRecord,
    DebtPositionRecord,
    PositionAnalysisResult,
    UserRecord,
    analyze_user_position,
)
from degenbot.aave.analysis.protocols import PositionQuery, PriceFetcher
from degenbot.checksum_cache import get_checksum_address
from degenbot.logging import logger
from degenbot.provider import AlloyProvider


class DatabasePositionQuery:
    """PositionQuery implementation backed by the Rust `degenbot-db` core.

    Routes every read through the `PyDatabasePositionQuery` PyO3 seam (ADR-005
    three-layer architecture). The `session` is retained for explicit-deps
    construction + resolving the DB file path + the `AaveV3Market` lookup (the
    market id is an I/O detail resolved at the CLI boundary); reads no longer
    use the SQLAlchemy session.
    """

    def __init__(self, session: Session) -> None:
        """Initialize the instance.

        Args:
            session: A SQLAlchemy session (used to resolve the database file
                path + look up the `AaveV3Market` id at the CLI boundary).

        """
        self._session = session
        self._rust: PyDatabasePositionQuery | None = None

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

    def _handle(self) -> PyDatabasePositionQuery:
        if self._rust is None:
            self._rust = PyDatabasePositionQuery(self._db_path())
        return self._rust

    def get_users_with_debt(self, market_id: int, limit: int | None = None) -> Sequence[UserRecord]:
        """Get all users with debt positions in a market, as flat records.

        Returns:
            The computed value.

        """
        rows = self._handle().get_users_with_debt(market_id, limit)
        return [UserRecord(**_checksum_user(r)) for r in rows]

    def get_collateral_positions(self, user_id: int) -> Sequence[CollateralPositionRecord]:
        """Get collateral positions for a user, as flat records.

        Returns:
            The computed value.

        """
        rows = self._handle().get_collateral_positions(user_id)
        return [CollateralPositionRecord(**_checksum_collateral(r)) for r in rows]

    def get_debt_positions(self, user_id: int) -> Sequence[DebtPositionRecord]:
        """Get debt positions for a user, as flat records.

        Returns:
            The computed value.

        """
        rows = self._handle().get_debt_positions(user_id)
        return [DebtPositionRecord(**_checksum_debt(r)) for r in rows]

    def get_collateral_config_map(self, user_id: int) -> dict[int, bool]:
        """Get map of asset_id to enabled status for a user.

        Returns:
            The computed value.

        """
        return self._handle().get_collateral_config_map(user_id)

    def get_oracle_address(self, market_id: int) -> ChecksumAddress | None:
        """Get the price oracle address for a market.

        Returns:
            The computed value.

        """
        addr = self._handle().get_oracle_address(market_id)
        return get_checksum_address(addr) if addr is not None else None

    def get_asset_addresses(self, market_id: int) -> set[ChecksumAddress]:
        """Get all unique underlying asset addresses for a market.

        Returns:
            The computed value.

        """
        return {get_checksum_address(a) for a in self._handle().get_asset_addresses(market_id)}


def _checksum_user(row: dict[str, Any]) -> dict[str, Any]:
    """Normalize the user address to a ``ChecksumAddress``.

    Returns:
        The row with its ``address`` field EIP-55 checksummed.

    """
    row = dict(row)
    row["address"] = get_checksum_address(row["address"])
    return row


def _checksum_collateral(row: dict[str, Any]) -> dict[str, Any]:
    """Normalize the underlying address to a ``ChecksumAddress``.

    Returns:
        The row with its ``underlying_address`` field EIP-55 checksummed.

    """
    row = dict(row)
    row["underlying_address"] = get_checksum_address(row["underlying_address"])
    return row


def _checksum_debt(row: dict[str, Any]) -> dict[str, Any]:
    """Normalize the underlying address to a ``ChecksumAddress``.

    Returns:
        The row with its ``underlying_address`` field EIP-55 checksummed.

    """
    row = dict(row)
    row["underlying_address"] = get_checksum_address(row["underlying_address"])
    return row


class OraclePriceFetcher:
    """PriceFetcher implementation using the Aave oracle contract.

    Delegating shell over the Rust ``PyAavePriceOracle`` reader (the
    ``degenbot-price`` core crate, ADR-005). The inline ``raw_call`` /
    ``encode_function_calldata`` loop is retired — ``getAssetPrice(address)``
    ``eth_call`` + ``uint256`` decode now run in Rust, with the same tolerant
    per-asset skip-on-error behavior (the Rust core logs + skips reverts /
    decode failures, matching the prior ``ContractLogicError`` / ``ValueError``
    catch).
    """

    def __init__(self, provider: AlloyProvider, oracle_address: ChecksumAddress) -> None:
        """Initialize the instance."""
        self._provider = provider
        self._oracle_address = oracle_address

    def fetch(self, asset_addresses: set[ChecksumAddress]) -> dict[ChecksumAddress, int]:
        """Fetch prices for all assets from the Aave oracle.

        Returns:
            The computed value.

        """
        py_oracle = PyAavePriceOracle(
            self._oracle_address,
            self._provider.to_alloy_provider(),
        )
        # The Rust reader returns EIP-55 checksummed string keys; the inputs
        # were already checksummed ``ChecksumAddress`` values, so each input
        # key is present verbatim — preserve the input keys exactly (matching
        # the prior Python ``prices[asset_address] = price`` shape).
        fetched = py_oracle.fetch(list(asset_addresses))
        return {addr: fetched[addr] for addr in asset_addresses if addr in fetched}


class PositionAnalysisService:
    """Service that coordinates position analysis with I/O.

    Created by Bot or CLI. Injects fetchers for testing.
    Orchestrator converts ORM → flat records, then delegates to core.
    """

    def __init__(
        self,
        position_query: PositionQuery,
        price_fetcher: PriceFetcher | None = None,
    ) -> None:
        """Initialize the instance."""
        self.position_query = position_query
        self.price_fetcher = price_fetcher

    def analyze_market(
        self,
        market_id: int,
        health_factor_threshold: float = 1.1,
        limit: int | None = None,
    ) -> PositionAnalysisResult:
        """Entry point: call I/O, then delegate to core.

        Returns:
            The computed value.

        """
        result = PositionAnalysisResult()

        # Query users with debt
        users = self.position_query.get_users_with_debt(market_id, limit)

        # Fetch prices if fetcher available
        price_map: dict[ChecksumAddress, int] = {}
        if self.price_fetcher is not None:
            asset_addresses = self.position_query.get_asset_addresses(market_id)
            if asset_addresses:
                price_map = self.price_fetcher.fetch(asset_addresses)

        logger.info(f"Analyzing {len(users)} users with debt positions")

        # Analyze each user via core
        for user in users:
            collateral = self.position_query.get_collateral_positions(user.id)
            debt = self.position_query.get_debt_positions(user.id)
            config_map = self.position_query.get_collateral_config_map(user.id)

            summary = analyze_user_position(
                user=user,
                collateral_positions=list(collateral),
                debt_positions=list(debt),
                collateral_config_map=config_map,
                price_map=price_map,
            )

            result.categorize(summary, health_factor_threshold)

        result.sort_by_risk()

        logger.info(
            f"Analysis complete: {len(result.safe_users)} safe, "
            f"{len(result.at_risk_users)} at risk, "
            f"{len(result.liquidatable_users)} liquidatable",
        )

        return result


def analyze_positions_for_market(
    session: Session,
    market_id: int,
    health_factor_threshold: float = 1.1,
    limit: int | None = None,
    provider: AlloyProvider | None = None,
) -> PositionAnalysisResult:
    """Backward-compatible entry point.

    Creates a PositionAnalysisService from session + optional provider,
    then delegates to analyze_market().

    Returns:
        The computed value.

    """
    position_query = DatabasePositionQuery(session)

    price_fetcher: PriceFetcher | None = None
    if provider is not None:
        oracle_address = position_query.get_oracle_address(market_id)
        if oracle_address is None:
            logger.warning("Price oracle not found for market, prices will not be applied")
        else:
            price_fetcher = OraclePriceFetcher(provider, oracle_address)

    service = PositionAnalysisService(
        position_query=position_query,
        price_fetcher=price_fetcher,
    )
    return service.analyze_market(market_id, health_factor_threshold, limit)
