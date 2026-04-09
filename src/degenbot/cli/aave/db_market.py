"""
Market-level database operations for Aave V3.

Functions for managing market state, eMode categories, and asset configurations.
"""

from sqlalchemy import select
from sqlalchemy.orm import Session

from degenbot.database.models.aave import (
    AaveV3Asset,
    AaveV3AssetConfig,
    AaveV3EModeCategory,
    AaveV3Market,
    AaveV3User,
    AaveV3UserCollateralConfig,
)


def get_e_mode_category(
    session: Session,
    market: AaveV3Market,
    category_id: int,
) -> AaveV3EModeCategory | None:
    """
    Get eMode category by ID.
    """

    return session.scalar(
        select(AaveV3EModeCategory).where(
            AaveV3EModeCategory.market_id == market.id,
            AaveV3EModeCategory.category_id == category_id,
        )
    )


def get_or_create_e_mode_category(
    session: Session,
    market: AaveV3Market,
    category_id: int,
) -> AaveV3EModeCategory:
    """
    Get existing eMode category or create new one.
    """

    category = get_e_mode_category(session, market, category_id)
    if category is not None:
        return category

    category = AaveV3EModeCategory(
        market_id=market.id,
        category_id=category_id,
        label="",
        ltv=0,
        liquidation_threshold=0,
        liquidation_bonus=0,
    )
    session.add(category)
    return category


def get_asset_config(
    session: Session,
    asset_id: int,
) -> AaveV3AssetConfig | None:
    """
    Get asset configuration by asset ID.
    """

    return session.scalar(
        select(AaveV3AssetConfig).where(
            AaveV3AssetConfig.asset_id == asset_id,
        )
    )


def get_or_create_asset_config(
    session: Session,
    asset_id: int,
) -> AaveV3AssetConfig:
    """
    Get existing asset config or create new one with defaults.
    """

    config = get_asset_config(session, asset_id)
    if config is not None:
        return config

    config = AaveV3AssetConfig(
        asset_id=asset_id,
        ltv=0,
        liquidation_threshold=0,
        liquidation_bonus=0,
        borrowing_enabled=False,
        stable_borrowing_enabled=False,
        flash_loan_enabled=False,
        borrowable_in_isolation=False,
        isolation_mode=False,
        debt_ceiling=None,
        e_mode_category_id=None,
    )
    session.add(config)
    return config


def get_user_collateral_config(
    session: Session,
    user_id: int,
    asset_id: int,
) -> AaveV3UserCollateralConfig | None:
    """
    Get user collateral configuration.
    """

    return session.scalar(
        select(AaveV3UserCollateralConfig).where(
            AaveV3UserCollateralConfig.user_id == user_id,
            AaveV3UserCollateralConfig.asset_id == asset_id,
        )
    )


def get_or_create_user_collateral_config(
    session: Session,
    user_id: int,
    asset_id: int,
) -> AaveV3UserCollateralConfig:
    """
    Get existing user collateral config or create new one.
    """

    config = get_user_collateral_config(session, user_id, asset_id)
    if config is not None:
        return config

    config = AaveV3UserCollateralConfig(
        user_id=user_id,
        asset_id=asset_id,
        enabled=False,
    )
    session.add(config)
    return config


def update_user_e_mode(
    user: AaveV3User,
    e_mode: int,
) -> None:
    """
    Update user's eMode category.
    """

    user.e_mode = e_mode


def record_oracle_price(
    session: Session,  # noqa: ARG001
    asset: AaveV3Asset,
    price: int,
    block_number: int,
) -> None:
    """
    Record an oracle price for an asset.

    Updates the asset's last_known_price and last_price_block.
    """

    asset.last_known_price = price
    asset.last_price_block = block_number
