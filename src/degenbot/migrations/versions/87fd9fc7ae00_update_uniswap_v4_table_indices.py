"""Update Uniswap V4 table indices

Revision ID: 87fd9fc7ae00
Revises: 756fba1f75f4
Create Date: 2025-09-05 10:37:01.739730

"""

from collections.abc import Sequence

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "87fd9fc7ae00"
down_revision: str | Sequence[str] | None = "756fba1f75f4"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    op.drop_index(op.f("ix_pool_managers_address"), table_name="pool_managers")
    op.create_index(op.f("ix_pool_managers_address"), "pool_managers", ["address"], unique=True)
    op.drop_index(op.f("ix_managed_pool_hash"), table_name="uniswap_v4_pools")
    op.create_index(
        op.f("ix_uniswap_v4_pools_pool_hash"), "uniswap_v4_pools", ["pool_hash"], unique=True
    )


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
