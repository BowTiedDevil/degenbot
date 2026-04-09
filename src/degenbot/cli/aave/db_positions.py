"""
Position database operations for Aave V3.

Functions for managing collateral and debt positions.
"""

from typing import TYPE_CHECKING, cast

from sqlalchemy import delete, select
from sqlalchemy.orm import Session

from degenbot.cli.aave.types import TransactionContext
from degenbot.database.models.aave import (
    AaveV3CollateralPosition,
    AaveV3DebtPosition,
    AaveV3Market,
    AaveV3User,
)

if TYPE_CHECKING:
    from typing import TypeVar

    T = TypeVar("T", AaveV3CollateralPosition, AaveV3DebtPosition)


def get_or_create_position(
    *,
    tx_context: TransactionContext,
    user: AaveV3User,
    asset_id: int,
    position_table: type["T"],
) -> "T":
    """
    Get existing position or create new one with zero balance.
    """

    # Query database - SQLAlchemy's identity map handles caching
    existing_position = tx_context.session.scalar(
        select(position_table).where(
            position_table.user_id == user.id,
            position_table.asset_id == asset_id,
        )
    )

    if existing_position is not None:
        # INVARIANT: Found position must match the user we queried for
        assert existing_position.user_id == user.id, (
            f"Database returned position with wrong user_id: "
            f"expected {user.id}, got {existing_position.user_id}. "
            f"This indicates a SQL error or database corruption."
        )

        return existing_position

    # Create new position
    new_position = position_table(user_id=user.id, asset_id=asset_id, balance=0)
    tx_context.session.add(new_position)
    tx_context.session.flush()

    return cast("T", new_position)


def get_or_create_collateral_position(
    *,
    tx_context: TransactionContext,
    user: AaveV3User,
    asset_id: int,
) -> AaveV3CollateralPosition:
    """
    Get existing collateral position or create new one with zero balance.

    Uses tx_context.modified_positions cache to avoid repeated database queries.
    """

    return get_or_create_position(
        tx_context=tx_context,
        user=user,
        asset_id=asset_id,
        position_table=AaveV3CollateralPosition,
    )


def get_or_create_debt_position(
    *,
    tx_context: TransactionContext,
    user: AaveV3User,
    asset_id: int,
) -> AaveV3DebtPosition:
    """
    Get existing debt position or create new one with zero balance.

    Uses tx_context.modified_positions cache to avoid repeated database queries.
    """

    return get_or_create_position(
        tx_context=tx_context,
        user=user,
        asset_id=asset_id,
        position_table=AaveV3DebtPosition,
    )


def get_collateral_position(
    session: Session,
    user_id: int,
    asset_id: int,
) -> AaveV3CollateralPosition | None:
    """
    Get existing collateral position.
    """

    return session.scalar(
        select(AaveV3CollateralPosition).where(
            AaveV3CollateralPosition.user_id == user_id,
            AaveV3CollateralPosition.asset_id == asset_id,
        )
    )


def get_debt_position(
    session: Session,
    user_id: int,
    asset_id: int,
) -> AaveV3DebtPosition | None:
    """
    Get existing debt position.
    """

    return session.scalar(
        select(AaveV3DebtPosition).where(
            AaveV3DebtPosition.user_id == user_id,
            AaveV3DebtPosition.asset_id == asset_id,
        )
    )


def update_position_balance(
    position: AaveV3CollateralPosition | AaveV3DebtPosition,
    new_balance: int,
) -> None:
    """
    Update a position's balance.
    """

    position.balance = new_balance


def update_debt_position_index(
    position: AaveV3DebtPosition,
    new_index: int,
) -> None:
    """
    Update a debt position's last index.
    """

    position.last_index = new_index


def delete_zero_balance_positions(
    session: Session,
    market: AaveV3Market,
) -> None:
    """
    Delete all zero-balance debt and collateral positions for the market.
    """

    # Delete zero-balance collateral positions using bulk delete
    session.execute(
        delete(AaveV3CollateralPosition).where(
            AaveV3CollateralPosition.id.in_(
                select(AaveV3CollateralPosition.id)
                .join(AaveV3User)
                .where(
                    AaveV3User.market_id == market.id,
                    AaveV3CollateralPosition.balance == 0,
                )
            )
        )
    )

    # Delete zero-balance debt positions using bulk delete
    session.execute(
        delete(AaveV3DebtPosition).where(
            AaveV3DebtPosition.id.in_(
                select(AaveV3DebtPosition.id)
                .join(AaveV3User)
                .where(
                    AaveV3User.market_id == market.id,
                    AaveV3DebtPosition.balance == 0,
                )
            )
        )
    )


def get_users_with_positions(
    session: Session,
    market: AaveV3Market,
) -> list[AaveV3User]:
    """
    Get all users who have at least one position (collateral or debt) in the market.
    """

    return list(
        session.scalars(
            select(AaveV3User)
            .where(
                AaveV3User.market_id == market.id,
                (AaveV3User.collateral_positions.any() | AaveV3User.debt_positions.any()),
            )
            .distinct()
        ).all()
    )


def get_user_collateral_positions(
    session: Session,
    user: AaveV3User,
) -> list[AaveV3CollateralPosition]:
    """
    Get all collateral positions for a user.
    """

    return list(
        session.scalars(
            select(AaveV3CollateralPosition).where(
                AaveV3CollateralPosition.user_id == user.id,
            )
        ).all()
    )


def get_user_debt_positions(
    session: Session,
    user: AaveV3User,
) -> list[AaveV3DebtPosition]:
    """
    Get all debt positions for a user.
    """

    return list(
        session.scalars(
            select(AaveV3DebtPosition).where(
                AaveV3DebtPosition.user_id == user.id,
            )
        ).all()
    )
