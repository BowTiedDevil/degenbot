"""add liquidity update marker

Revision ID: 7dc2ca38053f
Revises: 311beed36e7b
Create Date: 2025-10-14 14:49:00.008992

"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

# revision identifiers, used by Alembic.
revision: str = "7dc2ca38053f"
down_revision: str | Sequence[str] | None = "311beed36e7b"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    op.add_column(
        "aerodrome_v3_pools", sa.Column("liquidity_update_block", sa.Integer(), nullable=True)
    )
    op.add_column(
        "aerodrome_v3_pools", sa.Column("liquidity_update_log_index", sa.Integer(), nullable=True)
    )
    op.add_column(
        "pancakeswap_v3_pools", sa.Column("liquidity_update_block", sa.Integer(), nullable=True)
    )
    op.add_column(
        "pancakeswap_v3_pools", sa.Column("liquidity_update_log_index", sa.Integer(), nullable=True)
    )
    op.add_column(
        "sushiswap_v3_pools", sa.Column("liquidity_update_block", sa.Integer(), nullable=True)
    )
    op.add_column(
        "sushiswap_v3_pools", sa.Column("liquidity_update_log_index", sa.Integer(), nullable=True)
    )
    op.add_column(
        "uniswap_v3_pools", sa.Column("liquidity_update_block", sa.Integer(), nullable=True)
    )
    op.add_column(
        "uniswap_v3_pools", sa.Column("liquidity_update_log_index", sa.Integer(), nullable=True)
    )


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
