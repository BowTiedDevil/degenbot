"""SQLAlchemy model for ERC-20 token metadata."""

from sqlalchemy import Index
from sqlalchemy.orm import Mapped

from .base import Address, Base
from .types import PrimaryKeyInt


class Erc20TokenTable(Base):
    """Erc20TokenTable class."""

    __tablename__ = "erc20_tokens"

    id: Mapped[PrimaryKeyInt]
    chain: Mapped[int]
    address: Mapped[Address]
    name: Mapped[str | None]
    symbol: Mapped[str | None]
    decimals: Mapped[int | None]

    def __repr__(self) -> str:
        """Return a string representation.

        Returns:
            A string representation of the object.

        """
        return (
            f"{self.__class__.__name__}("
            f"chain={self.chain!r}, "
            f"address={self.address!r}, "
            f"symbol={self.symbol!r}"
            f")"
        )


# The (address, ChainId) tuple is unique for ERC-20 tokens
Index(
    "ix_erc20_tokens_address_chain",
    Erc20TokenTable.address,
    Erc20TokenTable.chain,
    unique=True,
)

# Supports queries filtering by chain without also filtering on address
Index(
    "ix_erc20_tokens_chain",
    Erc20TokenTable.chain,
    unique=False,
)
