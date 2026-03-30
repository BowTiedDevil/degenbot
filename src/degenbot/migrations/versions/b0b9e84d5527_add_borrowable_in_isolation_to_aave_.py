"""add_borrowable_in_isolation_to_aave_asset_config

Revision ID: b0b9e84d5527
Revises: 9c411aeeb15e
Create Date: 2026-03-30 12:13:52.917586

"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

# revision identifiers, used by Alembic.
revision: str = "b0b9e84d5527"
down_revision: str | Sequence[str] | None = "9c411aeeb15e"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    with op.batch_alter_table("aave_v3_asset_configs", schema=None) as batch_op:
        batch_op.add_column(
            sa.Column("borrowable_in_isolation", sa.Boolean(), nullable=False, server_default="0")
        )


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
