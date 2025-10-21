"""add exchange relationship

Revision ID: 4eada4ae4a55
Revises: 7dc2ca38053f
Create Date: 2025-10-16 14:01:57.396204

"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

# revision identifiers, used by Alembic.
revision: str = "4eada4ae4a55"
down_revision: str | Sequence[str] | None = "7dc2ca38053f"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    with op.batch_alter_table("pools", schema=None) as batch_op:
        batch_op.add_column(sa.Column("exchange_id", sa.Integer(), nullable=True))
        batch_op.create_foreign_key(
            "fk_pools_exchange_id_exchanges",
            "exchanges",
            ["exchange_id"],
            ["id"],
        )

    connection = op.get_bind()
    rows = connection.execute(sa.text("SELECT id, name, chain_id FROM exchanges")).fetchall()
    exchange_map = {
        (exchange_name, exchange_chain_id): exchange_id
        for (
            exchange_id,
            exchange_name,
            exchange_chain_id,
        ) in rows
    }

    for (name, chain_id), exchange_id in exchange_map.items():
        connection.execute(
            sa.text(
                """
                UPDATE pools
                SET exchange_id = :exchange_id
                WHERE kind = :name AND chain = :chain_id
                """
            ),
            {
                "exchange_id": exchange_id,
                "name": name,
                "chain_id": chain_id,
            },
        )

    # Step 3: Make column non-nullable after data filled
    with op.batch_alter_table("pools", schema=None) as batch_op:
        batch_op.alter_column("exchange_id", existing_type=sa.Integer(), nullable=False)


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
