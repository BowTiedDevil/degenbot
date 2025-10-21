"""V4 pools: add token foreign key

Revision ID: 8b29ed99a2ac
Revises: 50b39bafa0be
Create Date: 2025-10-17 14:33:55.170773

"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

from degenbot.database.models.pools import UniswapV4PoolTable

# revision identifiers, used by Alembic.
revision: str = "8b29ed99a2ac"
down_revision: str | Sequence[str] | None = "50b39bafa0be"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    with op.batch_alter_table(UniswapV4PoolTable.__tablename__, schema=None) as batch_op:
        batch_op.add_column(sa.Column("currency0_id", sa.Integer(), nullable=False))
        batch_op.add_column(sa.Column("currency1_id", sa.Integer(), nullable=False))
        batch_op.create_foreign_key("fk_currency0_id", "erc20_tokens", ["currency0_id"], ["id"])
        batch_op.create_foreign_key("fk_currency1_id", "erc20_tokens", ["currency1_id"], ["id"])

        # Drop the token address columns
        batch_op.drop_column("currency0")
        batch_op.drop_column("currency1")


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
