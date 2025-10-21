from typing import Annotated, ClassVar

from sqlalchemy import Dialect, String, Text
from sqlalchemy.orm import DeclarativeBase, Mapped, mapped_column
from sqlalchemy.types import TypeDecorator

from .types import PrimaryKeyInt


class IntMappedToString(TypeDecorator[int]):
    """
    EVM integers can be up to 32 bytes, which exceeds the usual 8 byte limit for most SQL backends.
    Map these values to a 78 character VARCHAR which can hold a string representation of all
    possible values.
    """

    cache_ok = True
    impl = String(78)

    def process_bind_param(
        self,
        value: int | None,
        dialect: Dialect,  # noqa: ARG002
    ) -> str | None:
        """
        Perform the Python type -> DB type conversion.
        """

        return None if value is None else str(value)

    def process_result_value(
        self,
        value: str | None,
        dialect: Dialect,  # noqa: ARG002
    ) -> int | None:
        """
        Perform the DB type -> Python type conversion.
        """

        return None if value is None else int(value)


Address = Annotated[str, mapped_column(String(42))]
BigInteger = Annotated[int, IntMappedToString]


class Base(DeclarativeBase):
    type_annotation_map: ClassVar = {
        # keys must be Python types (native or Annotated)
        # values must be SQLAlchemy types
        BigInteger: IntMappedToString,
        str: Text,
    }


class ExchangeTable(Base):
    __tablename__ = "exchanges"

    id: Mapped[PrimaryKeyInt]
    chain_id: Mapped[int]
    name: Mapped[str]
    active: Mapped[bool]
    last_update_block: Mapped[int | None]
    factory: Mapped[Address]
    deployer: Mapped[Address | None]
