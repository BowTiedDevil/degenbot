"""Convert pool hash to text

Revision ID: 5c8805573ab3
Revises: 87fd9fc7ae00
Create Date: 2025-09-05 13:12:10.475697

"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

# revision identifiers, used by Alembic.
revision: str = "5c8805573ab3"
down_revision: str | Sequence[str] | None = "87fd9fc7ae00"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    op.add_column(
        "uniswap_v4_pools", sa.Column("pool_hash_hex", sa.String(length=66), nullable=True)
    )
    op.execute("UPDATE uniswap_v4_pools SET pool_hash_hex = '0x' || hex(pool_hash)")
    with op.batch_alter_table("uniswap_v4_pools") as batch_op:
        batch_op.alter_column("pool_hash_hex", existing_type=sa.Text(), nullable=False)


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
