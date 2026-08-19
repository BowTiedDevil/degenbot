"""SQLAlchemy base model classes and column types."""

from __future__ import annotations

from typing import Annotated, ClassVar

from sqlalchemy import Dialect, String, Text
from sqlalchemy.orm import DeclarativeBase, Mapped, mapped_column
from sqlalchemy.types import TypeDecorator

# SQLAlchemy resolves Mapped[...] annotations at runtime via
# typing.get_type_hints, so this name must be importable (not TYPE_CHECKING-only).
from .types import PrimaryKeyInt  # ruff: ignore[typing-only-first-party-import]


class IntMappedToString(TypeDecorator[int]):
    """Map EVM integers (up to 32 bytes) to VARCHAR(78).

    Most SQL backends limit integers to 8 bytes, so this maps EVM
    values to a 78-character VARCHAR string representation.
    """

    cache_ok = True
    impl = String(78)

    def process_bind_param(  # ruff: ignore[no-self-use] # required to be a method per SQLAlchemy
        self,
        value: int | None,
        dialect: Dialect,  # ruff: ignore[unused-method-argument]
    ) -> str | None:
        """Perform the Python type -> DB type conversion.

        Returns:
            The computed value.

        """
        return None if value is None else str(value)

    def process_result_value(  # ruff: ignore[no-self-use] # required to be a method per SQLAlchemy
        self,
        value: str | None,
        dialect: Dialect,  # ruff: ignore[unused-method-argument]
    ) -> int | None:
        """Perform the DB type -> Python type conversion.

        Returns:
            The computed value.

        """
        return None if value is None else int(value)


Address = Annotated[str, mapped_column(String(42))]
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
