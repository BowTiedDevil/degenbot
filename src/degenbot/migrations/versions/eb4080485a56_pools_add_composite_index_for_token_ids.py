"""Pools: Add composite index for token IDs

Revision ID: eb4080485a56
Revises: 06a3739885a0
Create Date: 2025-11-08 00:58:51.072393

"""

from collections.abc import Sequence

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "eb4080485a56"
down_revision: str | Sequence[str] | None = "06a3739885a0"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    with op.batch_alter_table("pools", schema=None) as batch_op:
        batch_op.create_index(
            "ix_liquidity_pools_token_ids", ["token0_id", "token1_id"], unique=False
        )

    with op.batch_alter_table("uniswap_v4_pools", schema=None) as batch_op:
        batch_op.create_index(
            "ix_uniswap_v4_pools_token_ids", ["currency0_id", "currency1_id"], unique=False
        )


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
