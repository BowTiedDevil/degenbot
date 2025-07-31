import pathlib
from typing import Annotated, ClassVar

import pydantic
from alembic import command
from alembic.runtime.migration import MigrationContext
from alembic.script import ScriptDirectory
from sqlalchemy import Dialect, ForeignKey, Index, String, Text, create_engine
from sqlalchemy.orm import (
    DeclarativeBase,
    Mapped,
    declared_attr,
    mapped_column,
    relationship,
    scoped_session,
    sessionmaker,
)
from sqlalchemy.types import TypeDecorator

from degenbot import __version__
from degenbot.config import alembic_cfg, settings
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

        assert isinstance(value, (int, None))
        return None if value is None else str(value)

    def process_result_value(
        self,
        value: str | None,
        dialect: Dialect,  # noqa: ARG002
    ) -> int | None:
        """
        Perform the DB type -> Python type conversion.
        """

        assert isinstance(value, (str, None))
        return None if value is None else int(value)


Address = Annotated[
    str,
    mapped_column(String(42)),
]
BigInteger = Annotated[
    int,
    IntMappedToString,
]
PrimaryKeyInt = Annotated[
    int,
    mapped_column(primary_key=True, autoincrement=True),
]
PrimaryForeignKeyPoolId = Annotated[
    int,
    mapped_column(ForeignKey("pools.id"), primary_key=True),
]
ForeignKeyPoolId = Annotated[
    int,
    mapped_column(ForeignKey("pools.id")),
]


class Base(DeclarativeBase):
    type_annotation_map: ClassVar = {
        # keys must be Python types (native or Annotated)
        # values must be SQLAlchemy types
        BigInteger: IntMappedToString,
        str: Text,
    }


class Erc20TokenTable(Base):
    __tablename__ = "erc20_tokens"

    id: Mapped[PrimaryKeyInt]
    address: Mapped[Address]
    chain: Mapped[int]
    name: Mapped[str]
    symbol: Mapped[str]
    decimals: Mapped[int]


# A (address, ChainId) tuple is unique for ERC-20 tokens
Index(
    "ix_erc20_tokens_address_chain",
    Erc20TokenTable.address,
    Erc20TokenTable.chain,
    unique=True,
)


class MetadataTable(Base):
    __tablename__ = "metadata"

    id: Mapped[PrimaryKeyInt]
    key: Mapped[str]
    value: Mapped[str]


class LiquidityPositionTable(Base):
    __tablename__ = "liquidity_positions"

    id: Mapped[PrimaryKeyInt]
    pool_id: Mapped[ForeignKeyPoolId]
    tick: Mapped[int]
    liquidity_net: Mapped[BigInteger]
    liquidity_gross: Mapped[BigInteger]


# A (PoolId, tick) tuple is unique for each liquidity position
Index(
    "ix_liquidity_positions_pool_id_tick",
    LiquidityPositionTable.pool_id,
    LiquidityPositionTable.tick,
    unique=True,
)


class InitializationMapTable(Base):
    __tablename__ = "initialization_maps"

    id: Mapped[PrimaryKeyInt]
    pool_id: Mapped[ForeignKeyPoolId]
    word: Mapped[int]
    bitmap: Mapped[BigInteger]


# A (PoolId, word) tuple is unique for each initialization map
Index(
    "ix_initialization_maps_pool_id_word",
    InitializationMapTable.pool_id,
    InitializationMapTable.word,
    unique=True,
)


class UniswapPoolCommonColumnsMixin:
    """
    A mixin that adds columns common to all Uniswap variant classes and a link to an indexed
    foreign key for the pool ID.
    """

    token0: Mapped[Address]
    token1: Mapped[Address]
    factory: Mapped[Address | None]
    deployer: Mapped[Address | None]
    fee_token0: Mapped[int]
    fee_token1: Mapped[int]
    fee_denominator: Mapped[int]


class LiquidityPoolTable(Base):
    __tablename__ = "pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_on": "kind",
        "polymorphic_identity": "base",
    }

    id: Mapped[PrimaryKeyInt]
    address: Mapped[Address] = mapped_column(index=True)
    chain: Mapped[int]
    kind: Mapped[str]


class AbstractUniswapV2Pool(LiquidityPoolTable, UniswapPoolCommonColumnsMixin):
    """
    This abstract class represents a parent for all Uniswap V2 variants. It should not be
    instantiated directly, but may be used to query and select child classes.
    """

    __abstract__ = True

    pool_id: Mapped[PrimaryForeignKeyPoolId]


class AerodromeV2Pool(AbstractUniswapV2Pool):
    __tablename__ = "aerodrome_v2_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "aerodrome_v2",
    }


class CamelotV2PoolTable(AbstractUniswapV2Pool):
    __tablename__ = "camelot_v2_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "camelot_v2",
    }


class PancakeswapV2PoolTable(AbstractUniswapV2Pool):
    __tablename__ = "pancakeswap_v2_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "pancakeswap_v2",
    }


class SushiswapV2PoolTable(AbstractUniswapV2Pool):
    __tablename__ = "sushiswap_v2_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "sushiswap_v2",
    }


class SwapbasedV2PoolTable(AbstractUniswapV2Pool):
    __tablename__ = "swapbased_v2_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "swapbased_v2",
    }


class UniswapV2PoolTable(AbstractUniswapV2Pool):
    __tablename__ = "uniswap_v2_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "uniswap_v2",
    }


class AbstractUniswapV3Pool(LiquidityPoolTable, UniswapPoolCommonColumnsMixin):
    """
    This abstract class represents a parent for all Uniswap V3 variants. It should not be
    instantiated directly, but may be used to query and select child classes.
    """

    __abstract__ = True

    pool_id: Mapped[PrimaryForeignKeyPoolId]
    tick_spacing: Mapped[int]
    has_liquidity: Mapped[bool]

    @declared_attr
    @classmethod
    def liquidity_positions(cls) -> Mapped[list[LiquidityPositionTable]]:
        return relationship(
            "LiquidityPositionTable",
            cascade="all, delete",
        )

    @declared_attr
    @classmethod
    def initialization_maps(cls) -> Mapped[list[InitializationMapTable]]:
        return relationship(
            "InitializationMapTable",
            cascade="all, delete",
        )


class AerodromeV3PoolTable(AbstractUniswapV3Pool):
    __tablename__ = "aerodrome_v3_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "aerodrome_v3",
    }


class UniswapV3PoolTable(AbstractUniswapV3Pool):
    __tablename__ = "uniswap_v3_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "uniswap_v3",
    }


class PancakeswapV3PoolTable(AbstractUniswapV3Pool):
    __tablename__ = "pancakeswap_v3_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "pancakeswap_v3",
    }


class SushiswapV3PoolTable(AbstractUniswapV3Pool):
    __tablename__ = "sushiswap_v3_pools"
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
    command.stamp(alembic_cfg, "head")


def upgrade_existing_sqlite_database() -> None:
    command.upgrade(alembic_cfg, "head")
    logger.info(f"Updated existing SQLite database at {settings.database.path.absolute()}")


_default_engine = create_engine(
    f"sqlite:///{settings.database.path.absolute()}",
)
default_session = scoped_session(
    session_factory=sessionmaker(bind=_default_engine),
)

current_database_version = MigrationContext.configure(
    default_session.connection()
).get_current_revision()
latest_database_version = ScriptDirectory.from_config(alembic_cfg).get_current_head()

if latest_database_version is not None and current_database_version != latest_database_version:
    logger.warning(
        f"The current database revision ({current_database_version}) does not match the latest "
        f"({latest_database_version}) for {__package__} version {__version__}!"
        "\n"
        "Database-related features may raise exceptions if you continue! Run database migrations "
        "with 'degenbot database upgrade'."
    )
