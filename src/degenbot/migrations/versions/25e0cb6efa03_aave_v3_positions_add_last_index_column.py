"""Aave V3 (positions): add last index column

Revision ID: 25e0cb6efa03
Revises: 5076711d68a7
Create Date: 2026-01-22 22:40:53.591951

"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

import degenbot.database.models

# revision identifiers, used by Alembic.
revision: str = "25e0cb6efa03"
down_revision: str | Sequence[str] | None = "5076711d68a7"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    with op.batch_alter_table("aave_v3_collateral_positions", schema=None) as batch_op:
        batch_op.add_column(
            sa.Column(
                "last_index",
                degenbot.database.models.base.IntMappedToString(length=78),
                nullable=True,
            )
        )

    with op.batch_alter_table("aave_v3_debt_positions", schema=None) as batch_op:
        batch_op.add_column(
            sa.Column(
                "last_index",
                degenbot.database.models.base.IntMappedToString(length=78),
                nullable=True,
            )
        )


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
