"""Orchestrator that assembles data and delegates to core functions.

This is where I/O lives: database queries and oracle price fetching.
The orchestrator converts ORM objects to flat records before passing
them to core.
"""

from collections.abc import Sequence

from eth_typing import ChecksumAddress
from sqlalchemy import select
from sqlalchemy.orm import Session, joinedload
from web3 import Web3
from web3.exceptions import ContractLogicError

from degenbot.aave.analysis.core import (
    CollateralPositionRecord,
    DebtPositionRecord,
    PositionAnalysisResult,
    UserRecord,
    analyze_user_position,
)
from degenbot.aave.analysis.protocols import PositionQuery, PriceFetcher
from degenbot.database.models.aave import (
    AaveV3Asset,
    AaveV3CollateralPosition,
    AaveV3Contract,
    AaveV3DebtPosition,
    AaveV3User,
    AaveV3UserCollateralConfig,
)
from degenbot.logging import logger
from degenbot.provider.call_helpers import encode_function_calldata, raw_call
from degenbot.provider.interface import ProviderAdapter

# Contract name for price oracle in aave_v3_contracts table
ORACLE_CONTRACT_NAME = "PRICE_ORACLE"


class DatabasePositionQuery:
    """PositionQuery implementation backed by SQLAlchemy.

    Converts ORM objects to flat records at the query boundary.
    Core never sees ORM objects.
    """

    def __init__(self, session: Session) -> None:
        """Initialize the instance."""
        self._session = session

    def get_users_with_debt(self, market_id: int, limit: int | None = None) -> Sequence[UserRecord]:
        """Get all users with debt positions in a market, as flat records."""
        users_orm = (
            self._session
            .scalars(
                select(AaveV3User)
                .where(AaveV3User.market_id == market_id)
                .options(
                    joinedload(AaveV3User.debt_positions),
                )
            )
            .unique()
            .all()
        )

        # Filter to users with actual debt
        users_with_debt = [u for u in users_orm if len(u.debt_positions) > 0]

        if limit is not None:
            users_with_debt = users_with_debt[:limit]

        return [_convert_user(u) for u in users_with_debt]

    def get_collateral_positions(self, user_id: int) -> Sequence[CollateralPositionRecord]:
        """Get collateral positions for a user, as flat records."""
        positions = self._session.scalars(
            select(AaveV3CollateralPosition)
            .where(AaveV3CollateralPosition.user_id == user_id)
            .options(
                joinedload(AaveV3CollateralPosition.asset).joinedload(AaveV3Asset.underlying_token),
                joinedload(AaveV3CollateralPosition.asset).joinedload(AaveV3Asset.asset_config),
                joinedload(AaveV3CollateralPosition.asset).joinedload(AaveV3Asset.e_mode_category),
            )
        ).all()

        return [_convert_collateral_position(p) for p in positions]

    def get_debt_positions(self, user_id: int) -> Sequence[DebtPositionRecord]:
        """Get debt positions for a user, as flat records."""
        positions = self._session.scalars(
            select(AaveV3DebtPosition)
            .where(AaveV3DebtPosition.user_id == user_id)
            .options(
                joinedload(AaveV3DebtPosition.asset).joinedload(AaveV3Asset.underlying_token),
                joinedload(AaveV3DebtPosition.asset).joinedload(AaveV3Asset.e_mode_category),
            )
        ).all()

        return [_convert_debt_position(p) for p in positions]

    def get_collateral_config_map(self, user_id: int) -> dict[int, bool]:
        """Get map of asset_id -> enabled status for a user."""
        configs = self._session.scalars(
            select(AaveV3UserCollateralConfig).where(AaveV3UserCollateralConfig.user_id == user_id)
        ).all()
        return {cfg.asset_id: cfg.enabled for cfg in configs}

    def get_oracle_address(self, market_id: int) -> ChecksumAddress | None:
        """Get the price oracle address for a market."""
        contract = self._session.scalar(
            select(AaveV3Contract).where(
                AaveV3Contract.market_id == market_id,
                AaveV3Contract.name == ORACLE_CONTRACT_NAME,
            )
        )
        return contract.address if contract else None

    def get_asset_addresses(self, market_id: int) -> set[ChecksumAddress]:
        """Get all unique underlying asset addresses for a market."""
        assets = (
            self._session
            .scalars(
                select(AaveV3Asset)
                .where(AaveV3Asset.market_id == market_id)
                .options(
                    joinedload(AaveV3Asset.underlying_token),
                )
            )
            .unique()
            .all()
        )

        addresses: set[ChecksumAddress] = set()
        for asset in assets:
            if asset.underlying_token is not None:
                addresses.add(asset.underlying_token.address)

        return addresses


def _convert_user(user: AaveV3User) -> UserRecord:
    """Convert ORM AaveV3User to flat UserRecord."""
    isolation_debt_ceiling = None
    if user.is_isolation_mode and user.isolation_collateral_asset is not None:
        asset_config = user.isolation_collateral_asset.asset_config
        if asset_config is not None:
            isolation_debt_ceiling = asset_config.debt_ceiling

    return UserRecord(
        id=user.id,
        address=user.address,
        market_id=user.market_id,
        e_mode=user.e_mode,
        is_isolation_mode=user.is_isolation_mode,
        isolation_mode_debt=user.isolation_mode_debt,
        isolation_debt_ceiling=isolation_debt_ceiling,
    )


def _convert_collateral_position(pos: AaveV3CollateralPosition) -> CollateralPositionRecord:
    """Convert ORM AaveV3CollateralPosition to flat CollateralPositionRecord."""
    asset = pos.asset
    emode_cat_id = asset.e_mode_category_id or None

    emode_lt = None
    emode_ltv = None
    if asset.e_mode_category is not None:
        emode_lt = asset.e_mode_category.liquidation_threshold
        emode_ltv = asset.e_mode_category.ltv

    asset_lt = 0
    asset_ltv = 0
    if asset.asset_config is not None:
        asset_lt = asset.asset_config.liquidation_threshold
        asset_ltv = asset.asset_config.ltv

    zero_addr = ChecksumAddress(Web3.to_checksum_address("0x" + "0" * 40))
    underlying_address = asset.underlying_token.address if asset.underlying_token else zero_addr
    underlying_symbol = asset.underlying_token.symbol if asset.underlying_token else None

    return CollateralPositionRecord(
        asset_id=pos.asset_id,
        balance=pos.balance,
        underlying_address=underlying_address,
        underlying_symbol=underlying_symbol,
        liquidity_index=asset.liquidity_index,
        e_mode_category_id=emode_cat_id,
        asset_lt=asset_lt,
        asset_ltv=asset_ltv,
        emode_lt=emode_lt,
        emode_ltv=emode_ltv,
    )


def _convert_debt_position(pos: AaveV3DebtPosition) -> DebtPositionRecord:
    """Convert ORM AaveV3DebtPosition to flat DebtPositionRecord."""
    asset = pos.asset
    emode_cat_id = asset.e_mode_category_id or None

    zero_addr = ChecksumAddress(Web3.to_checksum_address("0x" + "0" * 40))
    underlying_address = asset.underlying_token.address if asset.underlying_token else zero_addr
    underlying_symbol = asset.underlying_token.symbol if asset.underlying_token else None

    return DebtPositionRecord(
        asset_id=pos.asset_id,
        balance=pos.balance,
        underlying_address=underlying_address,
        underlying_symbol=underlying_symbol,
        borrow_index=asset.borrow_index,
        e_mode_category_id=emode_cat_id,
    )


class OraclePriceFetcher:
    """PriceFetcher implementation using the Aave oracle contract."""

    def __init__(self, provider: ProviderAdapter, oracle_address: ChecksumAddress) -> None:
        """Initialize the instance."""
        self._provider = provider
        self._oracle_address = oracle_address

    def fetch(self, asset_addresses: set[ChecksumAddress]) -> dict[ChecksumAddress, int]:
        """Fetch prices for all assets from the Aave oracle."""
        prices: dict[ChecksumAddress, int] = {}

        for asset_address in asset_addresses:
            try:
                (price,) = raw_call(
                    provider=self._provider,
                    address=self._oracle_address,
                    calldata=encode_function_calldata(
                        function_prototype="getAssetPrice(address)",
                        function_arguments=[asset_address],
                    ),
                    return_types=["uint256"],
                )
                prices[asset_address] = price
            except (ContractLogicError, ValueError) as e:
                logger.warning(f"Failed to fetch price for {asset_address}: {e}")
                continue

        logger.info(f"Fetched prices for {len(prices)}/{len(asset_addresses)} assets")
        return prices


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
        """Entry point: call I/O, then delegate to core."""
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
            f"{len(result.liquidatable_users)} liquidatable"
        )

        return result


def analyze_positions_for_market(
    session: Session,
    market_id: int,
    health_factor_threshold: float = 1.1,
    limit: int | None = None,
    provider: ProviderAdapter | None = None,
) -> PositionAnalysisResult:
    """Backward-compatible entry point.

    Creates a PositionAnalysisService from session + optional provider,
    then delegates to analyze_market().
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
