"""V3 pools: convert token0/token1 to foreign keys

Revision ID: 50b39bafa0be
Revises: b8e0b921299a
Create Date: 2025-10-17 14:21:06.040240

"""

from collections.abc import Sequence
from typing import TYPE_CHECKING

import sqlalchemy as sa
from alembic import op
from sqlalchemy.orm import Session

from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.database.models.pools import (
    AbstractUniswapV3Pool,
    AerodromeV3PoolTable,
    PancakeswapV3PoolTable,
    SushiswapV3PoolTable,
    UniswapV3PoolTable,
)

# revision identifiers, used by Alembic.
revision: str = "50b39bafa0be"
down_revision: str | Sequence[str] | None = "b8e0b921299a"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    connection = op.get_bind()
    session = Session(bind=connection)

    # Build all token references
    tokens: set[tuple[int, str]] = set()

    for table in [
        AerodromeV3PoolTable,
        PancakeswapV3PoolTable,
        SushiswapV3PoolTable,
        UniswapV3PoolTable,
    ]:
        if TYPE_CHECKING:
            assert isinstance(table, AbstractUniswapV3Pool)

        tokens.update(session.query(table.chain, table.token0).all())
        tokens.update(session.query(table.chain, table.token1).all())

        token_ids: dict[tuple[int, str], int] = {
            (chain_id, token_address): session.scalar(
                sa.select(Erc20TokenTable.id).where(
                    Erc20TokenTable.chain == chain_id,
                    Erc20TokenTable.address == token_address,
                )
            )
            for chain_id, token_address in tokens
        }

        with op.batch_alter_table(table.__tablename__, schema=None) as batch_op:
            batch_op.add_column(sa.Column("token0_id", sa.Integer(), nullable=True))
            batch_op.add_column(sa.Column("token1_id", sa.Integer(), nullable=True))
            batch_op.create_foreign_key("fk_token0_id", "erc20_tokens", ["token0_id"], ["id"])
            batch_op.create_foreign_key("fk_token1_id", "erc20_tokens", ["token1_id"], ["id"])

        for pool_id, token0, token1 in connection.execute(
            sa.text(f"SELECT pool_id,token0,token1 FROM {table.__tablename__}")  # noqa: S608
        ).fetchall():
            pool_chain = session.scalar(sa.select(table.chain).where(table.pool_id == pool_id))
            connection.execute(
                sa.text(
                    f"""
                    UPDATE {table.__tablename__}\
                    SET token0_id = :token0_value, token1_id = :token1_value\
                    WHERE pool_id = :pool_id"""  # noqa: S608
                ),
                {
                    "pool_id": pool_id,
                    "token0_value": token_ids[(pool_chain, token0)],
                    "token1_value": token_ids[(pool_chain, token1)],
                },
            )

        with op.batch_alter_table(table.__tablename__, schema=None) as batch_op:
            # Drop the token address columns
            batch_op.drop_column("token0")
            batch_op.drop_column("token1")

            # Make column non-nullable after the keys are populated
            batch_op.alter_column("token0_id", existing_type=sa.Integer(), nullable=False)
            batch_op.alter_column("token1_id", existing_type=sa.Integer(), nullable=False)


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
