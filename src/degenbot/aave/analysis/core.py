"""Pure position analysis functions.

No database, no RPC, no ORM. All inputs are plain dataclasses
or primitives. All outputs are plain dataclasses.
"""

from dataclasses import dataclass, field

from eth_typing import ChecksumAddress

from degenbot.aave.libraries.wad_ray_math import ray_mul_ceil, ray_mul_floor

# Basis points constant (10000 = 100%)
BASIS_POINTS = 10000

# Health factor thresholds
HEALTH_FACTOR_AT_RISK_THRESHOLD = 1.1  # 10% buffer
HEALTH_FACTOR_LIQUIDATABLE_THRESHOLD = 1.0


@dataclass(frozen=True)
class CollateralPositionData:
    """Collateral position with calculated values for risk analysis."""

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
        """Liquidation threshold used for health factor calculation.

        Returns 0 if collateral is disabled by user preference.
        """
        return self.liquidation_threshold if self.is_enabled_as_collateral else 0

    @property
    def price_adjusted_balance(self) -> int:
        """Balance adjusted by price, in oracle base currency units.

        Returns actual_balance * price. If price is None, returns
        actual_balance (assumes price = 1).
        """
        if self.price is None:
            return self.actual_balance
        return self.actual_balance * self.price


@dataclass(frozen=True)
class DebtPositionData:
    """Debt position with calculated values for risk analysis."""

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
        """Balance adjusted by price, in oracle base currency units.

        Returns actual_balance * price. If price is None, returns
        actual_balance (assumes price = 1).
        """
        if self.price is None:
            return self.actual_balance
        return self.actual_balance * self.price


@dataclass(frozen=True)
class UserRecord:
    """Flat record for a user, extracted from ORM at the query boundary.

    Contains all fields needed by core analysis — no lazy-loaded
    relationships, no ORM references.
    """

    id: int
    address: ChecksumAddress
    market_id: int
    e_mode: int
    is_isolation_mode: bool
    isolation_mode_debt: int
    isolation_debt_ceiling: int | None = None


@dataclass(frozen=True)
class CollateralPositionRecord:
    """Flat record for a collateral position, extracted from ORM.

    All fields that core needs are flattened — no relationship navigation.
    """

    asset_id: int
    balance: int  # Scaled balance
    underlying_address: ChecksumAddress
    underlying_symbol: str | None
    liquidity_index: int
    e_mode_category_id: int | None
    asset_lt: int  # Standard liquidation threshold (bps)
    asset_ltv: int  # Standard LTV (bps)
    emode_lt: int | None = None  # eMode liquidation threshold (bps)
    emode_ltv: int | None = None  # eMode LTV (bps)


@dataclass(frozen=True)
class DebtPositionRecord:
    """Flat record for a debt position, extracted from ORM.

    All fields that core needs are flattened — no relationship navigation.
    """

    asset_id: int
    balance: int  # Scaled balance
    underlying_address: ChecksumAddress
    underlying_symbol: str | None
    borrow_index: int
    e_mode_category_id: int | None


@dataclass(frozen=True)
class UserPositionSummary:
    """Summary of a user's Aave V3 position for liquidation risk assessment."""

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
        """True if position is near liquidation (health factor < threshold)."""
        if self.health_factor is None:
            return False
        return self.health_factor < HEALTH_FACTOR_AT_RISK_THRESHOLD

    @property
    def is_liquidatable(self) -> bool:
        """True if position can be liquidated (health factor < 1)."""
        if self.health_factor is None:
            return False
        return self.health_factor < HEALTH_FACTOR_LIQUIDATABLE_THRESHOLD

    @property
    def has_debt(self) -> bool:
        """True if user has any debt positions."""
        return len(self.debt_positions) > 0


@dataclass
class PositionAnalysisResult:
    """Result of analyzing positions for liquidation risk."""

    safe_users: list[UserPositionSummary] = field(default_factory=list)
    at_risk_users: list[UserPositionSummary] = field(default_factory=list)
    liquidatable_users: list[UserPositionSummary] = field(default_factory=list)

    @property
    def total_users(self) -> int:
        """Return total users."""
        return len(self.safe_users) + len(self.at_risk_users) + len(self.liquidatable_users)

    @property
    def at_risk_count(self) -> int:
        """Return at risk count."""
        return len(self.at_risk_users)

    @property
    def liquidatable_count(self) -> int:
        """Return liquidatable count."""
        return len(self.liquidatable_users)

    def categorize(
        self,
        summary: UserPositionSummary,
        health_factor_threshold: float = HEALTH_FACTOR_AT_RISK_THRESHOLD,
    ) -> None:
        """Categorize a user summary by health factor."""
        if summary.health_factor is not None and summary.health_factor < 1.0:
            self.liquidatable_users.append(summary)
        elif summary.health_factor is not None and summary.health_factor < health_factor_threshold:
            self.at_risk_users.append(summary)
        else:
            self.safe_users.append(summary)

    def sort_by_risk(self) -> None:
        """Sort at-risk and liquidatable users by health factor (lowest first)."""
        self.at_risk_users.sort(key=lambda x: x.health_factor or float("inf"))
        self.liquidatable_users.sort(key=lambda x: x.health_factor or float("inf"))


def calculate_actual_collateral_balance(
    scaled_balance: int,
    liquidity_index: int,
) -> int:
    """Calculate actual collateral balance from scaled balance.

    Uses floor rounding (ray_mul_floor) to match Aave V3 behavior.
    Collateral balance should not be over-accounted.

    Returns:
        The computed value.

    """
    return ray_mul_floor(scaled_balance, liquidity_index)


def calculate_actual_debt_balance(
    scaled_balance: int,
    borrow_index: int,
) -> int:
    """Calculate actual debt balance from scaled balance.

    Uses ceil rounding (ray_mul_ceil) to match Aave V3 behavior.
    Debt should not be under-accounted.

    Returns:
        The computed value.

    """
    return ray_mul_ceil(scaled_balance, borrow_index)


def get_liquidation_threshold(
    *,
    asset_lt: int,
    emode_lt: int | None,
    user_emode: int,
    asset_emode_category_id: int | None,
) -> int:
    """Get the effective liquidation threshold for a collateral position.

    If the user is in eMode and the asset is in the same eMode category,
    use the eMode liquidation threshold. Otherwise use the standard threshold.

    Returns:
        The computed value.

    """
    if user_emode > 0 and asset_emode_category_id == user_emode and emode_lt is not None:
        return emode_lt
    return asset_lt


def get_ltv(
    *,
    asset_ltv: int,
    emode_ltv: int | None,
    user_emode: int,
    asset_emode_category_id: int | None,
) -> int:
    """Get the effective LTV for a collateral position.

    If the user is in eMode and the asset is in the same eMode category,
    use the eMode LTV. Otherwise use the standard LTV.

    Returns:
        The computed value.

    """
    if user_emode > 0 and asset_emode_category_id == user_emode and emode_ltv is not None:
        return emode_ltv
    return asset_ltv


def build_collateral_position_data(
    record: CollateralPositionRecord,
    user_emode: int,
    *,
    collateral_enabled: bool,
    price: int | None = None,
) -> CollateralPositionData:
    """Build CollateralPositionData from a flat record.

    Pure function — no ORM, no DB, no RPC.

    Returns:
        The computed value.

    """
    actual_balance = calculate_actual_collateral_balance(
        scaled_balance=record.balance,
        liquidity_index=record.liquidity_index,
    )

    lt = get_liquidation_threshold(
        asset_lt=record.asset_lt,
        emode_lt=record.emode_lt,
        user_emode=user_emode,
        asset_emode_category_id=record.e_mode_category_id,
    )
    ltv = get_ltv(
        asset_ltv=record.asset_ltv,
        emode_ltv=record.emode_ltv,
        user_emode=user_emode,
        asset_emode_category_id=record.e_mode_category_id,
    )

    in_emode = user_emode > 0 and record.e_mode_category_id == user_emode

    return CollateralPositionData(
        asset_address=record.underlying_address,
        asset_symbol=record.underlying_symbol,
        scaled_balance=record.balance,
        actual_balance=actual_balance,
        liquidation_threshold=lt,
        ltv=ltv,
        is_enabled_as_collateral=collateral_enabled,
        in_emode=in_emode,
        emode_category_id=record.e_mode_category_id,
        price=price,
    )


def build_debt_position_data(
    record: DebtPositionRecord,
    user_emode: int,
    *,
    price: int | None = None,
) -> DebtPositionData:
    """Build DebtPositionData from a flat record.

    Pure function — no ORM, no DB, no RPC.

    Returns:
        The computed value.

    """
    actual_balance = calculate_actual_debt_balance(
        scaled_balance=record.balance,
        borrow_index=record.borrow_index,
    )

    in_emode = user_emode > 0 and record.e_mode_category_id == user_emode

    return DebtPositionData(
        asset_address=record.underlying_address,
        asset_symbol=record.underlying_symbol,
        scaled_balance=record.balance,
        actual_balance=actual_balance,
        in_emode=in_emode,
        emode_category_id=record.e_mode_category_id,
        price=price,
    )


def calculate_health_factor(
    collateral_positions: tuple[CollateralPositionData, ...],
    debt_positions: tuple[DebtPositionData, ...],
    isolation_mode_debt: int = 0,
    isolation_debt_ceiling: int | None = None,
) -> float | None:
    """Calculate health factor for a user position.

    Health Factor = Sum(collateral * price * liquidation_threshold) / Sum(debt * price)

    Prices are in 8 decimals (oracle format). The result is scaled appropriately.

    For isolation mode, debt is capped at the debt ceiling.

    Returns:
        The computed value.

    """
    if not debt_positions and isolation_mode_debt == 0:
        return None

    # Calculate weighted collateral value
    weighted_collateral = 0
    for collateral_pos in collateral_positions:
        if collateral_pos.is_enabled_as_collateral and collateral_pos.actual_balance > 0:
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

    return weighted_collateral / (total_debt * BASIS_POINTS)


def analyze_user_position(
    user: UserRecord,
    collateral_positions: list[CollateralPositionRecord],
    debt_positions: list[DebtPositionRecord],
    collateral_config_map: dict[int, bool],
    price_map: dict[ChecksumAddress, int] | None = None,
) -> UserPositionSummary:
    """Analyze a single user's position for liquidation risk.

    Pure function — no ORM, no DB, no RPC.

    Returns:
        The computed value.

    """
    user_emode = user.e_mode
    price_map = price_map or {}

    # Build position data
    collateral_data = tuple(
        build_collateral_position_data(
            record=pos,
            user_emode=user_emode,
            collateral_enabled=collateral_config_map.get(pos.asset_id, True),
            price=price_map.get(pos.underlying_address),
        )
        for pos in collateral_positions
    )

    debt_data = tuple(
        build_debt_position_data(
            record=pos,
            user_emode=user_emode,
            price=price_map.get(pos.underlying_address),
        )
        for pos in debt_positions
    )

    # Calculate health factor
    health_factor = calculate_health_factor(
        collateral_positions=collateral_data,
        debt_positions=debt_data,
        isolation_mode_debt=user.isolation_mode_debt if user.is_isolation_mode else 0,
        isolation_debt_ceiling=user.isolation_debt_ceiling,
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
        is_isolation_mode=user.is_isolation_mode,
        collateral_positions=collateral_data,
        debt_positions=debt_data,
        total_collateral_value=total_collateral_value,
        total_debt_value=total_debt_value,
        health_factor=health_factor,
        max_ltv_ratio=max_ltv_ratio,
    )
