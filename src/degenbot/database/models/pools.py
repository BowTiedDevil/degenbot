from sqlalchemy import ForeignKey, Index
from sqlalchemy.orm import Mapped, declared_attr, mapped_column, relationship

from degenbot.database.models.erc20 import Erc20TokenTable

from .base import Address, Base, BigInteger, ExchangeTable
from .types import (
    ForeignKeyManagedPoolId,
    ForeignKeyPoolId,
    ForeignKeyPoolManagerId,
    ForeignKeyTokenId,
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


# The (PoolId, tick) tuple is unique for each liquidity position
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


# The (PoolId, word) tuple is unique for each initialization map
Index(
    "ix_initialization_maps_pool_id_word",
    InitializationMapTable.pool_id,
    InitializationMapTable.word,
    unique=True,
)


class UniswapPoolCommonColumnsMixin:
    """
    A mixin that adds columns common to all Uniswap V2 & V3 variants.
    """

    token0_id: Mapped[ForeignKeyTokenId]
    token1_id: Mapped[ForeignKeyTokenId]
    fee_token0: Mapped[int]
    fee_token1: Mapped[int]
    fee_denominator: Mapped[int]

    @declared_attr
    @classmethod
    def token0(cls) -> Mapped[Erc20TokenTable]:
        return relationship(
            "Erc20TokenTable",
            foreign_keys=cls.token0_id,
        )

    @declared_attr
    @classmethod
    def token1(cls) -> Mapped[Erc20TokenTable]:
        return relationship(
            "Erc20TokenTable",
            foreign_keys=cls.token1_id,
        )


class LiquidityPoolTable(Base):
    __tablename__ = "pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_on": "kind",
        "polymorphic_identity": "base",
    }

    id: Mapped[PrimaryKeyInt]
    address: Mapped[Address]
    chain: Mapped[int]
    kind: Mapped[str]

    exchange: Mapped[ExchangeTable] = relationship("ExchangeTable")
    exchange_id: Mapped[int] = mapped_column(ForeignKey("exchanges.id"))


# The (address, chainId) tuple is unique for each liquidity pool
Index(
    "ix_liquidity_pool_address_chain",
    LiquidityPoolTable.address,
    LiquidityPoolTable.chain,
    unique=True,
)


class ManagedPoolLiquidityPositionTable(Base):
    __tablename__ = "managed_pool_liquidity_positions"

    id: Mapped[PrimaryKeyInt]
    managed_pool_id: Mapped[ForeignKeyManagedPoolId]
    tick: Mapped[int]
    liquidity_net: Mapped[BigInteger]
    liquidity_gross: Mapped[BigInteger]


# The (PoolId, tick) tuple is unique for each liquidity position
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


# The (ManagedPoolId, word) tuple is unique for each initialization map
Index(
    "ix_managed_pool_initialization_maps_pool_id_word",
    ManagedPoolInitializationMapTable.managed_pool_id,
    ManagedPoolInitializationMapTable.word,
    unique=True,
)


class PoolManagerTable(Base):
    __tablename__ = "pool_managers"

    id: Mapped[PrimaryKeyInt]
    address: Mapped[Address]
    chain: Mapped[int]
    kind: Mapped[str]

    exchange_id: Mapped[int] = mapped_column(ForeignKey("exchanges.id"))
    exchange: Mapped[ExchangeTable] = relationship("ExchangeTable")


# The (address, chainId) tuple is unique for each pool manager
Index(
    "ix_pool_manager_address_chain",
    PoolManagerTable.address,
    PoolManagerTable.chain,
    unique=True,
)


class ManagedLiquidityPoolTable(Base):
    __tablename__ = "managed_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_on": "kind",
        "polymorphic_identity": "base",
    }

    id: Mapped[PrimaryKeyInt]
    kind: Mapped[str]

    manager_id: Mapped[ForeignKeyPoolManagerId]
    manager: Mapped[PoolManagerTable] = relationship("PoolManagerTable")


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

    stable: Mapped[bool]


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

    liquidity_update_block: Mapped[int | None]
    liquidity_update_log_index: Mapped[int | None]

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

    pool_hash: Mapped[ManagedPoolHash] = mapped_column(index=True, unique=True)
    hooks: Mapped[Address]
    currency0_id: Mapped[ForeignKeyTokenId]
    currency1_id: Mapped[ForeignKeyTokenId]
    fee_currency0: Mapped[int]
    fee_currency1: Mapped[int]
    fee_denominator: Mapped[int]
    tick_spacing: Mapped[int]

    liquidity_update_block: Mapped[int | None]
    liquidity_update_log_index: Mapped[int | None]

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

    @declared_attr
    @classmethod
    def currency0(cls) -> Mapped[Erc20TokenTable]:
        return relationship(
            "Erc20TokenTable",
            foreign_keys=cls.currency0_id,
        )

    @declared_attr
    @classmethod
    def currency1(cls) -> Mapped[Erc20TokenTable]:
        return relationship(
            "Erc20TokenTable",
            foreign_keys=cls.currency1_id,
        )


class UniswapV4PoolTable(AbstractUniswapV4Pool):
    __tablename__ = "uniswap_v4_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_on": "kind",
        "polymorphic_identity": "uniswap_v4",
    }
