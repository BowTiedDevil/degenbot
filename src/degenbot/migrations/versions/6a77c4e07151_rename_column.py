"""Rename column

Revision ID: 6a77c4e07151
Revises: 082ee8a3d339
Create Date: 2025-10-25 13:49:31.086818

"""

from collections.abc import Sequence

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "6a77c4e07151"
down_revision: str | Sequence[str] | None = "082ee8a3d339"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    with op.batch_alter_table("pools", recreate="always", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_pools_token0_id_"))
        batch_op.drop_index(batch_op.f("ix_pools_token1_id_"))

        batch_op.alter_column("token0_id_", new_column_name="token0_id")
        batch_op.alter_column("token1_id_", new_column_name="token1_id")


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
