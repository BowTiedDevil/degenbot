"""Pool Manager: add index

Revision ID: b20f5564b3b6
Revises: 8aa4babb128a
Create Date: 2025-10-21 09:58:26.455221

"""

from collections.abc import Sequence

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "b20f5564b3b6"
down_revision: str | Sequence[str] | None = "8aa4babb128a"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    with op.batch_alter_table("pool_managers", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_pool_managers_address"))
        batch_op.create_index("ix_pool_manager_address_chain", ["address", "chain"], unique=True)


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
