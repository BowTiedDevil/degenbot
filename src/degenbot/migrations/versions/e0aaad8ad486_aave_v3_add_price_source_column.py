"""Aave V3: add price_source column

Revision ID: e0aaad8ad486
Revises: b0b9e84d5527
Create Date: 2026-04-02 12:56:20.091168

"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

# revision identifiers, used by Alembic.
revision: str = "e0aaad8ad486"
down_revision: str | Sequence[str] | None = "b0b9e84d5527"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    with op.batch_alter_table("aave_v3_assets", schema=None) as batch_op:
        batch_op.add_column(sa.Column("price_source", sa.String(length=42), nullable=True))


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
