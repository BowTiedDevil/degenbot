"""Uniswap V4: Backfill state view addresses

Revision ID: 06a3739885a0
Revises: 6f376d34618b
Create Date: 2025-11-06 13:39:45.143999

"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op
from eth_typing import ChainId
from sqlalchemy.orm import Session

from degenbot.database.models.pools import PoolManagerTable

# revision identifiers, used by Alembic.
revision: str = "06a3739885a0"
down_revision: str | Sequence[str] | None = "6f376d34618b"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    connection = op.get_bind()
    session = Session(bind=connection)

    for pool_manager in session.scalars(sa.select(PoolManagerTable)).all():
        if (
            pool_manager.address == "0x000000000004444c5dc75cB358380D2e3dE08A90"
            and pool_manager.chain == ChainId.ETH
            and pool_manager.state_view is None
        ):
            pool_manager.state_view = "0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227"

        elif (
            pool_manager.address == "0x498581fF718922c3f8e6A244956aF099B2652b2b"
            and pool_manager.chain == ChainId.BASE
            and pool_manager.state_view is None
        ):
            pool_manager.state_view = "0xA3c0c9b65baD0b08107Aa264b0f3dB444b867A71"

    session.commit()


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
