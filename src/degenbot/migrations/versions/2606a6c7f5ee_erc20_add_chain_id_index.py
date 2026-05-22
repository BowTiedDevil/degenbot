"""
ERC20: add chain ID index.

Revision ID: 2606a6c7f5ee
Revises: e0aaad8ad486
Create Date: 2026-05-21 16:54:13.702427

"""

from collections.abc import Sequence

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "2606a6c7f5ee"
down_revision: str | Sequence[str] | None = "e0aaad8ad486"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    with op.batch_alter_table("erc20_tokens", schema=None) as batch_op:
        batch_op.create_index("ix_erc20_tokens_chain", ["chain"], unique=False)


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
