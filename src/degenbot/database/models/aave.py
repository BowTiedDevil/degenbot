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
    e_mode_categories: Mapped[list["AaveV3EModeCategory"]] = relationship(
        "AaveV3EModeCategory",
        back_populates="market",
        cascade="all",
    )

    def __repr__(self) -> str:
        return (
            f"{self.__class__.__name__}("
            f"id={self.id}, "
            f"chain_id={self.chain_id!r}, "
            f"name={self.name!r}, "
            f"active={self.active!r}"
            f")"
        )


ForeignKeyAaveMarketId = Annotated[
    int,
    mapped_column(ForeignKey(AaveV3Market.id), index=True),
]


class AaveV3EModeCategory(Base):
    """
    eMode category configuration for correlated assets.

    Users in eMode get better LTV and liquidation terms for assets in the
    same category. Liquidators need this to calculate effective thresholds.
    """

    __tablename__ = "aave_v3_emode_categories"

    id: Mapped[PrimaryKeyInt]
    market_id: Mapped[ForeignKeyAaveMarketId]

    category_id: Mapped[int]
    label: Mapped[str | None]

    # eMode-specific risk parameters (basis points)
    ltv: Mapped[int]
    liquidation_threshold: Mapped[int]
    liquidation_bonus: Mapped[int]
    price_source: Mapped[Address | None]

    # Relationships
    market: Mapped["AaveV3Market"] = relationship(
        "AaveV3Market",
        back_populates="e_mode_categories",
    )
    assets: Mapped[list["AaveV3Asset"]] = relationship(
        "AaveV3Asset",
        back_populates="e_mode_category",
    )

    def __repr__(self) -> str:
        return f"{self.__class__.__name__}(category_id={self.category_id!r}, label={self.label!r})"


Index(
    "ix_aave_emode_category_market_cat",
    AaveV3EModeCategory.market_id,
    AaveV3EModeCategory.category_id,
    unique=True,
)


ForeignKeyAaveEModeCategoryId = Annotated[
    int,
    mapped_column(ForeignKey(AaveV3EModeCategory.id), index=True),
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

    # Isolation mode settings
    isolation_mode_collateral_asset_id: Mapped[int | None] = mapped_column(
        ForeignKey("aave_v3_assets.id"),
        index=True,
        nullable=True,
    )
    isolation_mode_debt: Mapped[BigInteger] = mapped_column(default=0)

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
    # Isolation mode relationships
    isolation_collateral_asset: Mapped["AaveV3Asset | None"] = relationship(
        "AaveV3Asset",
        foreign_keys="AaveV3User.isolation_mode_collateral_asset_id",
        back_populates="isolation_mode_users",
    )
    # User collateral configuration - which assets are enabled as collateral
    collateral_configs: Mapped[list["AaveV3UserCollateralConfig"]] = relationship(
        "AaveV3UserCollateralConfig",
        back_populates="user",
        cascade="all",
    )

    @property
    def is_isolation_mode(self) -> bool:
        """True if user is in isolation mode (has isolated collateral)."""
        return self.isolation_mode_collateral_asset_id is not None

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

    # eMode category assignment (denormalized for query convenience)
    e_mode_category_id: Mapped[ForeignKeyAaveEModeCategoryId | None]

    # Price oracle source for this asset
    price_source: Mapped[Address | None]

    # Protocol state - updated every block
    last_update_block: Mapped[int | None]
    liquidity_index: Mapped[BigInteger]  # Scaled by ray (1e27)
    liquidity_rate: Mapped[BigInteger]
    borrow_index: Mapped[BigInteger]  # Scaled by ray (1e27)
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
    e_mode_category: Mapped["AaveV3EModeCategory | None"] = relationship(
        "AaveV3EModeCategory",
        back_populates="assets",
    )
    asset_config: Mapped["AaveV3AssetConfig"] = relationship(
        "AaveV3AssetConfig",
        back_populates="asset",
        uselist=False,
        lazy="joined",
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
    # Isolation mode users referencing this as their collateral
    isolation_mode_users: Mapped[list["AaveV3User"]] = relationship(
        "AaveV3User",
        foreign_keys="AaveV3User.isolation_mode_collateral_asset_id",
        back_populates="isolation_collateral_asset",
    )
    # User collateral configs for this asset
    collateral_user_configs: Mapped[list["AaveV3UserCollateralConfig"]] = relationship(
        "AaveV3UserCollateralConfig",
        back_populates="asset",
    )

    # Convenience properties for liquidation calculations
    @property
    def liquidation_threshold(self) -> int:
        """Liquidation threshold in basis points (e.g., 8000 = 80%)."""
        return self.asset_config.liquidation_threshold if self.asset_config else 0

    @property
    def ltv(self) -> int:
        """Loan-to-Value ratio in basis points (e.g., 7500 = 75%)."""
        return self.asset_config.ltv if self.asset_config else 0

    @property
    def liquidation_bonus(self) -> int:
        """Liquidation bonus in basis points (e.g., 500 = 5%)."""
        return self.asset_config.liquidation_bonus if self.asset_config else 0

    @property
    def isolation_mode(self) -> bool:
        """True if this asset can be used as isolation mode collateral."""
        return self.asset_config.isolation_mode if self.asset_config else False

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


class AaveV3AssetConfig(Base):
    """
    Asset configuration for liquidation monitoring.

    Stores durable configuration values that affect liquidation calculations:
    LTV, liquidation threshold, liquidation bonus, and feature flags.
    Updated only when governance changes asset parameters.
    """

    __tablename__ = "aave_v3_asset_configs"

    id: Mapped[PrimaryKeyInt]
    asset_id: Mapped[ForeignKeyAaveAssetId]

    # Risk parameters (in basis points: 10000 = 100%)
    ltv: Mapped[int]
    liquidation_threshold: Mapped[int]
    liquidation_bonus: Mapped[int]
    e_mode_category_id: Mapped[int | None]

    # Feature flags
    borrowing_enabled: Mapped[bool]
    stable_borrowing_enabled: Mapped[bool]
    flash_loan_enabled: Mapped[bool]

    # Isolation mode settings (relevant for liquidations)
    isolation_mode: Mapped[bool]
    borrowable_in_isolation: Mapped[bool]
    debt_ceiling: Mapped[BigInteger | None]

    # Relationships
    asset: Mapped["AaveV3Asset"] = relationship(
        "AaveV3Asset",
        back_populates="asset_config",
    )

    def __repr__(self) -> str:
        return (
            f"{self.__class__.__name__}("
            f"asset={self.asset!r}, "
            f"ltv={self.ltv!r}, "
            f"liquidation_threshold={self.liquidation_threshold!r}"
            f")"
        )


Index(
    "ix_aave_asset_config_asset",
    AaveV3AssetConfig.asset_id,
    unique=True,
)


class AaveV3CollateralPosition(Base):
    __tablename__ = "aave_v3_collateral_positions"

    id: Mapped[PrimaryKeyInt]
    user_id: Mapped[ForeignKeyAaveUserId]
    asset_id: Mapped[ForeignKeyAaveAssetId]

    balance: Mapped[BigInteger]  # Scaled balance
    last_index: Mapped[BigInteger | None]  # Last liquidity index observed

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

    balance: Mapped[BigInteger]  # Scaled balance
    last_index: Mapped[BigInteger | None]  # Last borrow index observed

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


class AaveV3UserCollateralConfig(Base):
    """
    Tracks which assets each user has enabled as collateral.

    A user can hold aTokens for an asset but choose not to use it as collateral.
    This table tracks that preference state, updated by
    ReserveUsedAsCollateralEnabled/Disabled events.
    """

    __tablename__ = "aave_v3_user_collateral_configs"

    id: Mapped[PrimaryKeyInt]
    user_id: Mapped[ForeignKeyAaveUserId]
    asset_id: Mapped[ForeignKeyAaveAssetId]

    enabled: Mapped[bool]

    # Relationships
    user: Mapped["AaveV3User"] = relationship(
        "AaveV3User",
        back_populates="collateral_configs",
    )
    asset: Mapped["AaveV3Asset"] = relationship(
        "AaveV3Asset",
        back_populates="collateral_user_configs",
    )

    def __repr__(self) -> str:
        return (
            f"{self.__class__.__name__}("
            f"user={self.user!r}, "
            f"asset={self.asset!r}, "
            f"enabled={self.enabled!r}"
            f")"
        )


Index(
    "ix_aave_user_collateral_config_user_asset",
    AaveV3UserCollateralConfig.user_id,
    AaveV3UserCollateralConfig.asset_id,
    unique=True,
)

Index(
    "ix_aave_user_collateral_config_enabled",
    AaveV3UserCollateralConfig.enabled,
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
