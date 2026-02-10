"""Aave V3: add tables

Revision ID: a512dbce9854
Revises: eb4080485a56
Create Date: 2026-02-10 10:22:18.532963

"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

import degenbot.database.models

# revision identifiers, used by Alembic.
revision: str = "a512dbce9854"
down_revision: str | Sequence[str] | None = "eb4080485a56"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    op.create_table(
        "aave_v3_markets",
        sa.Column("id", sa.Integer(), autoincrement=True, nullable=False),
        sa.Column("chain_id", sa.Integer(), nullable=False),
        sa.Column("name", sa.Text(), nullable=False),
        sa.Column("active", sa.Boolean(), nullable=False),
        sa.Column("last_update_block", sa.Integer(), nullable=True),
        sa.PrimaryKeyConstraint("id"),
    )
    op.create_table(
        "aave_gho_tokens",
        sa.Column("id", sa.Integer(), autoincrement=True, nullable=False),
        sa.Column("token_id", sa.Integer(), nullable=False),
        sa.Column("v_gho_discount_rate_strategy", sa.String(length=42), nullable=True),
        sa.Column("v_gho_discount_token", sa.String(length=42), nullable=True),
        sa.ForeignKeyConstraint(
            ["token_id"],
            ["erc20_tokens.id"],
        ),
        sa.PrimaryKeyConstraint("id"),
    )
    with op.batch_alter_table("aave_gho_tokens", schema=None) as batch_op:
        batch_op.create_index(batch_op.f("ix_aave_gho_tokens_token_id"), ["token_id"], unique=False)

    op.create_table(
        "aave_v3_assets",
        sa.Column("id", sa.Integer(), autoincrement=True, nullable=False),
        sa.Column("market_id", sa.Integer(), nullable=False),
        sa.Column("underlying_asset_id", sa.Integer(), nullable=False),
        sa.Column("a_token_id", sa.Integer(), nullable=False),
        sa.Column("a_token_revision", sa.Integer(), nullable=False),
        sa.Column("v_token_id", sa.Integer(), nullable=False),
        sa.Column("v_token_revision", sa.Integer(), nullable=False),
        sa.Column("last_update_block", sa.Integer(), nullable=True),
        sa.Column(
            "liquidity_index",
            degenbot.database.models.base.IntMappedToString(length=78),
            nullable=False,
        ),
        sa.Column(
            "liquidity_rate",
            degenbot.database.models.base.IntMappedToString(length=78),
            nullable=False,
        ),
        sa.Column(
            "borrow_index",
            degenbot.database.models.base.IntMappedToString(length=78),
            nullable=False,
        ),
        sa.Column(
            "borrow_rate",
            degenbot.database.models.base.IntMappedToString(length=78),
            nullable=False,
        ),
        sa.ForeignKeyConstraint(
            ["a_token_id"],
            ["erc20_tokens.id"],
        ),
        sa.ForeignKeyConstraint(
            ["market_id"],
            ["aave_v3_markets.id"],
        ),
        sa.ForeignKeyConstraint(
            ["underlying_asset_id"],
            ["erc20_tokens.id"],
        ),
        sa.ForeignKeyConstraint(
            ["v_token_id"],
            ["erc20_tokens.id"],
        ),
        sa.PrimaryKeyConstraint("id"),
    )
    with op.batch_alter_table("aave_v3_assets", schema=None) as batch_op:
        batch_op.create_index(
            "ix_aave_assets_underlying_asset_market",
            ["underlying_asset_id", "market_id"],
            unique=True,
        )
        batch_op.create_index(
            batch_op.f("ix_aave_v3_assets_a_token_id"), ["a_token_id"], unique=False
        )
        batch_op.create_index(
            batch_op.f("ix_aave_v3_assets_market_id"), ["market_id"], unique=False
        )
        batch_op.create_index(
            batch_op.f("ix_aave_v3_assets_underlying_asset_id"),
            ["underlying_asset_id"],
            unique=False,
        )
        batch_op.create_index(
            batch_op.f("ix_aave_v3_assets_v_token_id"), ["v_token_id"], unique=False
        )

    op.create_table(
        "aave_v3_contracts",
        sa.Column("id", sa.Integer(), autoincrement=True, nullable=False),
        sa.Column("market_id", sa.Integer(), nullable=False),
        sa.Column("name", sa.Text(), nullable=False),
        sa.Column("address", sa.String(length=42), nullable=False),
        sa.Column("revision", sa.Integer(), nullable=True),
        sa.ForeignKeyConstraint(
            ["market_id"],
            ["aave_v3_markets.id"],
        ),
        sa.PrimaryKeyConstraint("id"),
    )
    with op.batch_alter_table("aave_v3_contracts", schema=None) as batch_op:
        batch_op.create_index(
            batch_op.f("ix_aave_v3_contracts_market_id"), ["market_id"], unique=False
        )

    op.create_table(
        "aave_v3_users",
        sa.Column("id", sa.Integer(), autoincrement=True, nullable=False),
        sa.Column("market_id", sa.Integer(), nullable=False),
        sa.Column("address", sa.String(length=42), nullable=False),
        sa.Column("e_mode", sa.Integer(), nullable=False),
        sa.Column("gho_discount", sa.Integer(), nullable=False),
        sa.Column(
            "stk_aave_balance",
            degenbot.database.models.base.IntMappedToString(length=78),
            nullable=True,
        ),
        sa.ForeignKeyConstraint(
            ["market_id"],
            ["aave_v3_markets.id"],
        ),
        sa.PrimaryKeyConstraint("id"),
    )
    with op.batch_alter_table("aave_v3_users", schema=None) as batch_op:
        batch_op.create_index("ix_aave_users_address_market", ["address", "market_id"], unique=True)
        batch_op.create_index(batch_op.f("ix_aave_v3_users_market_id"), ["market_id"], unique=False)

    op.create_table(
        "aave_v3_collateral_positions",
        sa.Column("id", sa.Integer(), autoincrement=True, nullable=False),
        sa.Column("user_id", sa.Integer(), nullable=False),
        sa.Column("asset_id", sa.Integer(), nullable=False),
        sa.Column(
            "balance", degenbot.database.models.base.IntMappedToString(length=78), nullable=False
        ),
        sa.Column(
            "last_index", degenbot.database.models.base.IntMappedToString(length=78), nullable=True
        ),
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
    with op.batch_alter_table("aave_v3_collateral_positions", schema=None) as batch_op:
        batch_op.create_index(
            "ix_aave_collateral_position_user_asset", ["user_id", "asset_id"], unique=True
        )
        batch_op.create_index(
            batch_op.f("ix_aave_v3_collateral_positions_asset_id"), ["asset_id"], unique=False
        )
        batch_op.create_index(
            batch_op.f("ix_aave_v3_collateral_positions_user_id"), ["user_id"], unique=False
        )

    op.create_table(
        "aave_v3_debt_positions",
        sa.Column("id", sa.Integer(), autoincrement=True, nullable=False),
        sa.Column("user_id", sa.Integer(), nullable=False),
        sa.Column("asset_id", sa.Integer(), nullable=False),
        sa.Column(
            "balance", degenbot.database.models.base.IntMappedToString(length=78), nullable=False
        ),
        sa.Column(
            "last_index", degenbot.database.models.base.IntMappedToString(length=78), nullable=True
        ),
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
    with op.batch_alter_table("aave_v3_debt_positions", schema=None) as batch_op:
        batch_op.create_index(
            "ix_aave_debt_position_user_asset", ["user_id", "asset_id"], unique=True
        )
        batch_op.create_index(
            batch_op.f("ix_aave_v3_debt_positions_asset_id"), ["asset_id"], unique=False
        )
        batch_op.create_index(
            batch_op.f("ix_aave_v3_debt_positions_user_id"), ["user_id"], unique=False
        )


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
