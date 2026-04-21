"""Event processing handlers for Aave V3 configuration and contract updates."""

from typing import assert_never

import eth_abi.abi
from eth_typing import ChecksumAddress
from sqlalchemy import select
from sqlalchemy.orm import Session
from web3.types import LogReceipt

from degenbot.checksum_cache import get_checksum_address
from degenbot.cli.aave.constants import GHO_DISCOUNT_DEPRECATION_REVISION
from degenbot.cli.aave.db_assets import (
    get_asset_by_token_type,
    get_contract,
    get_gho_asset,
    get_or_create_erc20_token,
)
from degenbot.cli.aave.db_users import get_or_create_user
from degenbot.cli.aave.types import TokenType, TransactionContext
from degenbot.cli.aave_utils import decode_address
from degenbot.constants import ERC_1967_IMPLEMENTATION_SLOT, ZERO_ADDRESS
from degenbot.database.models import Erc20TokenTable
from degenbot.database.models.aave import (
    AaveGhoToken,
    AaveV3Asset,
    AaveV3AssetConfig,
    AaveV3Contract,
    AaveV3EModeCategory,
    AaveV3Market,
    AaveV3User,
    AaveV3UserCollateralConfig,
)
from degenbot.functions import encode_function_calldata, raw_call
from degenbot.logging import logger
from degenbot.provider.interface import ProviderAdapter


def _process_collateral_configuration_changed_event(
    *,
    provider: ProviderAdapter,
    session: Session,
    event: LogReceipt,
    market: AaveV3Market,
) -> AaveV3AssetConfig | None:
    """
    Process a CollateralConfigurationChanged event to update asset configuration.

    Fetches full configuration from the Pool contract via getConfiguration()
    and decodes the bitmap to populate all config fields.

    Reference:
    ```
    event CollateralConfigurationChanged(
        address indexed asset,
        uint256 ltv,
        uint256 liquidationThreshold,
        uint256 liquidationBonus
    );
    ```
    """

    asset_address = decode_address(event["topics"][1])

    # Find the asset in the database
    asset = session.scalar(
        select(AaveV3Asset).where(
            AaveV3Asset.underlying_token.has(address=get_checksum_address(asset_address))
        )
    )
    assert asset is not None

    # Get the Pool contract address
    pool_contract = get_contract(
        session=session,
        market=market,
        contract_name="POOL",
    )
    assert pool_contract is not None

    # Fetch full configuration from Pool contract
    (config_bitmap,) = raw_call(
        w3=provider,
        address=pool_contract.address,
        calldata=encode_function_calldata(
            function_prototype="getConfiguration(address)",
            function_arguments=[asset_address],
        ),
        return_types=["uint256"],
        block_identifier=event["blockNumber"],
    )

    # Decode the configuration bitmap
    decoded = _decode_reserve_configuration_bitmap(config_bitmap)

    # Get or create the asset config
    config = session.scalar(select(AaveV3AssetConfig).where(AaveV3AssetConfig.asset_id == asset.id))

    if config is None:
        config = AaveV3AssetConfig(
            asset_id=asset.id,
            ltv=decoded["ltv"],
            liquidation_threshold=decoded["liquidation_threshold"],
            liquidation_bonus=decoded["liquidation_bonus"],
            borrowing_enabled=decoded["borrowing_enabled"],
            stable_borrowing_enabled=False,
            flash_loan_enabled=decoded["flash_loan_enabled"],
            borrowable_in_isolation=decoded["borrowable_in_isolation"],
            isolation_mode=decoded["isolation_mode"],
            debt_ceiling=decoded["debt_ceiling"],
            e_mode_category_id=decoded["e_mode_category_id"],
        )
        session.add(config)
        logger.info(f"Created AaveV3AssetConfig for {asset_address}")
    else:
        config.ltv = decoded["ltv"]
        config.liquidation_threshold = decoded["liquidation_threshold"]
        config.liquidation_bonus = decoded["liquidation_bonus"]
        config.borrowing_enabled = decoded["borrowing_enabled"]
        config.flash_loan_enabled = decoded["flash_loan_enabled"]
        config.borrowable_in_isolation = decoded["borrowable_in_isolation"]
        config.isolation_mode = decoded["isolation_mode"]
        config.debt_ceiling = decoded["debt_ceiling"]
        config.e_mode_category_id = decoded["e_mode_category_id"]
        logger.info(f"Updated AaveV3AssetConfig for {asset_address}")

    return config


def _decode_reserve_configuration_bitmap(config_bitmap: int) -> dict:
    """Decode Aave reserve configuration bitmap into human-readable values."""
    # LTV: bits 0-15
    ltv = config_bitmap & 0xFFFF

    # Liquidation threshold: bits 16-31
    liquidation_threshold = (config_bitmap >> 16) & 0xFFFF

    # Liquidation bonus: bits 32-47
    liquidation_bonus = (config_bitmap >> 32) & 0xFFFF

    # Decimals: bits 48-55
    decimals = (config_bitmap >> 48) & 0xFF

    # Active flag: bit 56
    is_active = bool((config_bitmap >> 56) & 1)

    # Frozen flag: bit 57
    is_frozen = bool((config_bitmap >> 57) & 1)

    # Borrowing enabled flag: bit 58
    borrowing_enabled = bool((config_bitmap >> 58) & 1)

    # Stable rate borrowing enabled: bit 59
    stable_rate_borrowing_enabled = bool((config_bitmap >> 59) & 1)

    # Reserve factor: bits 64-79
    reserve_factor = (config_bitmap >> 64) & 0xFFFF

    # Borrow cap: bits 80-115
    borrow_cap = (config_bitmap >> 80) & 0xFFFFFFFF

    # Supply cap: bits 116-151
    supply_cap = (config_bitmap >> 116) & 0xFFFFFFFF

    # Debt ceiling (isolation mode): bits 212-251
    debt_ceiling = (config_bitmap >> 212) & 0xFFFFFFFFFF

    # Liquidation protocol fee: bits 152-167
    liquidation_protocol_fee = (config_bitmap >> 152) & 0xFFFF

    # Unbacked mint cap: bits 168-203
    unbacked_mint_cap = (config_bitmap >> 168) & 0xFFFFFFFF

    # E-mode category: bits 168-175 (overlap, depends on version)
    e_mode_category = (config_bitmap >> 168) & 0xFF

    # Flash loan enabled: bit 63
    flash_loan_enabled = bool((config_bitmap >> 63) & 1)

    # Isolation mode: bit 62
    isolation_mode = bool((config_bitmap >> 62) & 1)

    # Borrowable in isolation: bit 61
    borrowable_in_isolation = bool((config_bitmap >> 61) & 1)

    return {
        "ltv": ltv,
        "liquidation_threshold": liquidation_threshold,
        "liquidation_bonus": liquidation_bonus,
        "decimals": decimals,
        "is_active": is_active,
        "is_frozen": is_frozen,
        "borrowing_enabled": borrowing_enabled,
        "stable_rate_borrowing_enabled": stable_rate_borrowing_enabled,
        "reserve_factor": reserve_factor,
        "borrow_cap": borrow_cap,
        "supply_cap": supply_cap,
        "debt_ceiling": debt_ceiling,
        "liquidation_protocol_fee": liquidation_protocol_fee,
        "unbacked_mint_cap": unbacked_mint_cap,
        "e_mode_category_id": e_mode_category if e_mode_category > 0 else None,
        "flash_loan_enabled": flash_loan_enabled,
        "isolation_mode": isolation_mode,
        "borrowable_in_isolation": borrowable_in_isolation,
    }


def _process_e_mode_category_added_event(
    *,
    session: Session,
    event: LogReceipt,
    market_id: int,
) -> AaveV3EModeCategory | None:
    """
    Process an EModeCategoryAdded event to update eMode category configuration.

    Reference:
    ```
    event EModeCategoryAdded(
        uint8 indexed categoryId,
        uint256 ltv,
        uint256 liquidationThreshold,
        uint256 liquidationBonus,
        address oracle,
        string label
    );
    ```
    """

    category_id = int.from_bytes(event["topics"][1], "big")

    # Decode non-indexed parameters
    ltv, liquidation_threshold, liquidation_bonus, oracle, label = eth_abi.abi.decode(
        types=["uint256", "uint256", "uint256", "address", "string"],
        data=event["data"],
    )

    # Check if category already exists
    category = session.scalar(
        select(AaveV3EModeCategory).where(
            AaveV3EModeCategory.market_id == market_id,
            AaveV3EModeCategory.category_id == category_id,
        )
    )

    if category is None:
        category = AaveV3EModeCategory(
            market_id=market_id,
            category_id=category_id,
            label=label,
            ltv=int(ltv),
            liquidation_threshold=int(liquidation_threshold),
            liquidation_bonus=int(liquidation_bonus),
            price_source=get_checksum_address(oracle) if oracle else None,
        )
        session.add(category)
        logger.info(f"Created AaveV3EModeCategory: {label} (ID: {category_id})")
    else:
        category.ltv = int(ltv)
        category.liquidation_threshold = int(liquidation_threshold)
        category.liquidation_bonus = int(liquidation_bonus)
        category.price_source = get_checksum_address(oracle) if oracle else None
        category.label = label
        logger.info(f"Updated AaveV3EModeCategory: {label} (ID: {category_id})")

    return category


def _process_emode_asset_category_changed_event(
    *,
    session: Session,
    event: LogReceipt,
    market_id: int,
) -> AaveV3AssetConfig | None:
    """
    Process an EModeAssetCategoryChanged event (older Aave versions).

    Updates the asset's eMode category assignment.

    Reference:
    ```
    event EModeAssetCategoryChanged(
        address indexed asset,
        uint8 oldCategoryId,
        uint8 newCategoryId
    );
    ```
    """
    asset_address = decode_address(event["topics"][1])

    # Decode non-indexed parameters
    old_category_id, new_category_id = eth_abi.abi.decode(
        types=["uint8", "uint8"],
        data=event["data"],
    )

    # Find the asset
    asset = session.scalar(
        select(AaveV3Asset).where(
            AaveV3Asset.market_id == market_id,
            AaveV3Asset.underlying_token.has(address=get_checksum_address(asset_address)),
        )
    )
    assert asset is not None

    # Get or create the asset config
    config = session.scalar(select(AaveV3AssetConfig).where(AaveV3AssetConfig.asset_id == asset.id))

    if config is None:
        config = AaveV3AssetConfig(
            asset_id=asset.id,
            ltv=0,
            liquidation_threshold=0,
            liquidation_bonus=0,
            borrowing_enabled=False,
            stable_borrowing_enabled=False,
            flash_loan_enabled=False,
            borrowable_in_isolation=False,
            isolation_mode=False,
            debt_ceiling=None,
            e_mode_category_id=int(new_category_id) if new_category_id > 0 else None,
        )
        session.add(config)
        logger.info(
            f"Created AaveV3AssetConfig for {asset_address} with eMode category {new_category_id}"
        )
    else:
        config.e_mode_category_id = int(new_category_id) if new_category_id > 0 else None
        logger.info(
            f"Updated eMode category for {asset_address}: {old_category_id} -> {new_category_id}"
        )

    return config


def _process_asset_collateral_in_emode_changed_event(
    *,
    session: Session,
    event: LogReceipt,
    market_id: int,
) -> AaveV3AssetConfig | None:
    """
    Process an AssetCollateralInEModeChanged event (newer Aave versions v3.4+).

    This event is emitted when an asset is added or removed as collateral
    in an eMode category. The category_id in the event is the asset's primary
    eMode category.

    Reference:
    ```
    event AssetCollateralInEModeChanged(
        address indexed asset,
        uint8 categoryId,
        bool collateral
    );
    ```
    """
    asset_address = decode_address(event["topics"][1])

    # Decode non-indexed parameters
    category_id, is_collateral = eth_abi.abi.decode(
        types=["uint8", "bool"],
        data=event["data"],
    )

    # Find the asset
    asset = session.scalar(
        select(AaveV3Asset).where(
            AaveV3Asset.market_id == market_id,
            AaveV3Asset.underlying_token.has(address=get_checksum_address(asset_address)),
        )
    )
    assert asset is not None

    # Get or create the asset config
    config = session.scalar(select(AaveV3AssetConfig).where(AaveV3AssetConfig.asset_id == asset.id))

    if config is None:
        # Only set e_mode_category_id if this asset is being added as collateral
        e_mode_cat = int(category_id) if is_collateral and category_id > 0 else None
        config = AaveV3AssetConfig(
            asset_id=asset.id,
            ltv=0,
            liquidation_threshold=0,
            liquidation_bonus=0,
            borrowing_enabled=False,
            stable_borrowing_enabled=False,
            flash_loan_enabled=False,
            borrowable_in_isolation=False,
            isolation_mode=False,
            debt_ceiling=None,
            e_mode_category_id=e_mode_cat,
        )
        session.add(config)
        logger.info(
            f"Created AaveV3AssetConfig for {asset_address} with eMode category {e_mode_cat}"
        )
    # Update category if asset is being added as collateral, clear if removed
    elif is_collateral and category_id > 0:
        config.e_mode_category_id = int(category_id)
        logger.info(f"Set eMode category for {asset_address} to {category_id} (collateral enabled)")

    return config


def _process_reserve_used_as_collateral_enabled_event(
    *,
    session: Session,
    event: LogReceipt,
    market_id: int,
) -> AaveV3UserCollateralConfig | None:
    """
    Process a ReserveUsedAsCollateralEnabled event.

    Reference:
    ```
    event ReserveUsedAsCollateralEnabled(
        address indexed reserve,
        address indexed user
    );
    ```
    """

    asset_address = decode_address(event["topics"][1])
    user_address = decode_address(event["topics"][2])

    # Find the asset
    asset = session.scalar(
        select(AaveV3Asset).where(
            AaveV3Asset.market_id == market_id,
            AaveV3Asset.underlying_token.has(address=get_checksum_address(asset_address)),
        )
    )
    assert asset is not None

    # Find or create the user
    user = session.scalar(
        select(AaveV3User).where(
            AaveV3User.market_id == market_id,
            AaveV3User.address == get_checksum_address(user_address),
        )
    )
    assert user is not None

    # Get or create the collateral config
    config = session.scalar(
        select(AaveV3UserCollateralConfig).where(
            AaveV3UserCollateralConfig.user_id == user.id,
            AaveV3UserCollateralConfig.asset_id == asset.id,
        )
    )

    if config is None:
        config = AaveV3UserCollateralConfig(
            user_id=user.id,
            asset_id=asset.id,
            enabled=True,
        )
        session.add(config)
    else:
        config.enabled = True

    logger.debug(f"Collateral enabled for user {user_address} asset {asset_address}")
    return config


def _process_reserve_used_as_collateral_disabled_event(
    *,
    session: Session,
    event: LogReceipt,
    market_id: int,
) -> AaveV3UserCollateralConfig | None:
    """
    Process a ReserveUsedAsCollateralDisabled event.

    Reference:
    ```
    event ReserveUsedAsCollateralDisabled(
        address indexed reserve,
        address indexed user
    );
    ```
    """

    asset_address = decode_address(event["topics"][1])
    user_address = decode_address(event["topics"][2])

    # Find the asset
    asset = session.scalar(
        select(AaveV3Asset).where(
            AaveV3Asset.market_id == market_id,
            AaveV3Asset.underlying_token.has(address=get_checksum_address(asset_address)),
        )
    )
    assert asset is not None

    # Find the user
    user = session.scalar(
        select(AaveV3User).where(
            AaveV3User.market_id == market_id,
            AaveV3User.address == get_checksum_address(user_address),
        )
    )
    assert user is not None

    # Get the collateral config
    config = session.scalar(
        select(AaveV3UserCollateralConfig).where(
            AaveV3UserCollateralConfig.user_id == user.id,
            AaveV3UserCollateralConfig.asset_id == asset.id,
        )
    )

    if config is None:
        # No existing config - create one disabled
        config = AaveV3UserCollateralConfig(
            user_id=user.id,
            asset_id=asset.id,
            enabled=False,
        )
        session.add(config)
    else:
        config.enabled = False

    logger.debug(f"Collateral disabled for user {user_address} asset {asset_address}")
    return config


def _process_asset_initialization_event(
    provider: ProviderAdapter,
    event: LogReceipt,
    market: AaveV3Market,
    session: Session,
) -> None:
    """
    Process a ReserveInitialized event to add a new Aave asset to the database.

    Reference:
    ```
    event ReserveInitialized(
        address indexed asset,
        address indexed aToken,
        address stableDebtToken,
        address variableDebtToken,
        address interestRateStrategyAddress
    );
    ```
    """

    logger.debug(f"Processing asset initialization event at block {event['blockNumber']}")

    asset_address = decode_address(event["topics"][1])
    a_token_address = decode_address(event["topics"][2])

    # Note: stableDebtToken is deprecated in Aave V3 and no longer used, so is ignored
    v_token_address: str
    (_, v_token_address, _) = eth_abi.abi.decode(
        types=["address", "address", "address"], data=event["data"]
    )
    v_token_address = get_checksum_address(v_token_address)

    erc20_token_in_db = get_or_create_erc20_token(
        provider=provider,
        session=session,
        chain_id=market.chain_id,
        token_address=asset_address,
    )
    a_token = get_or_create_erc20_token(
        provider=provider,
        session=session,
        chain_id=market.chain_id,
        token_address=a_token_address,
    )
    v_token = get_or_create_erc20_token(
        provider=provider,
        session=session,
        chain_id=market.chain_id,
        token_address=v_token_address,
    )

    # Per EIP-1967, the implementation address is stored at a known storage slot
    (atoken_implementation_address,) = eth_abi.abi.decode(
        types=["address"],
        data=provider.get_storage_at(
            address=a_token_address,
            position=ERC_1967_IMPLEMENTATION_SLOT,
            block=event["blockNumber"],
        ),
    )
    atoken_implementation_address = get_checksum_address(atoken_implementation_address)

    (vtoken_implementation_address,) = eth_abi.abi.decode(
        types=["address"],
        data=provider.get_storage_at(
            address=v_token_address,
            position=ERC_1967_IMPLEMENTATION_SLOT,
            block=event["blockNumber"],
        ),
    )
    vtoken_implementation_address = get_checksum_address(vtoken_implementation_address)

    (atoken_revision,) = raw_call(
        w3=provider,
        address=atoken_implementation_address,
        calldata=encode_function_calldata(
            function_prototype="ATOKEN_REVISION()",
            function_arguments=None,
        ),
        return_types=["uint256"],
    )
    (vtoken_revision,) = raw_call(
        w3=provider,
        address=vtoken_implementation_address,
        calldata=encode_function_calldata(
            function_prototype="DEBT_TOKEN_REVISION()",
            function_arguments=None,
        ),
        return_types=["uint256"],
    )

    asset = AaveV3Asset(
        market_id=market.id,
        underlying_asset_id=erc20_token_in_db.id,
        a_token_id=a_token.id,
        a_token_revision=atoken_revision,
        v_token_id=v_token.id,
        v_token_revision=vtoken_revision,
        liquidity_index=0,
        liquidity_rate=0,
        borrow_index=0,
        borrow_rate=0,
    )
    session.add(asset)
    session.flush([asset])
    logger.info(f"Added new Aave V3 asset: {asset.underlying_token!r}")

    # Fetch and set the initial price source from the oracle
    oracle_contract = get_contract(
        session=session,
        market=market,
        contract_name="PRICE_ORACLE",
    )
    assert oracle_contract is not None

    (price_source,) = raw_call(
        w3=provider,
        address=oracle_contract.address,
        calldata=encode_function_calldata(
            function_prototype="getSourceOfAsset(address)",
            function_arguments=[asset_address],
        ),
        return_types=["address"],
        block_identifier=event["blockNumber"],
    )
    asset.price_source = get_checksum_address(price_source)
    logger.info(f"Set initial price source for {asset_address} to {price_source}")

    # If this is the GHO asset, update the GHO token entry with the vToken reference
    gho_asset = get_gho_asset(session, market)
    if asset_address == gho_asset.token.address:
        gho_token_entry = session.scalar(
            select(AaveGhoToken).where(AaveGhoToken.token_id == erc20_token_in_db.id)
        )
        assert gho_token_entry is not None
        gho_token_entry.v_token_id = v_token.id
        logger.info(f"Updated AaveGhoToken v_token_id to {v_token.id} ({v_token_address})")


def _process_user_e_mode_set_event(
    *,
    event: LogReceipt,
    tx_context: TransactionContext,
) -> None:
    """
    Process a UserEModeSet event to update a user's E-Mode category.

    Reference:
    ```
    event UserEModeSet(
        address indexed user,
        uint8 categoryId
    );
    ```
    """

    logger.debug(f"Processing user E-mode set event for user at block {event['blockNumber']}")

    user_address = decode_address(event["topics"][1])

    (e_mode,) = eth_abi.abi.decode(types=["uint8"], data=event["data"])

    user = get_or_create_user(
        tx_context=tx_context,
        user_address=user_address,
        block_number=event["blockNumber"],
    )
    user.e_mode = e_mode


def _process_discount_token_updated_event(
    *,
    event: LogReceipt,
    gho_asset: AaveGhoToken,
) -> None:
    """
    Process a DiscountTokenUpdated event to set the GHO vToken discount token.

    Reference:
    ```
    event DiscountTokenUpdated(
        address indexed oldDiscountToken,
        address indexed newDiscountToken
    );
    ```
    """

    # Ignore the event if it didn't come from the GHO VariableDebtToken contract
    if gho_asset.v_token is None or gho_asset.v_token.address != event["address"]:
        logger.debug(
            "Ignoring DiscountTokenUpdated event, not from canonical GHO VariableDebtToken contract"
        )
        return

    logger.debug(f"Processing discount token updated event at block {event['blockNumber']}")

    old_discount_token_address = decode_address(event["topics"][1])
    new_discount_token_address = decode_address(event["topics"][2])

    gho_asset.v_gho_discount_token = new_discount_token_address

    logger.info(
        f"SET NEW DISCOUNT TOKEN: {old_discount_token_address} -> {new_discount_token_address}"
    )


def _process_discount_rate_strategy_updated_event(
    *,
    event: LogReceipt,
    gho_asset: AaveGhoToken,
) -> None:
    """
    Process a DiscountRateStrategyUpdated event to set the GHO vToken attribute

    Reference:
    ```
    event DiscountRateStrategyUpdated(
        address indexed oldDiscountRateStrategy,
        address indexed newDiscountRateStrategy
    );
    ```
    """

    # Ignore the event if it didn't come from the GHO VariableDebtToken contract
    if gho_asset.v_token is None or gho_asset.v_token.address != event["address"]:
        logger.debug(
            "Ignoring DiscountRateStrategyUpdated event, not from canonical GHO VariableDebtToken "
            "contract"
        )
        return

    logger.debug(f"Processing discount rate strategy updated event at block {event['blockNumber']}")

    old_discount_rate_strategy_address = decode_address(event["topics"][1])
    new_discount_rate_strategy_address = decode_address(event["topics"][2])

    gho_asset.v_gho_discount_rate_strategy = new_discount_rate_strategy_address

    logger.info(
        f"SET NEW DISCOUNT RATE STRATEGY: {old_discount_rate_strategy_address} -> "
        f"{new_discount_rate_strategy_address}"
    )


def _process_reserve_data_update_event(
    *,
    session: Session,
    event: LogReceipt,
    market: AaveV3Market,
) -> None:
    """
    Process a ReserveDataUpdated event to update asset rates and indices.

    Reference:
    ```
    event ReserveDataUpdated(
        address indexed reserve,
        uint256 liquidityRate,
        uint256 stableBorrowRate,
        uint256 variableBorrowRate,
        uint256 liquidityIndex,
        uint256 variableBorrowIndex
    );
    ```
    """

    logger.debug(f"Processing reserve data update event at block {event['blockNumber']}")

    reserve_asset_address = decode_address(event["topics"][1])

    asset_in_db = session.scalar(
        select(AaveV3Asset)
        .join(Erc20TokenTable, AaveV3Asset.underlying_asset_id == Erc20TokenTable.id)
        .where(
            AaveV3Asset.market_id == market.id,
            Erc20TokenTable.address == reserve_asset_address,
        )
    )
    assert asset_in_db is not None

    if asset_in_db.last_update_block is not None:
        assert asset_in_db.last_update_block <= event["blockNumber"]

    liquidity_rate: int

    variable_borrow_rate: int
    liquidity_index: int
    variable_borrow_index: int
    (
        liquidity_rate,
        _,  # stable borrow rate is deprecated on Aave V3
        variable_borrow_rate,
        liquidity_index,
        variable_borrow_index,
    ) = eth_abi.abi.decode(
        types=["uint256", "uint256", "uint256", "uint256", "uint256"],
        data=event["data"],
    )

    asset_in_db.liquidity_rate = liquidity_rate
    asset_in_db.borrow_rate = variable_borrow_rate
    asset_in_db.liquidity_index = liquidity_index
    asset_in_db.borrow_index = variable_borrow_index
    asset_in_db.last_update_block = event["blockNumber"]


def _process_scaled_token_upgrade_event(
    *,
    event: LogReceipt,
    tx_context: TransactionContext,
) -> None:
    """
    Process an Upgraded event to update the aToken or vToken revision.

    Reference:
    ```
    event Upgraded(
        address indexed implementation
    );
    ```
    """

    logger.debug(f"Processing scaled token upgrade event at block {event['blockNumber']}")

    new_implementation_address = decode_address(event["topics"][1])

    if (
        aave_collateral_asset := get_asset_by_token_type(
            session=tx_context.session,
            market=tx_context.market,
            token_address=event["address"],
            token_type=TokenType.A_TOKEN,
        )
    ) is not None:
        (atoken_revision,) = raw_call(
            w3=tx_context.provider,
            address=new_implementation_address,
            calldata=encode_function_calldata(
                function_prototype="ATOKEN_REVISION()",
                function_arguments=None,
            ),
            return_types=["uint256"],
        )
        aave_collateral_asset.a_token_revision = atoken_revision
        logger.info(
            f"Upgraded aToken revision for {aave_collateral_asset.a_token} to {atoken_revision}"
        )
    elif (
        aave_debt_asset := get_asset_by_token_type(
            session=tx_context.session,
            market=tx_context.market,
            token_address=event["address"],
            token_type=TokenType.V_TOKEN,
        )
    ) is not None:
        (vtoken_revision,) = raw_call(
            w3=tx_context.provider,
            address=new_implementation_address,
            calldata=encode_function_calldata(
                function_prototype="DEBT_TOKEN_REVISION()",
                function_arguments=None,
            ),
            return_types=["uint256"],
        )
        aave_debt_asset.v_token_revision = vtoken_revision
        logger.info(f"Upgraded vToken revision for {aave_debt_asset.v_token} to {vtoken_revision}")

        # Handle GHO discount deprecation on upgrade to revision 4+
        gho_asset = get_gho_asset(tx_context.session, tx_context.market)
        if (
            gho_asset.v_token is not None
            and aave_debt_asset.v_token.address == gho_asset.v_token.address
            and vtoken_revision >= GHO_DISCOUNT_DEPRECATION_REVISION
        ):
            gho_asset.v_gho_discount_token = None
            gho_asset.v_gho_discount_rate_strategy = None

            # Reset all users' GHO discount to 0 since discount mechanism is deprecated
            for user in (
                tx_context.session
                .execute(
                    select(AaveV3User).where(
                        AaveV3User.market_id == tx_context.market.id,
                        AaveV3User.gho_discount != 0,
                    )
                )
                .scalars()
                .all()
            ):
                user.gho_discount = 0
            logger.info(
                f"GHO discount mechanism deprecated at revision {vtoken_revision}. "
                "Set GHO discounts to 0"
            )

    else:
        assert_never()


def _update_contract_revision(
    *,
    session: Session,
    provider: ProviderAdapter,
    market: AaveV3Market,
    contract_name: str,
    new_address: ChecksumAddress,
    revision_function_prototype: str,
) -> None:
    """
    Update contract revision in database.
    """

    revision: int
    (revision,) = raw_call(
        w3=provider,
        address=new_address,
        calldata=encode_function_calldata(
            function_prototype=f"{revision_function_prototype}()",
            function_arguments=None,
        ),
        return_types=["uint256"],
    )

    contract = get_contract(
        session=session,
        market=market,
        contract_name=contract_name,
    )
    assert contract is not None

    contract.revision = revision

    logger.info(f"Upgraded revision for {contract.name} to {revision}")


def _process_proxy_creation_event(
    *,
    provider: ProviderAdapter,
    session: Session,
    market: AaveV3Market,
    event: LogReceipt,
    proxy_name: str,
    proxy_id: bytes,
    revision_function_prototype: str,
) -> None:
    """
    Process a proxy creation event (POOL or POOL_CONFIGURATOR).
    """

    logger.debug(f"Processing _process_proxy_creation_event at block {event['blockNumber']}")

    (decoded_proxy_id,) = eth_abi.abi.decode(types=["bytes32"], data=event["topics"][1])

    if decoded_proxy_id != proxy_id:
        return

    proxy_address = decode_address(event["topics"][2])
    implementation_address = decode_address(event["topics"][3])

    (revision,) = raw_call(
        w3=provider,
        address=implementation_address,
        calldata=encode_function_calldata(
            function_prototype=f"{revision_function_prototype}()",
            function_arguments=None,
        ),
        return_types=["uint256"],
    )

    market.contracts.append(
        AaveV3Contract(
            market_id=market.id,
            name=proxy_name,
            address=proxy_address,
            revision=revision,
        )
    )


def _process_pool_data_provider_updated_event(
    *,
    session: Session,
    market: AaveV3Market,
    event: LogReceipt,
) -> None:
    """
    Process a PoolDataProviderUpdated event chronologically.

    Event structure:
    - topics[1]: oldAddress (address indexed)
    - topics[2]: newAddress (address indexed)
    """
    old_pool_data_provider_address = decode_address(event["topics"][1])
    new_pool_data_provider_address = decode_address(event["topics"][2])

    if old_pool_data_provider_address == ZERO_ADDRESS:
        session.add(
            AaveV3Contract(
                market_id=market.id,
                name="POOL_DATA_PROVIDER",
                address=new_pool_data_provider_address,
            )
        )
    else:
        pool_data_provider = session.scalar(
            select(AaveV3Contract).where(AaveV3Contract.address == old_pool_data_provider_address)
        )
        assert pool_data_provider is not None
        pool_data_provider.address = new_pool_data_provider_address


def _process_address_set_event(
    *,
    session: Session,
    market: AaveV3Market,
    event: LogReceipt,
) -> None:
    """
    Process an AddressSet event chronologically.

    Event structure:
    - topics[1]: id (bytes32 indexed) - contract identifier
    - topics[2]: oldAddress (address indexed)
    - topics[3]: newAddress (address indexed)
    """

    # Decode the contract id from bytes32
    contract_id_bytes: bytes
    (contract_id_bytes,) = eth_abi.abi.decode(types=["bytes32"], data=event["topics"][1])
    contract_name = contract_id_bytes.decode("ascii").strip("\x00")

    old_address = decode_address(event["topics"][2])
    assert old_address == ZERO_ADDRESS

    new_address = decode_address(event["topics"][3])
    session.add(
        AaveV3Contract(
            market_id=market.id,
            name=contract_name,
            address=new_address,
        )
    )
    logger.info(f"Registered contract {contract_name}: @ {new_address}")


def _process_price_oracle_updated_event(
    *,
    session: Session,
    market: AaveV3Market,
    event: LogReceipt,
) -> None:
    """
    Process a PriceOracleUpdated event from the PoolAddressesProvider.

    Event structure:
    - topics[1]: oldAddress (address indexed)
    - topics[2]: newAddress (address indexed)

    This event is emitted when the price oracle address is updated in the
    PoolAddressesProvider. We use it to track the canonical oracle address.

    Event definition:
        event PriceOracleUpdated(
            address indexed oldAddress,
            address indexed newAddress
        );
    """

    old_address = decode_address(event["topics"][1])
    new_address = decode_address(event["topics"][2])

    logger.info(
        f"PriceOracleUpdated: oldAddress={old_address}, newAddress={new_address} "
        f"(block {event['blockNumber']})"
    )

    # Register or update the PRICE_ORACLE in the database
    existing_oracle = session.scalar(
        select(AaveV3Contract).where(
            AaveV3Contract.market_id == market.id,
            AaveV3Contract.name == "PRICE_ORACLE",
        )
    )
    assert existing_oracle is None

    session.add(
        AaveV3Contract(
            market_id=market.id,
            name="PRICE_ORACLE",
            address=new_address,
        )
    )
    logger.info(f"Registered PRICE_ORACLE at {new_address} from PriceOracleUpdated event")


def _process_asset_source_updated_event(
    *,
    session: Session,
    market: AaveV3Market,
    event: LogReceipt,
) -> None:
    """
    Process an AssetSourceUpdated event from the AaveOracle.

    Event structure:
    - topics[1]: asset (address indexed)
    - topics[2]: source (address indexed)

    Updates the asset's price_source field in the database.

    Event definition:
        event AssetSourceUpdated(
            address indexed asset,
            address indexed source
        );
    """

    asset_address = decode_address(event["topics"][1])
    source_address = decode_address(event["topics"][2])

    # Find the asset by underlying token address
    asset = session.scalar(
        select(AaveV3Asset).where(
            AaveV3Asset.market_id == market.id,
            AaveV3Asset.underlying_token.has(address=get_checksum_address(asset_address)),
        )
    )
    assert asset is not None

    asset.price_source = get_checksum_address(source_address)
    logger.info(
        f"AssetSourceUpdated: asset={asset_address}, source={source_address} "
        f"(block {event['blockNumber']})"
    )


def _process_discount_percent_updated_event(
    *,
    event: LogReceipt,
    tx_context: TransactionContext,
) -> None:
    """
    Process a GHO discount percent update event.

    With transaction-level processing, this is called BEFORE Mint/Burn
    events in the same transaction, so the discount rate is already updated.

    Reference:
    ```
    event DiscountPercentUpdated(
        address indexed user,
        uint256 oldDiscountPercent,
        uint256 indexed newDiscountPercent
    );
    ```
    """

    logger.debug(
        f"Processing _process_discount_percent_updated_event at block {event['blockNumber']}"
    )

    user_address = decode_address(event["topics"][1])

    (_old_discount_percent,) = eth_abi.abi.decode(types=["uint256"], data=event["data"])
    (new_discount_percent,) = eth_abi.abi.decode(types=["uint256"], data=event["topics"][2])

    user = get_or_create_user(
        tx_context=tx_context,
        user_address=user_address,
        block_number=event["blockNumber"],
    )

    # With transaction-level processing, the discount is updated here
    # and subsequent Mint/Burn events in the same transaction will see
    # the updated value
    logger.debug(
        f"DiscountPercentUpdated for {user_address}: "
        f"{user.gho_discount} -> {new_discount_percent} "
        f"(user.id={user.id})"
    )
    user.gho_discount = new_discount_percent
