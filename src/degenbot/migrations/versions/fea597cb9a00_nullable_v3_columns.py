"""nullable V3 columns

Revision ID: fea597cb9a00
Revises: 4fa3a79786e2
Create Date: 2025-07-29 11:13:21.746601

"""

from collections.abc import Sequence

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "fea597cb9a00"
down_revision: str | Sequence[str] | None = "4fa3a79786e2"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    with op.batch_alter_table("pools") as batch_op:
        batch_op.alter_column("tick_spacing", nullable=True)
        batch_op.alter_column("has_liquidity", nullable=True)


def downgrade() -> None:
    """Downgrade schema."""

    with op.batch_alter_table("pools") as batch_op:
        batch_op.alter_column("tick_spacing", nullable=False)
        batch_op.alter_column("has_liquidity", nullable=False)
