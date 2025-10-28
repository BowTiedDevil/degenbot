"""Pools: remove default index on pool ID foreign key

Revision ID: 12321f192db4
Revises: 66930706257e
Create Date: 2025-10-28 11:56:19.637520

"""

from collections.abc import Sequence

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "12321f192db4"
down_revision: str | Sequence[str] | None = "66930706257e"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    with op.batch_alter_table("initialization_maps", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_initialization_maps_pool_id"))

    with op.batch_alter_table("liquidity_positions", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_liquidity_positions_pool_id"))


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
