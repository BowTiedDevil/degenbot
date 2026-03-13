"""Aave V3: fix GHO table

Revision ID: 5d2dfe7292b3
Revises: a512dbce9854
Create Date: 2026-03-12 21:39:16.781991

"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

# revision identifiers, used by Alembic.
revision: str = "5d2dfe7292b3"
down_revision: str | Sequence[str] | None = "a512dbce9854"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    with op.batch_alter_table("aave_gho_tokens", schema=None) as batch_op:
        batch_op.add_column(sa.Column("v_token_id", sa.Integer(), nullable=True))
        batch_op.create_index(
            batch_op.f("ix_aave_gho_tokens_v_token_id"), ["v_token_id"], unique=False
        )
        batch_op.create_foreign_key(
            "fk_aave_gho_tokens_v_token_id_erc20_tokens",
            "erc20_tokens",
            ["v_token_id"],
            ["id"],
        )


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
