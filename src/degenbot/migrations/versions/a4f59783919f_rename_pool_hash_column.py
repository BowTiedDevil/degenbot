"""Rename pool hash column, add unique constraint

Revision ID: a4f59783919f
Revises: 3199199def8c
Create Date: 2025-09-05 13:47:09.094614

"""

from collections.abc import Sequence

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "a4f59783919f"
down_revision: str | Sequence[str] | None = "3199199def8c"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    with op.batch_alter_table("uniswap_v4_pools") as batch_op:
        batch_op.alter_column("pool_hash_hex", new_column_name="pool_hash")


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
