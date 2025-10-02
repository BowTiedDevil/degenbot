"""add stable column for Aerodrome V2

Revision ID: 723fe25c87b8
Revises: fec1e047973b
Create Date: 2025-10-01 11:37:11.820793

"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

# revision identifiers, used by Alembic.
revision: str = "723fe25c87b8"
down_revision: str | Sequence[str] | None = "fec1e047973b"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    op.add_column("aerodrome_v2_pools", sa.Column("stable", sa.Boolean(), nullable=False))


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
