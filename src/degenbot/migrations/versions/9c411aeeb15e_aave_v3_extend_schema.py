"""Aave V3: extend schema

Revision ID: 9c411aeeb15e
Revises: 5d2dfe7292b3
Create Date: 2026-03-20 21:15:46.081891

"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

import degenbot.database.models

# revision identifiers, used by Alembic.
revision: str = "9c411aeeb15e"
down_revision: str | Sequence[str] | None = "5d2dfe7292b3"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    op.create_table(
        "aave_v3_emode_categories",
        sa.Column("id", sa.Integer(), autoincrement=True, nullable=False),
        sa.Column("market_id", sa.Integer(), nullable=False),
        sa.Column("category_id", sa.Integer(), nullable=False),
        sa.Column("label", sa.Text(), nullable=True),
        sa.Column("ltv", sa.Integer(), nullable=False),
        sa.Column("liquidation_threshold", sa.Integer(), nullable=False),
        sa.Column("liquidation_bonus", sa.Integer(), nullable=False),
        sa.Column("price_source", sa.String(length=42), nullable=True),
        sa.ForeignKeyConstraint(
            ["market_id"],
            ["aave_v3_markets.id"],
        ),
        sa.PrimaryKeyConstraint("id"),
    )
    with op.batch_alter_table("aave_v3_emode_categories", schema=None) as batch_op:
        batch_op.create_index(
            "ix_aave_emode_category_market_cat", ["market_id", "category_id"], unique=True
        )
        batch_op.create_index(
            batch_op.f("ix_aave_v3_emode_categories_market_id"), ["market_id"], unique=False
        )

    op.create_table(
        "aave_v3_asset_configs",
        sa.Column("id", sa.Integer(), autoincrement=True, nullable=False),
        sa.Column("asset_id", sa.Integer(), nullable=False),
        sa.Column("ltv", sa.Integer(), nullable=False),
        sa.Column("liquidation_threshold", sa.Integer(), nullable=False),
        sa.Column("liquidation_bonus", sa.Integer(), nullable=False),
        sa.Column("e_mode_category_id", sa.Integer(), nullable=True),
        sa.Column("borrowing_enabled", sa.Boolean(), nullable=False),
        sa.Column("stable_borrowing_enabled", sa.Boolean(), nullable=False),
        sa.Column("flash_loan_enabled", sa.Boolean(), nullable=False),
        sa.Column("isolation_mode", sa.Boolean(), nullable=False),
        sa.Column(
            "debt_ceiling",
            degenbot.database.models.base.IntMappedToString(length=78),
            nullable=True,
        ),
        sa.ForeignKeyConstraint(
            ["asset_id"],
            ["aave_v3_assets.id"],
        ),
        sa.PrimaryKeyConstraint("id"),
    )
    with op.batch_alter_table("aave_v3_asset_configs", schema=None) as batch_op:
        batch_op.create_index("ix_aave_asset_config_asset", ["asset_id"], unique=True)
        batch_op.create_index(
            batch_op.f("ix_aave_v3_asset_configs_asset_id"), ["asset_id"], unique=False
        )

    op.create_table(
        "aave_v3_user_collateral_configs",
        sa.Column("id", sa.Integer(), autoincrement=True, nullable=False),
        sa.Column("user_id", sa.Integer(), nullable=False),
        sa.Column("asset_id", sa.Integer(), nullable=False),
        sa.Column("enabled", sa.Boolean(), nullable=False),
        sa.ForeignKeyConstraint(
            ["asset_id"],
            ["aave_v3_assets.id"],
        ),
        sa.ForeignKeyConstraint(
            ["user_id"],
            ["aave_v3_users.id"],
        ),
        sa.PrimaryKeyConstraint("id"),
    )
    with op.batch_alter_table("aave_v3_user_collateral_configs", schema=None) as batch_op:
        batch_op.create_index("ix_aave_user_collateral_config_enabled", ["enabled"], unique=False)
        batch_op.create_index(
            "ix_aave_user_collateral_config_user_asset", ["user_id", "asset_id"], unique=True
        )
        batch_op.create_index(
            batch_op.f("ix_aave_v3_user_collateral_configs_asset_id"), ["asset_id"], unique=False
        )
        batch_op.create_index(
            batch_op.f("ix_aave_v3_user_collateral_configs_user_id"), ["user_id"], unique=False
        )

    with op.batch_alter_table("aave_v3_assets", schema=None) as batch_op:
        batch_op.add_column(sa.Column("e_mode_category_id", sa.Integer(), nullable=True))
        batch_op.create_index(
            batch_op.f("ix_aave_v3_assets_e_mode_category_id"), ["e_mode_category_id"], unique=False
        )
        batch_op.create_foreign_key(
            "fk_aave_v3_assets_emode_category",
            "aave_v3_emode_categories",
            ["e_mode_category_id"],
            ["id"],
        )

    with op.batch_alter_table("aave_v3_users", schema=None) as batch_op:
        batch_op.add_column(
            sa.Column("isolation_mode_collateral_asset_id", sa.Integer(), nullable=True)
        )
        batch_op.add_column(
            sa.Column(
                "isolation_mode_debt",
                degenbot.database.models.base.IntMappedToString(length=78),
                nullable=False,
                server_default="0",
            )
        )
        batch_op.create_index(
            batch_op.f("ix_aave_v3_users_isolation_mode_collateral_asset_id"),
            ["isolation_mode_collateral_asset_id"],
            unique=False,
        )
        batch_op.create_foreign_key(
            "fk_aave_v3_users_isolation_collateral",
            "aave_v3_assets",
            ["isolation_mode_collateral_asset_id"],
            ["id"],
        )


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
