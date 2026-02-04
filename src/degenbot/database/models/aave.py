from typing import TYPE_CHECKING, Annotated

from sqlalchemy import ForeignKey, Index
from sqlalchemy.orm import Mapped, mapped_column, relationship

from .base import Address, Base, BigInteger
from .types import ForeignKeyTokenId, PrimaryKeyInt

if TYPE_CHECKING:
    from .erc20 import Erc20TokenTable


class AaveV3MarketTable(Base):
    __tablename__ = "aave_v3_markets"

    id: Mapped[PrimaryKeyInt]
    chain_id: Mapped[int]
    name: Mapped[str]
    active: Mapped[bool]
    last_update_block: Mapped[int | None]

    # Relationships
    contracts: Mapped[list["AaveV3ContractsTable"]] = relationship(
        "AaveV3ContractsTable",
        back_populates="market",
    )
    users: Mapped[list["AaveV3UsersTable"]] = relationship(
        "AaveV3UsersTable",
        back_populates="market",
    )
    assets: Mapped[list["AaveV3AssetsTable"]] = relationship(
        "AaveV3AssetsTable",
        back_populates="market",
    )


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

    # Relationships
    market: Mapped["AaveV3MarketTable"] = relationship(
        "AaveV3MarketTable",
        back_populates="contracts",
    )


class AaveV3UsersTable(Base):
    __tablename__ = "aave_v3_users"

    id: Mapped[PrimaryKeyInt]
    market_id: Mapped[ForeignKeyAaveMarketId]

    address: Mapped[Address]
    e_mode: Mapped[int]
    gho_discount: Mapped[int]
    stk_aave_balance: Mapped[int | None]

    # Relationships
    market: Mapped["AaveV3MarketTable"] = relationship(
        "AaveV3MarketTable",
        back_populates="users",
    )
    collateral_positions: Mapped[list["AaveV3CollateralPositionsTable"]] = relationship(
        "AaveV3CollateralPositionsTable",
        back_populates="user",
    )
    debt_positions: Mapped[list["AaveV3DebtPositionsTable"]] = relationship(
        "AaveV3DebtPositionsTable",
        back_populates="user",
    )


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

    # Relationships
    market: Mapped["AaveV3MarketTable"] = relationship(
        "AaveV3MarketTable",
        back_populates="assets",
    )
    underlying_token: Mapped["Erc20TokenTable"] = relationship(
        "Erc20TokenTable",
        foreign_keys="AaveV3AssetsTable.underlying_asset_id",
    )
    a_token: Mapped["Erc20TokenTable"] = relationship(
        "Erc20TokenTable",
        foreign_keys="AaveV3AssetsTable.a_token_id",
    )
    v_token: Mapped["Erc20TokenTable"] = relationship(
        "Erc20TokenTable",
        foreign_keys="AaveV3AssetsTable.v_token_id",
    )
    collateral_positions: Mapped[list["AaveV3CollateralPositionsTable"]] = relationship(
        "AaveV3CollateralPositionsTable",
        back_populates="asset",
    )
    debt_positions: Mapped[list["AaveV3DebtPositionsTable"]] = relationship(
        "AaveV3DebtPositionsTable",
        back_populates="asset",
    )


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

    # Relationships
    user: Mapped["AaveV3UsersTable"] = relationship(
        "AaveV3UsersTable",
        foreign_keys="AaveV3CollateralPositionsTable.user_id",
        back_populates="collateral_positions",
    )
    asset: Mapped["AaveV3AssetsTable"] = relationship(
        "AaveV3AssetsTable",
        foreign_keys="AaveV3CollateralPositionsTable.asset_id",
        back_populates="collateral_positions",
    )


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

    # Relationships
    user: Mapped["AaveV3UsersTable"] = relationship(
        "AaveV3UsersTable",
        foreign_keys="AaveV3DebtPositionsTable.user_id",
        back_populates="debt_positions",
    )
    asset: Mapped["AaveV3AssetsTable"] = relationship(
        "AaveV3AssetsTable",
        foreign_keys="AaveV3DebtPositionsTable.asset_id",
        back_populates="debt_positions",
    )


Index(
    "ix_aave_debt_position_user_asset",
    AaveV3DebtPositionsTable.user_id,
    AaveV3DebtPositionsTable.asset_id,
    unique=True,
)


class AaveGhoTokenTable(Base):
    """
    GHO token attributes for Aave V3 markets.

    GHO tokens are chain-unique: multiple markets on the same chain share the same GHO token.
    This table stores global GHO configuration (discount token, discount rate strategy) that
    applies to all markets on a chain.

    Design rationale: Aave's GHO stablecoin is deployed once per chain. Multiple markets
    on the same chain (e.g., different Aave V3 instances) reference the same GHO variable
    debt token address. The GHO discount mechanism (stkAAVE on Ethereum) is also shared
    across markets on the same chain.
    """

    __tablename__ = "aave_gho_tokens"

    id: Mapped[PrimaryKeyInt]
    token_id: Mapped[ForeignKeyTokenId]

    v_gho_discount_rate_strategy: Mapped[Address | None]
    v_gho_discount_token: Mapped[Address | None]

    # Relationships
    token: Mapped["Erc20TokenTable"] = relationship(
        "Erc20TokenTable",
        foreign_keys="AaveGhoTokenTable.token_id",
    )
