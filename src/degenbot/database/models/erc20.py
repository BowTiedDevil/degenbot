from sqlalchemy import Index
from sqlalchemy.orm import Mapped

from .base import Address, Base
from .types import PrimaryKeyInt


class Erc20TokenTable(Base):
    __tablename__ = "erc20_tokens"

    id: Mapped[PrimaryKeyInt]
    address: Mapped[Address]
    chain: Mapped[int]
    name: Mapped[str]
    symbol: Mapped[str]
    decimals: Mapped[int]


# The (address, ChainId) tuple is unique for ERC-20 tokens
Index(
    "ix_erc20_tokens_address_chain",
    Erc20TokenTable.address,
    Erc20TokenTable.chain,
    unique=True,
)
