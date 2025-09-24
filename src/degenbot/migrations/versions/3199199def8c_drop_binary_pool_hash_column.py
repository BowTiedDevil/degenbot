"""Drop binary pool hash column

Revision ID: 3199199def8c
Revises: 5c8805573ab3
Create Date: 2025-09-05 13:38:26.500135

"""

from collections.abc import Sequence

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "3199199def8c"
down_revision: str | Sequence[str] | None = "5c8805573ab3"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    op.drop_index(op.f("ix_uniswap_v4_pools_pool_hash"), table_name="uniswap_v4_pools")
    with op.batch_alter_table("uniswap_v4_pools") as batch_op:
        batch_op.drop_column("pool_hash")


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
