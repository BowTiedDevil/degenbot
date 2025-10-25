"""Drop token0_id, token1_id from subclass tables

Revision ID: 082ee8a3d339
Revises: e453c9cd9e51
Create Date: 2025-10-25 13:41:16.069821

"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

# revision identifiers, used by Alembic.
revision: str = "082ee8a3d339"
down_revision: str | Sequence[str] | None = "e453c9cd9e51"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    with op.batch_alter_table("aerodrome_v2_pools", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_aerodrome_v2_pools_token0_id"))
        batch_op.drop_index(batch_op.f("ix_aerodrome_v2_pools_token1_id"))
        batch_op.drop_column("token0_id")
        batch_op.drop_column("token1_id")

    with op.batch_alter_table("aerodrome_v3_pools", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_aerodrome_v3_pools_token0_id"))
        batch_op.drop_index(batch_op.f("ix_aerodrome_v3_pools_token1_id"))
        batch_op.drop_column("token0_id")
        batch_op.drop_column("token1_id")

    with op.batch_alter_table("camelot_v2_pools", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_camelot_v2_pools_token0_id"))
        batch_op.drop_index(batch_op.f("ix_camelot_v2_pools_token1_id"))
        batch_op.drop_column("token0_id")
        batch_op.drop_column("token1_id")

    with op.batch_alter_table("pancakeswap_v2_pools", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_pancakeswap_v2_pools_token0_id"))
        batch_op.drop_index(batch_op.f("ix_pancakeswap_v2_pools_token1_id"))
        batch_op.drop_column("token0_id")
        batch_op.drop_column("token1_id")

    with op.batch_alter_table("pancakeswap_v3_pools", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_pancakeswap_v3_pools_token0_id"))
        batch_op.drop_index(batch_op.f("ix_pancakeswap_v3_pools_token1_id"))
        batch_op.drop_column("token0_id")
        batch_op.drop_column("token1_id")

    with op.batch_alter_table("pools", schema=None) as batch_op:
        batch_op.alter_column("token0_id_", existing_type=sa.INTEGER(), nullable=False)
        batch_op.alter_column("token1_id_", existing_type=sa.INTEGER(), nullable=False)
        batch_op.create_index(batch_op.f("ix_pools_token0_id_"), ["token0_id_"], unique=False)
        batch_op.create_index(batch_op.f("ix_pools_token1_id_"), ["token1_id_"], unique=False)

    with op.batch_alter_table("sushiswap_v2_pools", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_sushiswap_v2_pools_token0_id"))
        batch_op.drop_index(batch_op.f("ix_sushiswap_v2_pools_token1_id"))
        batch_op.drop_column("token0_id")
        batch_op.drop_column("token1_id")

    with op.batch_alter_table("sushiswap_v3_pools", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_sushiswap_v3_pools_token0_id"))
        batch_op.drop_index(batch_op.f("ix_sushiswap_v3_pools_token1_id"))
        batch_op.drop_column("token0_id")
        batch_op.drop_column("token1_id")

    with op.batch_alter_table("swapbased_v2_pools", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_swapbased_v2_pools_token0_id"))
        batch_op.drop_index(batch_op.f("ix_swapbased_v2_pools_token1_id"))
        batch_op.drop_column("token0_id")
        batch_op.drop_column("token1_id")

    with op.batch_alter_table("uniswap_v2_pools", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_uniswap_v2_pools_token0_id"))
        batch_op.drop_index(batch_op.f("ix_uniswap_v2_pools_token1_id"))
        batch_op.drop_column("token0_id")
        batch_op.drop_column("token1_id")

    with op.batch_alter_table("uniswap_v3_pools", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_uniswap_v3_pools_token0_id"))
        batch_op.drop_index(batch_op.f("ix_uniswap_v3_pools_token1_id"))
        batch_op.drop_column("token0_id")
        batch_op.drop_column("token1_id")


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
