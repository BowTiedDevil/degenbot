"""adjust index

Revision ID: fb9abf7c7d57
Revises: 2afcff276d90
Create Date: 2025-10-17 10:37:43.037532

"""

from collections.abc import Sequence

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "fb9abf7c7d57"
down_revision: str | Sequence[str] | None = "2afcff276d90"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    with op.batch_alter_table("erc20_tokens", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_erc20_tokens_address_chain"))
        batch_op.create_index("ix_erc20_tokens_chain_address", ["chain", "address"], unique=True)


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
