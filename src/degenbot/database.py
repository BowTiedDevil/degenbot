import pathlib
from typing import Annotated, ClassVar

import pydantic
from sqlalchemy import Dialect, ForeignKey, Index, String, Text, create_engine
from sqlalchemy.orm import DeclarativeBase, Mapped, mapped_column, relationship, sessionmaker
from sqlalchemy.types import TypeDecorator

from degenbot.config import settings
from degenbot.logging import logger

type Tick = int
type Word = int


class TicksAtWord(pydantic.BaseModel):
    bitmap: int


class LiquidityAtTick(pydantic.BaseModel):
    liquidity_net: int
    liquidity_gross: int


class PoolLiquidityMap(pydantic.BaseModel):
    tick_bitmap: dict[Word, TicksAtWord]
    tick_data: dict[Tick, LiquidityAtTick]


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

        assert isinstance(value, int | None)
        return None if value is None else str(value)

    def process_result_value(
        self,
        value: str | None,
        dialect: Dialect,  # noqa: ARG002
    ) -> int | None:
        """
        Perform the DB type -> Python type conversion.
        """

        assert isinstance(value, str | None)
        return None if value is None else int(value)


Address = Annotated[str, mapped_column(String(42))]
BigInteger = Annotated[int, IntMappedToString]
PrimaryKeyInteger = Annotated[int, mapped_column(primary_key=True)]
IndexedForeignKeyPoolId = Annotated[int, mapped_column(ForeignKey("pools.id"), index=True)]


class Base(DeclarativeBase):
    type_annotation_map: ClassVar = {
        # keys must be Python types (native or Annotated)
        # values must be SQLAlchemy types
        BigInteger: IntMappedToString,
        str: Text,
    }


class Erc20TokenTableEntry(Base):
    __tablename__ = "erc20_tokens"

    id: Mapped[PrimaryKeyInteger]
    address: Mapped[Address]
    chain: Mapped[int]
    name: Mapped[str]
    symbol: Mapped[str]
    decimals: Mapped[int]


# A (address, ChainId) tuple is unique for ERC-20 tokens
Index(
    "ix_erc20_tokens_address_chain",
    Erc20TokenTableEntry.address,
    Erc20TokenTableEntry.chain,
    unique=True,
)


class MetadataTableEntry(Base):
    __tablename__ = "metadata"

    id: Mapped[PrimaryKeyInteger]
    key: Mapped[str]
    value: Mapped[str]


class LiquidityPositionTableEntry(Base):
    __tablename__ = "liquidity_positions"

    id: Mapped[PrimaryKeyInteger]
    pool_id: Mapped[IndexedForeignKeyPoolId]
    tick: Mapped[int]
    liquidity_net: Mapped[BigInteger]
    liquidity_gross: Mapped[BigInteger]


# A (PoolId, tick) tuple is unique for each liquidity position
Index(
    "ix_liquidity_positions_pool_id_tick",
    LiquidityPositionTableEntry.pool_id,
    LiquidityPositionTableEntry.tick,
    unique=True,
)


class InitializationMapTableEntry(Base):
    __tablename__ = "initialization_maps"

    id: Mapped[PrimaryKeyInteger]
    pool_id: Mapped[IndexedForeignKeyPoolId]
    word: Mapped[int]
    bitmap: Mapped[BigInteger]


# A (PoolId, word) tuple is unique for each initialization map
Index(
    "ix_initialization_maps_pool_id_word",
    InitializationMapTableEntry.pool_id,
    InitializationMapTableEntry.word,
    unique=True,
)


class Pool(Base):
    __tablename__ = "pools"

    id: Mapped[PrimaryKeyInteger]
    address: Mapped[Address] = mapped_column(unique=True, index=True)
    chain: Mapped[int]
    kind: Mapped[str] = mapped_column()

    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_on": kind,
        "polymorphic_identity": "pool",
    }


class AbstractUniswapPool(Pool):
    """
    This abstract class serves as a parent container for columns common to all Uniswap variant
    child classes. This class should not be directly instantiated.
    """

    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_abstract": True,
    }

    token0: Mapped[Address]
    token1: Mapped[Address]
    factory: Mapped[Address | None]
    deployer: Mapped[Address | None]


class UniswapV2Pool(AbstractUniswapPool):
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "uniswap_v2",
    }


class UniswapV3Pool(AbstractUniswapPool):
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "uniswap_v3",
    }

    fee: Mapped[int]
    tick_spacing: Mapped[int]
    has_liquidity: Mapped[bool]

    liquidity_positions: Mapped[list[LiquidityPositionTableEntry] | None] = relationship()
    initialization_maps: Mapped[list[InitializationMapTableEntry] | None] = relationship()


class AerodromeV3Pool(UniswapV3Pool):
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "aerodrome_v3",
    }


class PancakeswapV3Pool(UniswapV3Pool):
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "pancakeswap_v3",
    }


class SushiswapV3Pool(UniswapV3Pool):
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "sushiswap_v3",
    }


def create_new_sqlite_database(db_path: pathlib.Path) -> None:
    if db_path.exists():
        db_path.unlink()

    engine = create_engine(
        f"sqlite:///{db_path.absolute()}",
    )
    Base.metadata.create_all(bind=engine)
    logger.info(f"Initialized new SQLite database at {db_path}")


def upgrade_existing_sqlite_database(db_path: pathlib.Path) -> None:
    db_path_abs = db_path.absolute()
    engine = create_engine(
        f"sqlite:///{db_path_abs}",
    )
    Base.metadata.create_all(bind=engine)
    logger.info(f"Updated existing SQLite database at {db_path_abs}")


_default_engine = create_engine(
    f"sqlite:///{settings.database.path.absolute()}",
)
default_session = sessionmaker(_default_engine)()
