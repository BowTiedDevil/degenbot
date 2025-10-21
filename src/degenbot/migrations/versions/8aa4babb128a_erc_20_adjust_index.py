"""ERC-20: adjust index

Revision ID: 8aa4babb128a
Revises: 8c69198e6a21
Create Date: 2025-10-20 21:53:46.297738

"""

from collections.abc import Sequence

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "8aa4babb128a"
down_revision: str | Sequence[str] | None = "8c69198e6a21"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""

    with op.batch_alter_table("erc20_tokens", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_erc20_tokens_chain_address"))
        batch_op.create_index("ix_erc20_tokens_address_chain", ["address", "chain"], unique=True)


def downgrade() -> None:
    """Downgrade schema."""
    msg = "Downgrade is not supported for this migration."
    raise NotImplementedError(msg)
