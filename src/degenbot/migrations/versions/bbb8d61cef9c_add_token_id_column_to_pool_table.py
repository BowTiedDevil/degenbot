"""Add token ID column to pool table

Revision ID: bbb8d61cef9c
Revises: 03e723f439dc
Create Date: 2025-10-25 00:06:00.655581

"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

# revision identifiers, used by Alembic.
revision: str = "bbb8d61cef9c"
down_revision: str | Sequence[str] | None = "03e723f439dc"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    with op.batch_alter_table("pools", schema=None) as batch_op:
        batch_op.add_column(sa.Column("token0_id_", sa.Integer(), nullable=True))
        batch_op.add_column(sa.Column("token1_id_", sa.Integer(), nullable=True))
        batch_op.create_foreign_key("fk_token0_id", "erc20_tokens", ["token0_id_"], ["id"])
        batch_op.create_foreign_key("fk_token1_id", "erc20_tokens", ["token1_id_"], ["id"])


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
