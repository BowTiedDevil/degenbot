"""Uniswap V4: Add state view column

Revision ID: 6f376d34618b
Revises: 4c5e6157714b
Create Date: 2025-11-06 13:28:05.613209

"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

# revision identifiers, used by Alembic.
revision: str = "6f376d34618b"
down_revision: str | Sequence[str] | None = "4c5e6157714b"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    with op.batch_alter_table("pool_managers", schema=None) as batch_op:
        batch_op.add_column(sa.Column("state_view", sa.String(length=42), nullable=True))


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
