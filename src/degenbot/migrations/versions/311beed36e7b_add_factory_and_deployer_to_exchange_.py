"""add factory and deployer to exchange table

Revision ID: 311beed36e7b
Revises: 723fe25c87b8
Create Date: 2025-10-03 15:27:06.651242

"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

# revision identifiers, used by Alembic.
revision: str = "311beed36e7b"
down_revision: str | Sequence[str] | None = "723fe25c87b8"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    op.alter_column("exchanges", "factory", existing_type=sa.VARCHAR(length=42), nullable=False)


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
