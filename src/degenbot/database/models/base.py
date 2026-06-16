"""SQLAlchemy base model classes and column types."""

from typing import Annotated, ClassVar

from eth_typing import ChecksumAddress
from sqlalchemy import Dialect, String, Text
from sqlalchemy.orm import DeclarativeBase, Mapped, mapped_column
from sqlalchemy.types import TypeDecorator

from .types import PrimaryKeyInt


class IntMappedToString(TypeDecorator[int]):
    """Map EVM integers (up to 32 bytes) to VARCHAR(78).

    Most SQL backends limit integers to 8 bytes, so this maps EVM
    values to a 78-character VARCHAR string representation.
    """

    cache_ok = True
    impl = String(78)

    def process_bind_param(  # ruff: ignore[PLR6301] # required to be a method per SQLAlchemy
        self,
        value: int | None,
        dialect: Dialect,  # ruff: ignore[ARG002]
    ) -> str | None:
        """Perform the Python type -> DB type conversion.

        Returns:
            The computed value.

        """
        return None if value is None else str(value)

    def process_result_value(  # ruff: ignore[PLR6301] # required to be a method per SQLAlchemy
        self,
        value: str | None,
        dialect: Dialect,  # ruff: ignore[ARG002]
    ) -> int | None:
        """Perform the DB type -> Python type conversion.

        Returns:
            The computed value.

        """
        return None if value is None else int(value)


Address = Annotated[ChecksumAddress, mapped_column(String(42))]
BigInteger = Annotated[int, IntMappedToString]


class Base(DeclarativeBase):
    """Base class."""

    type_annotation_map: ClassVar = {
        # keys must be Python types (native or Annotated)
        # values must be SQLAlchemy types
        BigInteger: IntMappedToString,
        str: Text,
    }


class ExchangeTable(Base):
    """ExchangeTable class."""

    __tablename__ = "exchanges"

    id: Mapped[PrimaryKeyInt]
    chain_id: Mapped[int]
    name: Mapped[str]
    active: Mapped[bool]
    last_update_block: Mapped[int | None]
    factory: Mapped[Address]
    deployer: Mapped[Address | None]
