from typing import Annotated

from sqlalchemy import ForeignKey, Index
from sqlalchemy.orm import Mapped, mapped_column

from .base import Address, Base, BigInteger
from .types import ForeignKeyTokenId, PrimaryKeyInt


class AaveV3MarketTable(Base):
    __tablename__ = "aave_v3_markets"

    id: Mapped[PrimaryKeyInt]
    chain_id: Mapped[int]
    name: Mapped[str]
    active: Mapped[bool]
    last_update_block: Mapped[int | None]


ForeignKeyAaveMarketId = Annotated[
    int,
    mapped_column(ForeignKey(AaveV3MarketTable.id), index=True),
]


class AaveV3ContractsTable(Base):
    __tablename__ = "aave_v3_contracts"

    id: Mapped[PrimaryKeyInt]
    market_id: Mapped[ForeignKeyAaveMarketId]

    name: Mapped[str]
    address: Mapped[Address]
    revision: Mapped[int | None]


class AaveV3UsersTable(Base):
    __tablename__ = "aave_v3_users"

    id: Mapped[PrimaryKeyInt]
    market_id: Mapped[ForeignKeyAaveMarketId]

    address: Mapped[Address]
    e_mode: Mapped[int]
    gho_discount: Mapped[int]


ForeignKeyAaveUserId = Annotated[
    int,
    mapped_column(ForeignKey(AaveV3UsersTable.id), index=True),
]


Index(
    "ix_aave_users_address_market",
    AaveV3UsersTable.address,
    AaveV3UsersTable.market_id,
    unique=True,
)


class AaveV3AssetsTable(Base):
    __tablename__ = "aave_v3_assets"

    id: Mapped[PrimaryKeyInt]
    market_id: Mapped[ForeignKeyAaveMarketId]
    underlying_asset_id: Mapped[ForeignKeyTokenId]
    a_token_id: Mapped[ForeignKeyTokenId]
    a_token_revision: Mapped[int]
    v_token_id: Mapped[ForeignKeyTokenId]
    v_token_revision: Mapped[int]

    last_update_block: Mapped[int | None]
    liquidity_index: Mapped[BigInteger]
    liquidity_rate: Mapped[BigInteger]
    borrow_index: Mapped[BigInteger]
    borrow_rate: Mapped[BigInteger]


Index(
    "ix_aave_assets_underlying_asset_market",
    AaveV3AssetsTable.underlying_asset_id,
    AaveV3AssetsTable.market_id,
    unique=True,
)

ForeignKeyAaveAssetId = Annotated[
    int,
    mapped_column(ForeignKey(AaveV3AssetsTable.id), index=True),
]


class AaveV3CollateralPositionsTable(Base):
    __tablename__ = "aave_v3_collateral_positions"

    id: Mapped[PrimaryKeyInt]
    user_id: Mapped[ForeignKeyAaveUserId]
    asset_id: Mapped[ForeignKeyAaveAssetId]

    balance: Mapped[BigInteger]
    last_index: Mapped[BigInteger | None]


Index(
    "ix_aave_collateral_position_user_asset",
    AaveV3CollateralPositionsTable.user_id,
    AaveV3CollateralPositionsTable.asset_id,
    unique=True,
)


class AaveV3DebtPositionsTable(Base):
    __tablename__ = "aave_v3_debt_positions"

    id: Mapped[PrimaryKeyInt]
    user_id: Mapped[ForeignKeyAaveUserId]
    asset_id: Mapped[ForeignKeyAaveAssetId]

    balance: Mapped[BigInteger]
    last_index: Mapped[BigInteger | None]


Index(
    "ix_aave_debt_position_user_asset",
    AaveV3DebtPositionsTable.user_id,
    AaveV3DebtPositionsTable.asset_id,
    unique=True,
)


class AaveGhoTokenTable(Base):
    __tablename__ = "aave_gho_tokens"

    id: Mapped[PrimaryKeyInt]
    token_id: Mapped[ForeignKeyTokenId]

    v_gho_discount_rate_strategy: Mapped[Address | None]
    v_gho_discount_token: Mapped[Address | None]
