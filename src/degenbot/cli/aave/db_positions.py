"""
Position database operations for Aave V3.

Functions for managing collateral and debt positions.
"""

from typing import TYPE_CHECKING, cast

from sqlalchemy import select

from degenbot.cli.aave.types import TransactionContext
from degenbot.database.models.aave import AaveV3CollateralPosition, AaveV3DebtPosition, AaveV3User

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
