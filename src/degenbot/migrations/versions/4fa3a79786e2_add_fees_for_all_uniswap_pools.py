"""add fees for all Uniswap pools

Revision ID: 4fa3a79786e2
Revises: beb3fdea3a89
Create Date: 2025-07-28 21:20:10.855765

"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op
from sqlalchemy.orm import sessionmaker

from degenbot.database import Pool

# revision identifiers, used by Alembic.
revision: str = "4fa3a79786e2"
down_revision: str | Sequence[str] | None = "beb3fdea3a89"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    # Add new columns with a default value which will be overwritten in the next step
    op.add_column(
        "pools",
        sa.Column("fee_token0", sa.Integer(), nullable=False, server_default=sa.text("-1")),
    )
    op.add_column(
        "pools",
        sa.Column("fee_token1", sa.Integer(), nullable=False, server_default=sa.text("-1")),
    )
    op.add_column(
        "pools",
        sa.Column("fee_denominator", sa.Integer(), nullable=False, server_default=sa.text("-1")),
    )

    connection = op.get_bind()
    session = sessionmaker(bind=connection)()
    with session.begin():
        # Copy the scalar fee value into the new columns
        session.execute(
            sa.update(Pool.__table__).values(fee_token0=Pool.__table__.c.fee),
        )
        session.execute(
            sa.update(Pool.__table__).values(fee_token1=Pool.__table__.c.fee),
        )
        # Set the denominator for V3 pools to 1_000_000
        session.execute(
            sa.update(Pool.__table__)
            .where(Pool.__table__.c.kind.like("%_v3"))
            .values(fee_denominator=1_000_000),
        )

    # SQLite doesn't support modifying individual columns via op.alter_column, so alter them with
    # a batch alteration of the table
    with op.batch_alter_table("pools") as batch_op:
        batch_op.alter_column("fee_token0", server_default=None)
        batch_op.alter_column("fee_token1", server_default=None)
        batch_op.alter_column("fee_denominator", server_default=None)
        batch_op.alter_column("fee", new_column_name="fee_deprecated")


def downgrade() -> None:
    """Downgrade schema."""

    # SQLite doesn't support modifying individual columns via op.alter_column, so alter them with
    # a batch alteration of the table
    with op.batch_alter_table("pools") as batch_op:
        batch_op.drop_column("fee_denominator")
        batch_op.drop_column("fee_token1")
        batch_op.drop_column("fee_token0")
        batch_op.alter_column("fee_deprecated", new_column_name="fee")
