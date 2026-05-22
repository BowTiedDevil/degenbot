"""SQLAlchemy model for pool state and deployment data."""
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
    """LiquidityPositionTable class."""

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
    """InitializationMapTable class."""

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


class LiquidityPoolTable(Base):
    """LiquidityPoolTable class."""

    __tablename__ = "pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_on": "kind",
        "polymorphic_identity": "base",
    }

    id: Mapped[PrimaryKeyInt]
    address: Mapped[Address]
    chain: Mapped[int]
    kind: Mapped[str]

    token0_id: Mapped[int] = mapped_column(ForeignKey("erc20_tokens.id"), index=True)
    token1_id: Mapped[int] = mapped_column(ForeignKey("erc20_tokens.id"), index=True)

    token0: Mapped[Erc20TokenTable] = relationship("Erc20TokenTable", foreign_keys=token0_id)
    token1: Mapped[Erc20TokenTable] = relationship("Erc20TokenTable", foreign_keys=token1_id)

    exchange: Mapped[ExchangeTable] = relationship("ExchangeTable")
    exchange_id: Mapped[int] = mapped_column(ForeignKey("exchanges.id"))


# The (address, chainId) tuple is unique for each liquidity pool
Index(
    "ix_liquidity_pool_address_chain",
    LiquidityPoolTable.address,
    LiquidityPoolTable.chain,
    unique=True,
)

Index(
    "ix_liquidity_pools_token_ids",
    LiquidityPoolTable.token0_id,
    LiquidityPoolTable.token1_id,
)


class UniswapFeeMixin:
    """A mixin class defining common columns for Uniswap V2 & V3 pools and variants."""

    fee_token0: Mapped[int]
    fee_token1: Mapped[int]
    fee_denominator: Mapped[int]


class UniswapV2PoolTableBase(LiquidityPoolTable, UniswapFeeMixin):
    """
    Abstract parent for all Uniswap V2 variants.

    It may be used to identify concrete subclasses at runtime, but otherwise is not useful for
    performing database queries.
    """

    __abstract__ = True


class AerodromeV2PoolTable(UniswapV2PoolTableBase):
    """AerodromeV2PoolTable class."""

    __tablename__ = "aerodrome_v2_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "aerodrome_v2",
    }

    stable: Mapped[bool]
    pool_id: Mapped[PrimaryForeignKeyPoolId]


class CamelotV2PoolTable(UniswapV2PoolTableBase):
    """CamelotV2PoolTable class."""

    __tablename__ = "camelot_v2_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "camelot_v2",
    }

    pool_id: Mapped[PrimaryForeignKeyPoolId]


class PancakeswapV2PoolTable(UniswapV2PoolTableBase):
    """PancakeswapV2PoolTable class."""

    __tablename__ = "pancakeswap_v2_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "pancakeswap_v2",
    }

    pool_id: Mapped[PrimaryForeignKeyPoolId]


class SushiswapV2PoolTable(UniswapV2PoolTableBase):
    """SushiswapV2PoolTable class."""

    __tablename__ = "sushiswap_v2_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "sushiswap_v2",
    }

    pool_id: Mapped[PrimaryForeignKeyPoolId]


class SwapbasedV2PoolTable(UniswapV2PoolTableBase):
    """SwapbasedV2PoolTable class."""

    __tablename__ = "swapbased_v2_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "swapbased_v2",
    }

    pool_id: Mapped[PrimaryForeignKeyPoolId]


class UniswapV2PoolTable(UniswapV2PoolTableBase):
    """UniswapV2PoolTable class."""

    __tablename__ = "uniswap_v2_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "uniswap_v2",
    }

    pool_id: Mapped[PrimaryForeignKeyPoolId]


class UniswapV3PoolTableBase(LiquidityPoolTable, UniswapFeeMixin):
    """
    Abstract parent for all Uniswap V3 variants.

    It may be used to identify concrete subclasses at runtime.
    """

    __abstract__ = True

    tick_spacing: Mapped[int]

    liquidity_update_block: Mapped[int | None]
    liquidity_update_log_index: Mapped[int | None]

    @declared_attr
    @classmethod
    def liquidity_positions(cls) -> Mapped[list[LiquidityPositionTable]]:
        """Liquidity positions."""
        return relationship(
            "LiquidityPositionTable",
            cascade="all, delete",
        )

    @declared_attr
    @classmethod
    def initialization_maps(cls) -> Mapped[list[InitializationMapTable]]:
        """Map initialization records."""
        return relationship(
            "InitializationMapTable",
            cascade="all, delete",
        )


class AerodromeV3PoolTable(UniswapV3PoolTableBase):
    """AerodromeV3PoolTable class."""

    __tablename__ = "aerodrome_v3_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "aerodrome_v3",
    }

    pool_id: Mapped[PrimaryForeignKeyPoolId]


class UniswapV3PoolTable(UniswapV3PoolTableBase):
    """UniswapV3PoolTable class."""

    __tablename__ = "uniswap_v3_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "uniswap_v3",
    }

    pool_id: Mapped[PrimaryForeignKeyPoolId]


class PancakeswapV3PoolTable(UniswapV3PoolTableBase):
    """PancakeswapV3PoolTable class."""

    __tablename__ = "pancakeswap_v3_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "pancakeswap_v3",
    }

    pool_id: Mapped[PrimaryForeignKeyPoolId]


class SushiswapV3PoolTable(UniswapV3PoolTableBase):
    """SushiswapV3PoolTable class."""

    __tablename__ = "sushiswap_v3_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_identity": "sushiswap_v3",
    }

    pool_id: Mapped[PrimaryForeignKeyPoolId]


class ManagedPoolLiquidityPositionTable(Base):
    """ManagedPoolLiquidityPositionTable class."""

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
    """ManagedPoolInitializationMapTable class."""

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
    """PoolManagerTable class."""

    __tablename__ = "pool_managers"

    id: Mapped[PrimaryKeyInt]
    address: Mapped[Address]
    chain: Mapped[int]
    kind: Mapped[str]

    state_view: Mapped[Address | None]

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
    """ManagedLiquidityPoolTable class."""

    __tablename__ = "managed_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_on": "kind",
        "polymorphic_identity": "base",
    }

    id: Mapped[PrimaryKeyInt]
    kind: Mapped[str]

    manager_id: Mapped[ForeignKeyPoolManagerId]
    manager: Mapped[PoolManagerTable] = relationship("PoolManagerTable")


class UniswapV4PoolTableBase(ManagedLiquidityPoolTable):
    """
    Abstract parent for all Uniswap V4 variants.

    It should not be instantiated directly, but may be used to query and select child classes.
    """

    __abstract__ = True

    managed_pool_id: Mapped[PrimaryForeignKeyManagedPoolId]

    pool_hash: Mapped[ManagedPoolHash] = mapped_column(index=True)
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
        """Liquidity positions."""
        return relationship(
            "ManagedPoolLiquidityPositionTable",
            cascade="all, delete",
        )

    @declared_attr
    @classmethod
    def initialization_maps(cls) -> Mapped[list[ManagedPoolInitializationMapTable]]:
        """Map initialization records."""
        return relationship(
            "ManagedPoolInitializationMapTable",
            cascade="all, delete",
        )

    @declared_attr
    @classmethod
    def currency0(cls) -> Mapped[Erc20TokenTable]:
        """Currency0."""
        return relationship(
            "Erc20TokenTable",
            foreign_keys=cls.currency0_id,
        )

    @declared_attr
    @classmethod
    def currency1(cls) -> Mapped[Erc20TokenTable]:
        """Currency1."""
        return relationship(
            "Erc20TokenTable",
            foreign_keys=cls.currency1_id,
        )


class UniswapV4PoolTable(UniswapV4PoolTableBase):
    """UniswapV4PoolTable class."""

    __tablename__ = "uniswap_v4_pools"
    __mapper_args__ = {  # noqa: RUF012
        "polymorphic_on": "kind",
        "polymorphic_identity": "uniswap_v4",
    }


Index(
    "ix_uniswap_v4_pools_token_ids",
    UniswapV4PoolTable.currency0_id,
    UniswapV4PoolTable.currency1_id,
)
