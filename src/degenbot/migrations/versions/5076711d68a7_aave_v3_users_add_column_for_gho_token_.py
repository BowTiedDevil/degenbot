"""Aave V3 (users): add column for GHO token discount

Revision ID: 5076711d68a7
Revises: efaaed8ddc68
Create Date: 2026-01-22 17:53:17.642914

"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

# revision identifiers, used by Alembic.
revision: str = "5076711d68a7"
down_revision: str | Sequence[str] | None = "efaaed8ddc68"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    with op.batch_alter_table("aave_v3_users", schema=None) as batch_op:
        batch_op.add_column(sa.Column("gho_discount", sa.Integer(), nullable=True))


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
