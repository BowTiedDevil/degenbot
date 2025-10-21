"""Uniswap V4: add exchange foreign key

Revision ID: 39e331854eb2
Revises: 901adb947000
Create Date: 2025-10-20 10:45:16.090760

"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

# revision identifiers, used by Alembic.
revision: str = "39e331854eb2"
down_revision: str | Sequence[str] | None = "901adb947000"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    with op.batch_alter_table("pool_managers", schema=None) as batch_op:
        batch_op.add_column(sa.Column("exchange_id", sa.Integer(), nullable=False))
        batch_op.create_foreign_key("fk_exchange", "exchanges", ["exchange_id"], ["id"])


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
