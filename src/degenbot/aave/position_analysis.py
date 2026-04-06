"""
Aave V3 position analysis for liquidation risk monitoring.

This module provides tools to analyze user positions and identify accounts
near liquidation risk. Health factor calculations follow Aave V3 contract logic.

Health Factor = Sum(collateral * liquidation_threshold * price) / Sum(debt * price)
- HF > 1: Position is safe
- HF < 1: Position can be liquidated
- HF approaching 1: Position at risk

The calculation accounts for:
- Standard collateral with user-specific enable/disable preferences
- eMode categories (enhanced LTV/thresholds for correlated assets)
- Isolation mode (debt ceiling for isolated collateral)
- Asset prices from Aave oracle (converts to common currency)
"""

from dataclasses import dataclass, field

from eth_typing import ChecksumAddress
from sqlalchemy import select
from sqlalchemy.orm import Session, joinedload
from web3.exceptions import ContractLogicError

from degenbot.aave.libraries.wad_ray_math import ray_mul_ceil, ray_mul_floor
from degenbot.constants import ZERO_ADDRESS
from degenbot.database.models.aave import (
    AaveV3Asset,
    AaveV3CollateralPosition,
    AaveV3Contract,
    AaveV3DebtPosition,
    AaveV3User,
    AaveV3UserCollateralConfig,
)
from degenbot.functions import encode_function_calldata, raw_call
from degenbot.logging import logger
from degenbot.provider.interface import ProviderAdapter

# Basis points constant (10000 = 100%)
BASIS_POINTS = 10000

# Health factor thresholds
HEALTH_FACTOR_AT_RISK_THRESHOLD = 1.1  # 10% buffer
HEALTH_FACTOR_LIQUIDATABLE_THRESHOLD = 1.0

# Oracle price decimals (Chainlink-style 8 decimals)
ORACLE_PRICE_DECIMALS = 8

# Contract name for price oracle in aave_v3_contracts table
ORACLE_CONTRACT_NAME = "PRICE_ORACLE"


@dataclass(frozen=True)
class CollateralPositionData:
    """
    Collateral position with calculated values for risk analysis.
    """

    asset_address: ChecksumAddress
    asset_symbol: str | None
    scaled_balance: int
    actual_balance: int
    liquidation_threshold: int  # Basis points (e.g., 8000 = 80%)
    ltv: int  # Basis points
    is_enabled_as_collateral: bool
    in_emode: bool
    emode_category_id: int | None = None
    price: int | None = None  # Oracle price (8 decimals), None if unavailable

    @property
    def effective_liquidation_threshold(self) -> int:
        """
        Liquidation threshold used for health factor calculation.

        Returns 0 if collateral is disabled by user preference.
        """
        return self.liquidation_threshold if self.is_enabled_as_collateral else 0

    @property
    def price_adjusted_balance(self) -> int:
        """
        Balance adjusted by price, in oracle base currency units.

        Returns actual_balance * price (with proper scaling).
        If price is None, returns actual_balance (assumes price = 1).
        """
        if self.price is None:
            return self.actual_balance
        return self.actual_balance * self.price


@dataclass(frozen=True)
class DebtPositionData:
    """
    Debt position with calculated values for risk analysis.
    """

    asset_address: ChecksumAddress
    asset_symbol: str | None
    scaled_balance: int
    actual_balance: int
    stable_debt: bool = False  # For future use if stable debt is tracked
    in_emode: bool = False
    emode_category_id: int | None = None
    price: int | None = None  # Oracle price (8 decimals), None if unavailable

    @property
    def price_adjusted_balance(self) -> int:
        """
        Balance adjusted by price, in oracle base currency units.

        Returns actual_balance * price (with proper scaling).
        If price is None, returns actual_balance (assumes price = 1).
        """
        if self.price is None:
            return self.actual_balance
        return self.actual_balance * self.price


@dataclass(frozen=True)
class UserPositionSummary:
    """
    Summary of a user's Aave V3 position for liquidation risk assessment.
    """

    user_address: ChecksumAddress
    market_id: int
    emode_category_id: int | None
    is_isolation_mode: bool

    # Position data
    collateral_positions: tuple[CollateralPositionData, ...]
    debt_positions: tuple[DebtPositionData, ...]

    # Calculated values in oracle base currency
    total_collateral_value: float = 0.0
    total_debt_value: float = 0.0
    weighted_collateral_value: float = 0.0  # Collateral * liquidation_threshold

    # Health metrics
    health_factor: float | None = None  # None if no debt
    max_ltv_ratio: float | None = None  # Current debt / max LTV capacity

    @property
    def is_at_risk(self) -> bool:
        """
        True if position is near liquidation (health factor < threshold).
        """
        if self.health_factor is None:
            return False
        return self.health_factor < HEALTH_FACTOR_AT_RISK_THRESHOLD

    @property
    def is_liquidatable(self) -> bool:
        """
        True if position can be liquidated (health factor < 1).
        """
        if self.health_factor is None:
            return False
        return self.health_factor < HEALTH_FACTOR_LIQUIDATABLE_THRESHOLD

    @property
    def has_debt(self) -> bool:
        """
        True if user has any debt positions.
        """
        return len(self.debt_positions) > 0


@dataclass
class PositionAnalysisResult:
    """
    Result of analyzing positions for liquidation risk.
    """

    safe_users: list[UserPositionSummary] = field(default_factory=list)
    at_risk_users: list[UserPositionSummary] = field(default_factory=list)
    liquidatable_users: list[UserPositionSummary] = field(default_factory=list)

    @property
    def total_users(self) -> int:
        return len(self.safe_users) + len(self.at_risk_users) + len(self.liquidatable_users)

    @property
    def at_risk_count(self) -> int:
        return len(self.at_risk_users)

    @property
    def liquidatable_count(self) -> int:
        return len(self.liquidatable_users)


def calculate_actual_collateral_balance(
    scaled_balance: int,
    liquidity_index: int,
) -> int:
    """
    Calculate actual collateral balance from scaled balance.

    Uses floor rounding (ray_mul_floor) to match Aave V3 behavior.
    Collateral balance should not be over-accounted.

    Args:
        scaled_balance: The scaled balance from the position record
        liquidity_index: Current liquidity index for the asset

    Returns:
        Actual collateral balance in underlying token units
    """
    return ray_mul_floor(scaled_balance, liquidity_index)


def calculate_actual_debt_balance(
    scaled_balance: int,
    borrow_index: int,
) -> int:
    """
    Calculate actual debt balance from scaled balance.

    Uses ceil rounding (ray_mul_ceil) to match Aave V3 behavior.
    Debt should not be under-accounted.

    Args:
        scaled_balance: The scaled balance from the position record
        borrow_index: Current borrow index for the asset

    Returns:
        Actual debt balance in underlying token units
    """
    return ray_mul_ceil(scaled_balance, borrow_index)


def get_oracle_address_for_market(session: Session, market_id: int) -> ChecksumAddress | None:
    """
    Get the price oracle address for a market.

    Args:
        session: SQLAlchemy session
        market_id: Market ID to query

    Returns:
        Oracle contract address, or None if not found
    """
    contract = session.scalar(
        select(AaveV3Contract).where(
            AaveV3Contract.market_id == market_id,
            AaveV3Contract.name == ORACLE_CONTRACT_NAME,
        )
    )
    return contract.address if contract else None


def fetch_asset_prices(
    provider: ProviderAdapter,
    oracle_address: ChecksumAddress,
    asset_addresses: set[ChecksumAddress],
) -> dict[ChecksumAddress, int]:
    """
    Fetch prices for all assets from the Aave oracle.

    Uses batch calls to minimize RPC requests.

    Args:
        provider: ProviderAdapter instance
        oracle_address: Price oracle contract address
        asset_addresses: Set of asset addresses to fetch prices for

    Returns:
        Dict mapping asset address to price (8 decimals)
    """
    prices: dict[ChecksumAddress, int] = {}

    for asset_address in asset_addresses:
        try:
            (price,) = raw_call(
                w3=provider,
                address=oracle_address,
                calldata=encode_function_calldata(
                    function_prototype="getAssetPrice(address)",
                    function_arguments=[asset_address],
                ),
                return_types=["uint256"],
            )
            prices[asset_address] = price
        except (ContractLogicError, ValueError) as e:
            logger.warning(f"Failed to fetch price for {asset_address}: {e}")
            # Skip assets with missing prices - they'll use price=None
            continue

    logger.info(f"Fetched prices for {len(prices)}/{len(asset_addresses)} assets")
    return prices


def get_liquidation_threshold_for_position(
    asset: AaveV3Asset,
    emode_category_id: int | None,
    user_emode: int,
) -> int:
    """
    Get the effective liquidation threshold for an asset position.

    If the user is in eMode and the asset is in the same eMode category,
    use the eMode liquidation threshold. Otherwise use the standard threshold.

    Args:
        asset: The Aave V3 asset record
        emode_category_id: The asset's eMode category (or None)
        user_emode: The user's active eMode category (0 = no eMode)

    Returns:
        Liquidation threshold in basis points
    """
    # Check if user is in eMode and asset belongs to that category
    if user_emode > 0 and emode_category_id == user_emode and asset.e_mode_category is not None:
        return asset.e_mode_category.liquidation_threshold

    # Use standard asset config threshold
    return asset.asset_config.liquidation_threshold if asset.asset_config else 0


def get_ltv_for_position(
    asset: AaveV3Asset,
    emode_category_id: int | None,
    user_emode: int,
) -> int:
    """
    Get the effective LTV for an asset position.

    If the user is in eMode and the asset is in the same eMode category,
    use the eMode LTV. Otherwise use the standard LTV.

    Args:
        asset: The Aave V3 asset record
        emode_category_id: The asset's eMode category (or None)
        user_emode: The user's active eMode category (0 = no eMode)

    Returns:
        LTV in basis points
    """
    if user_emode > 0 and emode_category_id == user_emode and asset.e_mode_category is not None:
        return asset.e_mode_category.ltv

    return asset.asset_config.ltv if asset.asset_config else 0


def build_collateral_position_data(
    position: AaveV3CollateralPosition,
    user_emode: int,
    *,
    collateral_enabled: bool,
    price: int | None = None,
) -> CollateralPositionData:
    """
    Build CollateralPositionData from database records.

    Args:
        position: The collateral position database record
        user_emode: The user's active eMode category
        collateral_enabled: Whether user has enabled this asset as collateral
        price: Oracle price for the asset (8 decimals), None if unavailable

    Returns:
        CollateralPositionData with calculated values
    """
    asset = position.asset

    # Get asset addresses - use ZERO_ADDRESS as fallback if no underlying token
    # This should never happen in production data but provides type safety
    underlying_address = asset.underlying_token.address if asset.underlying_token else ZERO_ADDRESS

    # Calculate actual balance
    actual_balance = calculate_actual_collateral_balance(
        scaled_balance=position.balance,
        liquidity_index=asset.liquidity_index,
    )

    # Get eMode category ID for this asset
    emode_cat_id = asset.e_mode_category_id or None

    # Determine liquidation threshold and LTV
    lt = get_liquidation_threshold_for_position(
        asset=asset,
        emode_category_id=emode_cat_id,
        user_emode=user_emode,
    )
    ltv = get_ltv_for_position(
        asset=asset,
        emode_category_id=emode_cat_id,
        user_emode=user_emode,
    )

    # Check if this position is in eMode
    in_emode = user_emode > 0 and emode_cat_id == user_emode

    return CollateralPositionData(
        asset_address=underlying_address,
        asset_symbol=asset.underlying_token.symbol if asset.underlying_token else None,
        scaled_balance=position.balance,
        actual_balance=actual_balance,
        liquidation_threshold=lt,
        ltv=ltv,
        is_enabled_as_collateral=collateral_enabled,
        in_emode=in_emode,
        emode_category_id=emode_cat_id,
        price=price,
    )


def build_debt_position_data(
    position: AaveV3DebtPosition,
    user_emode: int,
    *,
    price: int | None = None,
) -> DebtPositionData:
    """
    Build DebtPositionData from database records.

    Args:
        position: The debt position database record
        user_emode: The user's active eMode category
        price: Oracle price for the asset (8 decimals), None if unavailable

    Returns:
        DebtPositionData with calculated values
    """
    asset = position.asset

    # Get asset addresses - use ZERO_ADDRESS as fallback if no underlying token
    # This should never happen in production data but provides type safety
    underlying_address = asset.underlying_token.address if asset.underlying_token else ZERO_ADDRESS

    # Calculate actual balance
    actual_balance = calculate_actual_debt_balance(
        scaled_balance=position.balance,
        borrow_index=asset.borrow_index,
    )

    # Get eMode category ID for this asset
    emode_cat_id = asset.e_mode_category_id or None

    # Check if this position is in eMode
    in_emode = user_emode > 0 and emode_cat_id == user_emode

    return DebtPositionData(
        asset_address=underlying_address,
        asset_symbol=asset.underlying_token.symbol if asset.underlying_token else None,
        scaled_balance=position.balance,
        actual_balance=actual_balance,
        in_emode=in_emode,
        emode_category_id=emode_cat_id,
        price=price,
    )


def calculate_health_factor(
    collateral_positions: tuple[CollateralPositionData, ...],
    debt_positions: tuple[DebtPositionData, ...],
    isolation_mode_debt: int = 0,
    isolation_debt_ceiling: int | None = None,
) -> float | None:
    """
    Calculate health factor for a user position.

    Health Factor = Sum(collateral * price * liquidation_threshold) / Sum(debt * price)

    Prices are in 8 decimals (oracle format). The result is scaled appropriately.

    For isolation mode, debt is capped at the debt ceiling.

    Args:
        collateral_positions: Tuple of collateral position data
        debt_positions: Tuple of debt position data
        isolation_mode_debt: User's isolation mode debt (if applicable)
        isolation_debt_ceiling: Debt ceiling for isolation mode asset

    Returns:
        Health factor, or None if no debt
    """
    if not debt_positions and isolation_mode_debt == 0:
        return None

    # Calculate weighted collateral value
    weighted_collateral = 0
    for collateral_pos in collateral_positions:
        if collateral_pos.is_enabled_as_collateral and collateral_pos.actual_balance > 0:
            # Multiply balance by price and LT
            weighted = (
                collateral_pos.price_adjusted_balance
                * collateral_pos.effective_liquidation_threshold
            )
            weighted_collateral += weighted

    # Calculate total debt (balance * price)
    total_debt = 0
    for debt_pos in debt_positions:
        total_debt += debt_pos.price_adjusted_balance

    # Handle isolation mode debt ceiling
    if isolation_mode_debt > 0 and isolation_debt_ceiling is not None:
        total_debt = min(total_debt, isolation_debt_ceiling)

    if total_debt == 0:
        return None

    # Health factor = weighted_collateral / (total_debt * BASIS_POINTS)
    return weighted_collateral / (total_debt * BASIS_POINTS)


def analyze_user_position(
    user: AaveV3User,
    collateral_positions: list[AaveV3CollateralPosition],
    debt_positions: list[AaveV3DebtPosition],
    collateral_config_map: dict[int, bool],
    price_map: dict[ChecksumAddress, int] | None = None,
) -> UserPositionSummary:
    """
    Analyze a single user's position for liquidation risk.

    Args:
        user: The user database record
        collateral_positions: List of collateral position records
        debt_positions: List of debt position records
        collateral_config_map: Map of asset_id -> enabled status
        price_map: Map of asset address -> oracle price (8 decimals), optional

    Returns:
        UserPositionSummary with risk metrics
    """
    user_emode = user.e_mode
    price_map = price_map or {}

    # Build position data
    collateral_data = tuple(
        build_collateral_position_data(
            position=pos,
            user_emode=user_emode,
            collateral_enabled=collateral_config_map.get(pos.asset_id, True),
            price=_get_price_for_position(pos.asset, price_map),
        )
        for pos in collateral_positions
    )

    debt_data = tuple(
        build_debt_position_data(
            position=pos,
            user_emode=user_emode,
            price=_get_price_for_position(pos.asset, price_map),
        )
        for pos in debt_positions
    )

    # Get isolation mode info
    is_isolation_mode = user.is_isolation_mode
    isolation_debt_ceiling = None
    if is_isolation_mode and user.isolation_collateral_asset is not None:
        asset_config = user.isolation_collateral_asset.asset_config
        if asset_config is not None:
            isolation_debt_ceiling = asset_config.debt_ceiling

    # Calculate health factor
    health_factor = calculate_health_factor(
        collateral_positions=collateral_data,
        debt_positions=debt_data,
        isolation_mode_debt=user.isolation_mode_debt if is_isolation_mode else 0,
        isolation_debt_ceiling=isolation_debt_ceiling,
    )

    # Calculate values in oracle base currency
    total_collateral_value = sum(
        pos.price_adjusted_balance for pos in collateral_data if pos.is_enabled_as_collateral
    )
    total_debt_value = sum(pos.price_adjusted_balance for pos in debt_data)

    # Calculate max LTV capacity (price-adjusted)
    max_ltv_capacity = sum(
        pos.price_adjusted_balance * pos.ltv
        for pos in collateral_data
        if pos.is_enabled_as_collateral
    )
    total_debt_raw = sum(pos.price_adjusted_balance for pos in debt_data)

    max_ltv_ratio = None
    if total_debt_raw > 0 and max_ltv_capacity > 0:
        max_ltv_ratio = (total_debt_raw * BASIS_POINTS) / max_ltv_capacity

    return UserPositionSummary(
        user_address=user.address,
        market_id=user.market_id,
        emode_category_id=user_emode if user_emode > 0 else None,
        is_isolation_mode=is_isolation_mode,
        collateral_positions=collateral_data,
        debt_positions=debt_data,
        total_collateral_value=total_collateral_value,
        total_debt_value=total_debt_value,
        health_factor=health_factor,
        max_ltv_ratio=max_ltv_ratio,
    )


def _get_price_for_position(
    asset: AaveV3Asset,
    price_map: dict[ChecksumAddress, int],
) -> int | None:
    """Get price for an asset from the price map."""
    if asset.underlying_token is None:
        return None
    return price_map.get(asset.underlying_token.address)


def analyze_positions_for_market(
    session: Session,
    market_id: int,
    health_factor_threshold: float = HEALTH_FACTOR_AT_RISK_THRESHOLD,
    limit: int | None = None,
    provider: ProviderAdapter | None = None,
) -> PositionAnalysisResult:
    """
    Analyze all user positions in a market for liquidation risk.

    This is the main entry point for position analysis. It queries
    all users with debt positions and calculates their health factors.

    If provider is provided, prices are fetched from the Aave oracle for
    accurate health factor calculations. Otherwise, positions are
    analyzed without price adjustments (useful for single-asset analysis).

    Args:
        session: SQLAlchemy session
        market_id: Market ID to analyze
        health_factor_threshold: Threshold for "at risk" classification
        limit: Maximum number of users to analyze (for testing)
        provider: ProviderAdapter for fetching oracle prices (optional)

    Returns:
        PositionAnalysisResult with categorized users
    """
    result = PositionAnalysisResult()

    # Get oracle address and fetch prices if provider provided
    price_map: dict[ChecksumAddress, int] = {}
    if provider is not None:
        oracle_address = get_oracle_address_for_market(session, market_id)
        if oracle_address is None:
            logger.warning("Price oracle not found for market, prices will not be applied")
        else:
            # Collect all unique asset addresses first
            asset_addresses = _collect_asset_addresses(session, market_id)
            if asset_addresses:
                logger.info(f"Fetching prices for {len(asset_addresses)} assets from oracle...")
                price_map = fetch_asset_prices(provider, oracle_address, asset_addresses)

    # Query users with debt positions (users without debt can't be liquidated)
    # Use unique() because joinedload on debt_positions (a collection) creates duplicate rows
    users_with_debt = (
        session
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
    users_to_analyze = [u for u in users_with_debt if len(u.debt_positions) > 0]

    if limit is not None:
        users_to_analyze = users_to_analyze[:limit]

    logger.info(f"Analyzing {len(users_to_analyze)} users with debt positions")

    for user in users_to_analyze:
        # Fetch collateral positions for this user
        collateral_positions = session.scalars(
            select(AaveV3CollateralPosition)
            .where(AaveV3CollateralPosition.user_id == user.id)
            .options(
                joinedload(AaveV3CollateralPosition.asset).joinedload(AaveV3Asset.underlying_token),
                joinedload(AaveV3CollateralPosition.asset).joinedload(AaveV3Asset.asset_config),
                joinedload(AaveV3CollateralPosition.asset).joinedload(AaveV3Asset.e_mode_category),
            )
        ).all()

        # Fetch debt positions with full asset info
        debt_positions = session.scalars(
            select(AaveV3DebtPosition)
            .where(AaveV3DebtPosition.user_id == user.id)
            .options(
                joinedload(AaveV3DebtPosition.asset).joinedload(AaveV3Asset.underlying_token),
                joinedload(AaveV3DebtPosition.asset).joinedload(AaveV3Asset.asset_config),
                joinedload(AaveV3DebtPosition.asset).joinedload(AaveV3Asset.e_mode_category),
            )
        ).all()

        # Build collateral config map
        collateral_configs = session.scalars(
            select(AaveV3UserCollateralConfig).where(AaveV3UserCollateralConfig.user_id == user.id)
        ).all()
        collateral_config_map = {cfg.asset_id: cfg.enabled for cfg in collateral_configs}

        # Analyze position
        summary = analyze_user_position(
            user=user,
            collateral_positions=list(collateral_positions),
            debt_positions=list(debt_positions),
            collateral_config_map=collateral_config_map,
            price_map=price_map,
        )

        # Categorize by health factor
        if summary.health_factor is not None and summary.health_factor < 1.0:
            result.liquidatable_users.append(summary)
        elif summary.health_factor is not None and summary.health_factor < health_factor_threshold:
            result.at_risk_users.append(summary)
        else:
            result.safe_users.append(summary)

    # Sort at-risk and liquidatable users by health factor (lowest first)
    result.at_risk_users.sort(key=lambda x: x.health_factor or float("inf"))
    result.liquidatable_users.sort(key=lambda x: x.health_factor or float("inf"))

    logger.info(
        f"Analysis complete: {len(result.safe_users)} safe, "
        f"{len(result.at_risk_users)} at risk, "
        f"{len(result.liquidatable_users)} liquidatable"
    )

    return result


def _collect_asset_addresses(session: Session, market_id: int) -> set[ChecksumAddress]:
    """
    Collect all unique asset addresses for a market.

    Args:
        session: SQLAlchemy session
        market_id: Market ID to query

    Returns:
        Set of underlying asset addresses
    """
    assets = (
        session
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
