from typing import TYPE_CHECKING, Annotated

from sqlalchemy import ForeignKey, Index
from sqlalchemy.orm import Mapped, mapped_column, relationship

from .base import Address, Base, BigInteger
from .types import ForeignKeyTokenId, PrimaryKeyInt

if TYPE_CHECKING:
    from .erc20 import Erc20TokenTable


class AaveV3Market(Base):
    __tablename__ = "aave_v3_markets"

    id: Mapped[PrimaryKeyInt]
    chain_id: Mapped[int]
    name: Mapped[str]
    active: Mapped[bool]
    last_update_block: Mapped[int | None]

    # Relationships
    contracts: Mapped[list["AaveV3Contract"]] = relationship(
        "AaveV3Contract",
        back_populates="market",
        cascade="all",
    )
    users: Mapped[list["AaveV3User"]] = relationship(
        "AaveV3User",
        back_populates="market",
        cascade="all",
    )
    assets: Mapped[list["AaveV3Asset"]] = relationship(
        "AaveV3Asset",
        back_populates="market",
        cascade="all",
    )

    def __repr__(self) -> str:
        return (
            f"{self.__class__.__name__}("
            f"chain_id={self.chain_id!r}, "
            f"name={self.name!r}, "
            f"active={self.active!r}"
            f")"
        )


ForeignKeyAaveMarketId = Annotated[
    int,
    mapped_column(ForeignKey(AaveV3Market.id), index=True),
]


class AaveV3Contract(Base):
    __tablename__ = "aave_v3_contracts"

    id: Mapped[PrimaryKeyInt]
    market_id: Mapped[ForeignKeyAaveMarketId]

    name: Mapped[str]
    address: Mapped[Address]
    revision: Mapped[int | None]

    # Relationships
    market: Mapped["AaveV3Market"] = relationship(
        "AaveV3Market",
        back_populates="contracts",
    )


class AaveV3User(Base):
    __tablename__ = "aave_v3_users"

    id: Mapped[PrimaryKeyInt]
    market_id: Mapped[ForeignKeyAaveMarketId]

    address: Mapped[Address]
    e_mode: Mapped[int]
    gho_discount: Mapped[int]
    stk_aave_balance: Mapped[BigInteger | None]

    # Relationships
    market: Mapped["AaveV3Market"] = relationship(
        "AaveV3Market",
        back_populates="users",
    )
    collateral_positions: Mapped[list["AaveV3CollateralPosition"]] = relationship(
        "AaveV3CollateralPosition",
        back_populates="user",
        cascade="all",
    )
    debt_positions: Mapped[list["AaveV3DebtPosition"]] = relationship(
        "AaveV3DebtPosition",
        back_populates="user",
        cascade="all",
    )

    def __repr__(self) -> str:
        return (
            f"{self.__class__.__name__}("
            f"market={self.market!r}, "
            f"address={self.address!r}, "
            f"e_mode={self.e_mode!r}"
            f")"
        )


ForeignKeyAaveUserId = Annotated[
    int,
    mapped_column(ForeignKey(AaveV3User.id), index=True),
]


Index(
    "ix_aave_users_address_market",
    AaveV3User.address,
    AaveV3User.market_id,
    unique=True,
)


class AaveV3Asset(Base):
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
    market: Mapped["AaveV3Market"] = relationship(
        "AaveV3Market",
        back_populates="assets",
    )
    underlying_token: Mapped["Erc20TokenTable"] = relationship(
        "Erc20TokenTable",
        foreign_keys="AaveV3Asset.underlying_asset_id",
    )
    a_token: Mapped["Erc20TokenTable"] = relationship(
        "Erc20TokenTable",
        foreign_keys="AaveV3Asset.a_token_id",
    )
    v_token: Mapped["Erc20TokenTable"] = relationship(
        "Erc20TokenTable",
        foreign_keys="AaveV3Asset.v_token_id",
    )
    collateral_positions: Mapped[list["AaveV3CollateralPosition"]] = relationship(
        "AaveV3CollateralPosition",
        back_populates="asset",
        cascade="all",
    )
    debt_positions: Mapped[list["AaveV3DebtPosition"]] = relationship(
        "AaveV3DebtPosition",
        back_populates="asset",
        cascade="all",
    )

    def __repr__(self) -> str:
        return (
            f"{self.__class__.__name__}("
            f"market={self.market!r}, "
            f"underlying_token={self.underlying_token!r}, "
            f"a_token={self.a_token!r}, "
            f"v_token={self.v_token!r}"
            f")"
        )


Index(
    "ix_aave_assets_underlying_asset_market",
    AaveV3Asset.underlying_asset_id,
    AaveV3Asset.market_id,
    unique=True,
)

ForeignKeyAaveAssetId = Annotated[
    int,
    mapped_column(ForeignKey(AaveV3Asset.id), index=True),
]


class AaveV3CollateralPosition(Base):
    __tablename__ = "aave_v3_collateral_positions"

    id: Mapped[PrimaryKeyInt]
    user_id: Mapped[ForeignKeyAaveUserId]
    asset_id: Mapped[ForeignKeyAaveAssetId]

    balance: Mapped[BigInteger]
    last_index: Mapped[BigInteger | None]

    # Relationships
    user: Mapped["AaveV3User"] = relationship(
        "AaveV3User",
        foreign_keys="AaveV3CollateralPosition.user_id",
        back_populates="collateral_positions",
    )
    asset: Mapped["AaveV3Asset"] = relationship(
        "AaveV3Asset",
        foreign_keys="AaveV3CollateralPosition.asset_id",
        back_populates="collateral_positions",
    )

    def __repr__(self) -> str:
        return (
            f"{self.__class__.__name__}("
            f"user={self.user!r}, "
            f"asset={self.asset!r}, "
            f"balance={self.balance!r}"
            f")"
        )


Index(
    "ix_aave_collateral_position_user_asset",
    AaveV3CollateralPosition.user_id,
    AaveV3CollateralPosition.asset_id,
    unique=True,
)


class AaveV3DebtPosition(Base):
    __tablename__ = "aave_v3_debt_positions"

    id: Mapped[PrimaryKeyInt]
    user_id: Mapped[ForeignKeyAaveUserId]
    asset_id: Mapped[ForeignKeyAaveAssetId]

    balance: Mapped[BigInteger]
    last_index: Mapped[BigInteger | None]

    # Relationships
    user: Mapped["AaveV3User"] = relationship(
        "AaveV3User",
        foreign_keys="AaveV3DebtPosition.user_id",
        back_populates="debt_positions",
    )
    asset: Mapped["AaveV3Asset"] = relationship(
        "AaveV3Asset",
        foreign_keys="AaveV3DebtPosition.asset_id",
        back_populates="debt_positions",
    )

    def __repr__(self) -> str:
        return (
            f"{self.__class__.__name__}("
            f"user={self.user!r}, "
            f"asset={self.asset!r}, "
            f"balance={self.balance!r}"
            f")"
        )


Index(
    "ix_aave_debt_position_user_asset",
    AaveV3DebtPosition.user_id,
    AaveV3DebtPosition.asset_id,
    unique=True,
)


class AaveGhoToken(Base):
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

    v_token_id: Mapped[ForeignKeyTokenId | None]
    v_gho_discount_rate_strategy: Mapped[Address | None]
    v_gho_discount_token: Mapped[Address | None]

    # Relationships
    token: Mapped["Erc20TokenTable"] = relationship(
        "Erc20TokenTable",
        foreign_keys="AaveGhoToken.token_id",
    )
    v_token: Mapped["Erc20TokenTable | None"] = relationship(
        "Erc20TokenTable",
        foreign_keys="AaveGhoToken.v_token_id",
    )
