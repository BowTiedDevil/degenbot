from sqlalchemy import Index
from sqlalchemy.orm import Mapped, declared_attr, mapped_column, relationship

from .base import Address, Base, BigInteger
from .types import (
    ForeignKeyManagedPoolId,
    ForeignKeyPoolId,
    ForeignKeyPoolManagerId,
    ManagedPoolHash,
    PrimaryForeignKeyManagedPoolId,
    PrimaryForeignKeyPoolId,
    PrimaryKeyInt,
)


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
    A mixin that adds columns common to all Uniswap V2 & V3 variants and a link to an indexed
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


class ManagedPoolLiquidityPositionTable(Base):
    __tablename__ = "managed_pool_liquidity_positions"

    id: Mapped[PrimaryKeyInt]
    managed_pool_id: Mapped[ForeignKeyManagedPoolId]
    tick: Mapped[int]
    liquidity_net: Mapped[BigInteger]
    liquidity_gross: Mapped[BigInteger]


# A (PoolId, tick) tuple is unique for each liquidity position
Index(
    "ix_managed_pool_liquidity_positions_pool_id_tick",
    ManagedPoolLiquidityPositionTable.managed_pool_id,
    ManagedPoolLiquidityPositionTable.tick,
    unique=True,
)


class ManagedPoolInitializationMapTable(Base):
    __tablename__ = "managed_pool_initialization_maps"

    id: Mapped[PrimaryKeyInt]
    managed_pool_id: Mapped[ForeignKeyManagedPoolId]
    word: Mapped[int]
    bitmap: Mapped[BigInteger]


# A (ManagedPoolId, word) tuple is unique for each initialization map
Index(
    "ix_managed_pool_initialization_maps_pool_id_word",
    ManagedPoolInitializationMapTable.managed_pool_id,
    ManagedPoolInitializationMapTable.word,
    unique=True,
)


class PoolManagerTable(Base):
    __tablename__ = "pool_managers"

    id: Mapped[PrimaryKeyInt]
    address: Mapped[Address] = mapped_column(index=True)
    chain: Mapped[int]
    kind: Mapped[str]


class ManagedLiquidityPoolTable(Base):
    __tablename__ = "managed_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_on": "kind",
        "polymorphic_identity": "base",
    }

    id: Mapped[PrimaryKeyInt]
    manager_id: Mapped[ForeignKeyPoolManagerId]
    kind: Mapped[str]


# TODO: investigate if token0/token1 and currency0/currency1 should map back to erc20 table


class AbstractUniswapV2Pool(LiquidityPoolTable, UniswapPoolCommonColumnsMixin):
    """
    This abstract class represents a parent for all Uniswap V2 variants. It should not be
    instantiated directly, but may be used to query and select child classes.
    """

    __abstract__ = True

    pool_id: Mapped[PrimaryForeignKeyPoolId]


class AerodromeV2PoolTable(AbstractUniswapV2Pool):
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


class AbstractUniswapV4Pool(ManagedLiquidityPoolTable):
    """
    This abstract class represents a parent for all Uniswap V4 variants. It should not be
    instantiated directly, but may be used to query and select child classes.
    """

    __abstract__ = True

    managed_pool_id: Mapped[PrimaryForeignKeyManagedPoolId]

    pool_hash: Mapped[ManagedPoolHash]
    hooks: Mapped[Address]
    currency0: Mapped[Address]
    currency1: Mapped[Address]
    fee_currency0: Mapped[int]
    fee_currency1: Mapped[int]
    fee_denominator: Mapped[int]
    tick_spacing: Mapped[int]
    has_liquidity: Mapped[bool]

    @declared_attr
    @classmethod
    def liquidity_positions(cls) -> Mapped[list[ManagedPoolLiquidityPositionTable]]:
        return relationship(
            "ManagedPoolLiquidityPositionTable",
            cascade="all, delete",
        )

    @declared_attr
    @classmethod
    def initialization_maps(cls) -> Mapped[list[ManagedPoolInitializationMapTable]]:
        return relationship(
            "ManagedPoolInitializationMapTable",
            cascade="all, delete",
        )


class UniswapV4PoolTable(AbstractUniswapV4Pool):
    __tablename__ = "uniswap_v4_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_on": "kind",
        "polymorphic_identity": "uniswap_v4",
    }
