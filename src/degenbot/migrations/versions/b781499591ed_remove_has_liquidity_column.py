"""Remove has_liquidity column

Revision ID: b781499591ed
Revises: bd7ca13a7d39
Create Date: 2025-09-05 15:09:15.898102

"""

from collections.abc import Sequence

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "b781499591ed"
down_revision: str | Sequence[str] | None = "bd7ca13a7d39"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    with op.batch_alter_table("aerodrome_v3_pools") as batch_op:
        batch_op.drop_column("has_liquidity")
    with op.batch_alter_table("pancakeswap_v3_pools") as batch_op:
        batch_op.drop_column("has_liquidity")
    with op.batch_alter_table("sushiswap_v3_pools") as batch_op:
        batch_op.drop_column("has_liquidity")
    with op.batch_alter_table("uniswap_v3_pools") as batch_op:
        batch_op.drop_column("has_liquidity")
    with op.batch_alter_table("uniswap_v4_pools") as batch_op:
        batch_op.drop_column("has_liquidity")


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
