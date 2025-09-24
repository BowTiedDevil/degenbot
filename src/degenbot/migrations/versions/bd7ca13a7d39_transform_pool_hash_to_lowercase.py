"""Transform pool hash to lowercase

Revision ID: bd7ca13a7d39
Revises: d1f98e2c3b18
Create Date: 2025-09-05 14:19:57.457685

"""

from collections.abc import Sequence

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "bd7ca13a7d39"
down_revision: str | Sequence[str] | None = "d1f98e2c3b18"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    op.execute(
        """
        UPDATE uniswap_v4_pools
        SET pool_hash = LOWER(pool_hash)
        """
    )


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
