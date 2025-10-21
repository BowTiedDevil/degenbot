"""drop deployer column

Revision ID: 901adb947000
Revises: 04f858f979a9
Create Date: 2025-10-18 00:08:22.355858

"""

from collections.abc import Sequence

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "901adb947000"
down_revision: str | Sequence[str] | None = "04f858f979a9"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    with op.batch_alter_table("aerodrome_v2_pools", schema=None) as batch_op:
        batch_op.drop_column("deployer")

    with op.batch_alter_table("aerodrome_v3_pools", schema=None) as batch_op:
        batch_op.drop_column("deployer")

    with op.batch_alter_table("camelot_v2_pools", schema=None) as batch_op:
        batch_op.drop_column("deployer")

    with op.batch_alter_table("pancakeswap_v2_pools", schema=None) as batch_op:
        batch_op.drop_column("deployer")

    with op.batch_alter_table("pancakeswap_v3_pools", schema=None) as batch_op:
        batch_op.drop_column("deployer")

    with op.batch_alter_table("sushiswap_v2_pools", schema=None) as batch_op:
        batch_op.drop_column("deployer")

    with op.batch_alter_table("sushiswap_v3_pools", schema=None) as batch_op:
        batch_op.drop_column("deployer")

    with op.batch_alter_table("swapbased_v2_pools", schema=None) as batch_op:
        batch_op.drop_column("deployer")

    with op.batch_alter_table("uniswap_v2_pools", schema=None) as batch_op:
        batch_op.drop_column("deployer")

    with op.batch_alter_table("uniswap_v3_pools", schema=None) as batch_op:
        batch_op.drop_column("deployer")


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
