"""Add unique index

Revision ID: d1f98e2c3b18
Revises: a4f59783919f
Create Date: 2025-09-05 13:53:55.488450

"""

from collections.abc import Sequence

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "d1f98e2c3b18"
down_revision: str | Sequence[str] | None = "a4f59783919f"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    op.create_index(
        op.f("ix_uniswap_v4_pools_pool_hash"), "uniswap_v4_pools", ["pool_hash"], unique=True
    )


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
