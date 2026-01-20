"""Aave V3: add aToken/vToken revision column

Revision ID: efaaed8ddc68
Revises: eb4080485a56
Create Date: 2026-01-19 10:22:21.379154

"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

# revision identifiers, used by Alembic.
revision: str = "efaaed8ddc68"
down_revision: str | Sequence[str] | None = "eb4080485a56"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    with op.batch_alter_table("aave_v3_assets", schema=None) as batch_op:
        batch_op.add_column(sa.Column("a_token_revision", sa.Integer(), nullable=False))
        batch_op.add_column(sa.Column("v_token_revision", sa.Integer(), nullable=False))


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
