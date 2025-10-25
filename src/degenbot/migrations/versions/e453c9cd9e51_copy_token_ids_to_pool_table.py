"""Copy token IDs to pool table

Revision ID: e453c9cd9e51
Revises: bbb8d61cef9c
Create Date: 2025-10-25 12:07:25.023957

"""

from collections.abc import Sequence
from typing import TYPE_CHECKING

import sqlalchemy as sa
from alembic import op
from sqlalchemy.orm import Session

from degenbot.database.models.pools import (
    AbstractUniswapV2Pool,
    AbstractUniswapV3Pool,
    AerodromeV2PoolTable,
    AerodromeV3PoolTable,
    CamelotV2PoolTable,
    LiquidityPoolTable,
    PancakeswapV2PoolTable,
    PancakeswapV3PoolTable,
    SushiswapV2PoolTable,
    SushiswapV3PoolTable,
    SwapbasedV2PoolTable,
    UniswapV2PoolTable,
    UniswapV3PoolTable,
)

# revision identifiers, used by Alembic.
revision: str = "e453c9cd9e51"
down_revision: str | Sequence[str] | None = "bbb8d61cef9c"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    connection = op.get_bind()
    session = Session(bind=connection)

    for table in [
        AerodromeV2PoolTable,
        AerodromeV3PoolTable,
        CamelotV2PoolTable,
        PancakeswapV2PoolTable,
        PancakeswapV3PoolTable,
        SushiswapV2PoolTable,
        SushiswapV3PoolTable,
        SwapbasedV2PoolTable,
        UniswapV2PoolTable,
        UniswapV3PoolTable,
    ]:
        if TYPE_CHECKING:
            assert isinstance(
                table,
                (AbstractUniswapV2Pool, AbstractUniswapV3Pool),
            )

        for pool in session.scalars(sa.select(table)).all():
            base_pool = session.scalar(
                sa.select(LiquidityPoolTable).where(LiquidityPoolTable.id == pool.pool_id)
            )

            assert isinstance(pool, (AbstractUniswapV2Pool, AbstractUniswapV3Pool))
            assert isinstance(base_pool, LiquidityPoolTable)

            base_pool.token0_id_ = pool.token0_id
            base_pool.token1_id_ = pool.token1_id

    session.commit()


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
