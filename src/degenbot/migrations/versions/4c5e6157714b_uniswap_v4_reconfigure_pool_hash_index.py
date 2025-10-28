"""Uniswap V4: Reconfigure pool hash index

Revision ID: 4c5e6157714b
Revises: 12321f192db4
Create Date: 2025-10-28 12:22:47.222482

"""

from collections.abc import Sequence

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "4c5e6157714b"
down_revision: str | Sequence[str] | None = "12321f192db4"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    with op.batch_alter_table("uniswap_v4_pools", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_uniswap_v4_pools_pool_hash"))
        batch_op.create_index(
            batch_op.f("ix_uniswap_v4_pools_pool_hash"), ["pool_hash"], unique=False
        )


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
