"""Aave V3: use BigInteger for stkAAVE balance

Revision ID: 22990a3626ae
Revises: 3b62cfcbc261
Create Date: 2026-02-04 12:06:23.960177

"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

import degenbot.database.models

# revision identifiers, used by Alembic.
revision: str = "22990a3626ae"
down_revision: str | Sequence[str] | None = "3b62cfcbc261"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    with op.batch_alter_table("aave_v3_users", schema=None) as batch_op:
        batch_op.alter_column(
            "stk_aave_balance",
            existing_type=sa.INTEGER(),
            type_=degenbot.database.models.base.IntMappedToString(length=78),
            existing_nullable=True,
        )


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
