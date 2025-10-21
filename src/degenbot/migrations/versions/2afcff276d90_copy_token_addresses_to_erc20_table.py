"""copy token addresses to erc20 table

Revision ID: 2afcff276d90
Revises: 4eada4ae4a55
Create Date: 2025-10-17 10:30:01.354905

"""

from collections.abc import Sequence
from typing import TYPE_CHECKING

import sqlalchemy as sa
from alembic import op
from sqlalchemy.orm import Session

from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.database.models.pools import (
    AbstractUniswapV2Pool,
    AbstractUniswapV3Pool,
    AerodromeV2PoolTable,
    AerodromeV3PoolTable,
    CamelotV2PoolTable,
    PancakeswapV2PoolTable,
    PancakeswapV3PoolTable,
    PoolManagerTable,
    SushiswapV2PoolTable,
    SushiswapV3PoolTable,
    SwapbasedV2PoolTable,
    UniswapV2PoolTable,
    UniswapV3PoolTable,
    UniswapV4PoolTable,
)

# revision identifiers, used by Alembic.
revision: str = "2afcff276d90"
down_revision: str | Sequence[str] | None = "4eada4ae4a55"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    with op.batch_alter_table("erc20_tokens", schema=None) as batch_op:
        batch_op.alter_column("name", existing_type=sa.TEXT(), nullable=True)
        batch_op.alter_column("symbol", existing_type=sa.TEXT(), nullable=True)
        batch_op.alter_column("decimals", existing_type=sa.INTEGER(), nullable=True)

    connection = op.get_bind()
    session = Session(bind=connection)

    new_token_addresses: set[tuple[int, str]] = set()
    existing_token_addresses: set[tuple[int, str]] = set()

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
            assert isinstance(table, (AbstractUniswapV2Pool, AbstractUniswapV3Pool))

        for chain_id, token0_address, token1_address in session.query(
            table.chain,
            table.token0,
            table.token1,
        ).all():
            new_token_addresses.add((chain_id, token0_address))
            new_token_addresses.add((chain_id, token1_address))

    for pool_id, token0_address, token1_address in session.query(
        UniswapV4PoolTable.managed_pool_id,
        UniswapV4PoolTable.currency0,
        UniswapV4PoolTable.currency1,
    ).all():
        # Get the chain ID from the manager_id
        chain_id = session.scalar(
            sa.select(PoolManagerTable.chain).where(PoolManagerTable.id == pool_id)
        )
        if TYPE_CHECKING:
            assert chain_id is not None

        new_token_addresses.add((chain_id, token0_address))
        new_token_addresses.add((chain_id, token1_address))

    # Fetch existing addresses to avoid re-insertion
    existing_token_addresses.update(
        (chain_id, token_address)
        for chain_id, token_address in session.query(
            Erc20TokenTable.chain, Erc20TokenTable.address
        ).all()
    )

    if new_token_addresses:
        session.add_all(
            Erc20TokenTable(chain=chain_id, address=addr)
            for chain_id, addr in new_token_addresses - existing_token_addresses
        )

    session.commit()


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
