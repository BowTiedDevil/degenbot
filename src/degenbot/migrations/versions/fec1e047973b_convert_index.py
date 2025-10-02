"""Convert index

Revision ID: fec1e047973b
Revises: 3eb6b82dc42d
Create Date: 2025-09-30 14:53:06.171358

"""

from collections.abc import Sequence

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "fec1e047973b"
down_revision: str | Sequence[str] | None = "3eb6b82dc42d"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    op.drop_index(op.f("ix_pools_address"), table_name="pools")
    op.create_index("ix_liquidity_pool_address_chain", "pools", ["address", "chain"], unique=True)


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
