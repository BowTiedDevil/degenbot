"""Add indexes for non-primary foreign keys

Revision ID: 03e723f439dc
Revises: b20f5564b3b6
Create Date: 2025-10-24 23:16:27.369363

"""

from collections.abc import Sequence

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "03e723f439dc"
down_revision: str | Sequence[str] | None = "b20f5564b3b6"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    with op.batch_alter_table("aerodrome_v2_pools", schema=None) as batch_op:
        batch_op.create_index(
            batch_op.f("ix_aerodrome_v2_pools_token0_id"), ["token0_id"], unique=False
        )
        batch_op.create_index(
            batch_op.f("ix_aerodrome_v2_pools_token1_id"), ["token1_id"], unique=False
        )

    with op.batch_alter_table("aerodrome_v3_pools", schema=None) as batch_op:
        batch_op.create_index(
            batch_op.f("ix_aerodrome_v3_pools_token0_id"), ["token0_id"], unique=False
        )
        batch_op.create_index(
            batch_op.f("ix_aerodrome_v3_pools_token1_id"), ["token1_id"], unique=False
        )

    with op.batch_alter_table("camelot_v2_pools", schema=None) as batch_op:
        batch_op.create_index(
            batch_op.f("ix_camelot_v2_pools_token0_id"), ["token0_id"], unique=False
        )
        batch_op.create_index(
            batch_op.f("ix_camelot_v2_pools_token1_id"), ["token1_id"], unique=False
        )

    with op.batch_alter_table("initialization_maps", schema=None) as batch_op:
        batch_op.create_index(
            batch_op.f("ix_initialization_maps_pool_id"), ["pool_id"], unique=False
        )

    with op.batch_alter_table("liquidity_positions", schema=None) as batch_op:
        batch_op.create_index(
            batch_op.f("ix_liquidity_positions_pool_id"), ["pool_id"], unique=False
        )

    with op.batch_alter_table("managed_pool_initialization_maps", schema=None) as batch_op:
        batch_op.create_index(
            batch_op.f("ix_managed_pool_initialization_maps_managed_pool_id"),
            ["managed_pool_id"],
            unique=False,
        )

    with op.batch_alter_table("managed_pool_liquidity_positions", schema=None) as batch_op:
        batch_op.create_index(
            batch_op.f("ix_managed_pool_liquidity_positions_managed_pool_id"),
            ["managed_pool_id"],
            unique=False,
        )

    with op.batch_alter_table("managed_pools", schema=None) as batch_op:
        batch_op.create_index(
            batch_op.f("ix_managed_pools_manager_id"), ["manager_id"], unique=False
        )

    with op.batch_alter_table("pancakeswap_v2_pools", schema=None) as batch_op:
        batch_op.create_index(
            batch_op.f("ix_pancakeswap_v2_pools_token0_id"), ["token0_id"], unique=False
        )
        batch_op.create_index(
            batch_op.f("ix_pancakeswap_v2_pools_token1_id"), ["token1_id"], unique=False
        )

    with op.batch_alter_table("pancakeswap_v3_pools", schema=None) as batch_op:
        batch_op.create_index(
            batch_op.f("ix_pancakeswap_v3_pools_token0_id"), ["token0_id"], unique=False
        )
        batch_op.create_index(
            batch_op.f("ix_pancakeswap_v3_pools_token1_id"), ["token1_id"], unique=False
        )

    with op.batch_alter_table("sushiswap_v2_pools", schema=None) as batch_op:
        batch_op.create_index(
            batch_op.f("ix_sushiswap_v2_pools_token0_id"), ["token0_id"], unique=False
        )
        batch_op.create_index(
            batch_op.f("ix_sushiswap_v2_pools_token1_id"), ["token1_id"], unique=False
        )

    with op.batch_alter_table("sushiswap_v3_pools", schema=None) as batch_op:
        batch_op.create_index(
            batch_op.f("ix_sushiswap_v3_pools_token0_id"), ["token0_id"], unique=False
        )
        batch_op.create_index(
            batch_op.f("ix_sushiswap_v3_pools_token1_id"), ["token1_id"], unique=False
        )

    with op.batch_alter_table("swapbased_v2_pools", schema=None) as batch_op:
        batch_op.create_index(
            batch_op.f("ix_swapbased_v2_pools_token0_id"), ["token0_id"], unique=False
        )
        batch_op.create_index(
            batch_op.f("ix_swapbased_v2_pools_token1_id"), ["token1_id"], unique=False
        )

    with op.batch_alter_table("uniswap_v2_pools", schema=None) as batch_op:
        batch_op.create_index(
            batch_op.f("ix_uniswap_v2_pools_token0_id"), ["token0_id"], unique=False
        )
        batch_op.create_index(
            batch_op.f("ix_uniswap_v2_pools_token1_id"), ["token1_id"], unique=False
        )

    with op.batch_alter_table("uniswap_v3_pools", schema=None) as batch_op:
        batch_op.create_index(
            batch_op.f("ix_uniswap_v3_pools_token0_id"), ["token0_id"], unique=False
        )
        batch_op.create_index(
            batch_op.f("ix_uniswap_v3_pools_token1_id"), ["token1_id"], unique=False
        )

    with op.batch_alter_table("uniswap_v4_pools", schema=None) as batch_op:
        batch_op.create_index(
            batch_op.f("ix_uniswap_v4_pools_currency0_id"), ["currency0_id"], unique=False
        )
        batch_op.create_index(
            batch_op.f("ix_uniswap_v4_pools_currency1_id"), ["currency1_id"], unique=False
        )


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
