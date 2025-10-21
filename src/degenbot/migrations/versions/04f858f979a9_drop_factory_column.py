"""drop factory column

Revision ID: 04f858f979a9
Revises: 8b29ed99a2ac
Create Date: 2025-10-18 00:07:36.604383

"""

from collections.abc import Sequence

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "04f858f979a9"
down_revision: str | Sequence[str] | None = "8b29ed99a2ac"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    op.drop_table("metadata")
    with op.batch_alter_table("aerodrome_v2_pools", schema=None) as batch_op:
        batch_op.drop_column("factory")

    with op.batch_alter_table("aerodrome_v3_pools", schema=None) as batch_op:
        batch_op.drop_column("factory")

    with op.batch_alter_table("camelot_v2_pools", schema=None) as batch_op:
        batch_op.drop_column("factory")

    with op.batch_alter_table("pancakeswap_v2_pools", schema=None) as batch_op:
        batch_op.drop_column("factory")

    with op.batch_alter_table("pancakeswap_v3_pools", schema=None) as batch_op:
        batch_op.drop_column("factory")

    with op.batch_alter_table("sushiswap_v2_pools", schema=None) as batch_op:
        batch_op.drop_column("factory")

    with op.batch_alter_table("sushiswap_v3_pools", schema=None) as batch_op:
        batch_op.drop_column("factory")

    with op.batch_alter_table("swapbased_v2_pools", schema=None) as batch_op:
        batch_op.drop_column("factory")

    with op.batch_alter_table("uniswap_v2_pools", schema=None) as batch_op:
        batch_op.drop_column("factory")

    with op.batch_alter_table("uniswap_v3_pools", schema=None) as batch_op:
        batch_op.drop_column("factory")


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
