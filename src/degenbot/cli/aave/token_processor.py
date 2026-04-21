"""
Token processing functions for Aave V3.

This module contains functions for processing token mint/burn events and
updating user positions. It delegates to revision-specific processors for
handling different token contract versions.
"""

from typing import TYPE_CHECKING, assert_never

import eth_abi.abi
from web3.types import LogReceipt

from degenbot.aave.events import AaveV3PoolEvent
from degenbot.aave.libraries.gho_math import GhoMath
from degenbot.aave.libraries.pool_math import PoolMath
from degenbot.aave.libraries.token_math import TokenMathFactory
from degenbot.aave.models import EnrichedScaledTokenEvent
from degenbot.aave.pattern_types import LiquidationPattern
from degenbot.aave.processors import (
    CollateralBurnEvent,
    CollateralMintEvent,
    DebtBurnEvent,
    DebtMintEvent,
    TokenProcessorFactory,
)
from degenbot.cli.aave.constants import UserOperation, WadRayMathLibrary
from degenbot.cli.aave.db_assets import get_asset_by_token_type, get_asset_identifier
from degenbot.cli.aave.db_positions import (
    get_or_create_collateral_position,
    get_or_create_debt_position,
)
from degenbot.cli.aave.db_users import get_or_create_user
from degenbot.cli.aave.stkaave import get_or_init_stk_aave_balance
from degenbot.cli.aave.transfers import _process_collateral_transfer
from degenbot.cli.aave.types import TokenType, TransactionContext
from degenbot.cli.aave.verification import update_debt_position_index
from degenbot.cli.aave_transaction_operations import (
    Operation,
    OperationType,
    ScaledTokenEvent,
    ScaledTokenEventType,
)
from degenbot.cli.aave_utils import decode_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.database.models.aave import AaveV3CollateralPosition, AaveV3DebtPosition, AaveV3User
from degenbot.logging import logger

if TYPE_CHECKING:
    from degenbot.aave.processors.base import ScaledTokenBurnResult, ScaledTokenMintResult


def _process_scaled_token_operation(
    event: CollateralMintEvent | CollateralBurnEvent | DebtMintEvent | DebtBurnEvent,
    scaled_token_revision: int,
    position: "AaveV3CollateralPosition | AaveV3DebtPosition",
) -> UserOperation:
    """
    Determine the user operation for scaled token events and apply the appropriate delta to the
    position balance.

    This function delegates to revision-specific processors for handling token events.

    Args:
        event: The scaled token event data
        scaled_token_revision: The token contract revision
        position: The user's position to update
    """

    # Determine token type for logging
    token_type = (
        "aToken" if isinstance(event, (CollateralMintEvent, CollateralBurnEvent)) else "vToken"
    )
    logger.debug(
        f"Processing scaled token operation ({type(event).__name__}) for {token_type} revision "
        f"{scaled_token_revision}"
    )
    logger.debug(position)

    match event:
        case CollateralMintEvent():
            assert isinstance(position, AaveV3CollateralPosition)
            collateral_processor = TokenProcessorFactory.get_collateral_processor(
                scaled_token_revision
            )
            mint_result: ScaledTokenMintResult = collateral_processor.process_mint_event(
                event_data=event,
                previous_balance=position.balance,
                previous_index=position.last_index or 0,
                scaled_delta=event.scaled_amount,
            )
            position.balance += mint_result.balance_delta
            # Only update last_index if the new index is greater than current
            # This prevents earlier events (in log index order) from overwriting
            # later events' indices when operations are processed out of order
            if mint_result.new_index > (position.last_index or 0):
                position.last_index = mint_result.new_index
            return UserOperation.WITHDRAW if mint_result.is_repay else UserOperation.DEPOSIT

        case CollateralBurnEvent():
            assert isinstance(position, AaveV3CollateralPosition)
            collateral_processor = TokenProcessorFactory.get_collateral_processor(
                scaled_token_revision
            )
            burn_result: ScaledTokenBurnResult = collateral_processor.process_burn_event(
                event_data=event,
                previous_balance=position.balance,
                previous_index=position.last_index or 0,
                scaled_delta=event.scaled_amount,
            )
            logger.debug(
                f"_process_scaled_token_operation burn: delta={burn_result.balance_delta}, "
                f"new_balance={position.balance + burn_result.balance_delta}"
            )
            position.balance += burn_result.balance_delta
            # Only update last_index if the new index is greater than current
            # This prevents earlier events (in log index order) from overwriting
            # later events' indices when operations are processed out of order
            if burn_result.new_index > (position.last_index or 0):
                position.last_index = burn_result.new_index
            return UserOperation.WITHDRAW

        case DebtMintEvent():
            assert isinstance(position, AaveV3DebtPosition)
            debt_processor = TokenProcessorFactory.get_debt_processor(scaled_token_revision)
            debt_mint_result: ScaledTokenMintResult = debt_processor.process_mint_event(
                event_data=event,
                previous_balance=position.balance,
                previous_index=position.last_index or 0,
                scaled_delta=event.scaled_amount,
            )
            position.balance += debt_mint_result.balance_delta
            # Only update last_index if the new index is greater than current
            # This prevents earlier events (in log index order) from overwriting
            # later events' indices when operations are processed out of order
            if debt_mint_result.new_index > (position.last_index or 0):
                position.last_index = debt_mint_result.new_index
            return UserOperation.REPAY if debt_mint_result.is_repay else UserOperation.BORROW

        case DebtBurnEvent():
            assert isinstance(position, AaveV3DebtPosition)
            debt_processor = TokenProcessorFactory.get_debt_processor(scaled_token_revision)
            debt_burn_result: ScaledTokenBurnResult = debt_processor.process_burn_event(
                event_data=event,
                previous_balance=position.balance,
                previous_index=position.last_index or 0,
                scaled_delta=event.scaled_amount,
            )
            position.balance += debt_burn_result.balance_delta
            # Only update last_index if the new index is greater than current
            # This prevents earlier events (in log index order) from overwriting
            # later events' indices when operations are processed out of order
            if debt_burn_result.new_index > (position.last_index or 0):
                position.last_index = debt_burn_result.new_index
            return UserOperation.REPAY

        case _ as unreachable:
            assert_never(unreachable)


def calculate_gho_discount_rate(
    debt_balance: int,
    discount_token_balance: int,
) -> int:
    """
    Calculate the GHO discount rate locally.

    Delegates to GhoMath.calculate_discount_rate which mirrors the logic from
    the GhoDiscountRateStrategy contract at mainnet address
    0x4C38Ec4D1D2068540DfC11DFa4de41F733DDF812.

    Returns the discount rate in basis points (10000 = 100.00%).
    """
    return GhoMath.calculate_discount_rate(
        debt_balance=debt_balance,
        discount_token_balance=discount_token_balance,
    )


def _refresh_discount_rate(
    *,
    user: AaveV3User,
    discount_token_balance: int,
    scaled_debt_balance: int,
    debt_index: int,
    wad_ray_math: WadRayMathLibrary,
) -> None:
    """
    Calculate and update the user's GHO discount rate.

    Calculates the debt token balance from scaled balance and index, then
    computes the discount rate locally using the same logic as the GhoDiscountRateStrategy
    contract.
    """

    debt_token_balance = wad_ray_math.ray_mul(
        a=scaled_debt_balance,
        b=debt_index,
    )
    user.gho_discount = calculate_gho_discount_rate(
        debt_balance=debt_token_balance,
        discount_token_balance=discount_token_balance,
    )


def _calculate_mint_to_treasury_scaled_amount(
    scaled_event: ScaledTokenEvent,
    operation: Operation,
) -> int:
    """Calculate scaled amount for MINT_TO_TREASURY operations.

    Delegates to PoolMath for revision-aware calculation. This ensures
    the correct rounding mode is used based on Pool revision.

    Args:
        scaled_event: The scaled token Mint event
        operation: The operation containing minted_to_treasury_amount

    Returns:
        The calculated scaled amount to add to the treasury position
    """
    assert scaled_event.balance_increase is not None
    assert scaled_event.index is not None

    # Get the underlying amount to mint
    # Use MintedToTreasury event if available, otherwise calculate from Mint event
    assert operation.minted_to_treasury_amount is not None
    minted_amount = operation.minted_to_treasury_amount

    # Use PoolMath for revision-aware calculation
    return PoolMath.underlying_to_scaled_collateral(
        underlying_amount=minted_amount,
        liquidity_index=scaled_event.index,
        pool_revision=operation.pool_revision,
    )


def _process_deficit_coverage_operation(
    *,
    operation: Operation,
    tx_context: TransactionContext,
) -> None:
    """
    Process DEFICIT_COVERAGE operations atomically.

    DEFICIT_COVERAGE operations contain paired Transfer + Burn events that occur
    during Umbrella protocol's deficit coverage operations. These must be processed
    atomically (credit then debit) to maintain correct balances.

    The pattern is:
    1. Transfer/BalanceTransfer credits user's collateral position
    2. Burn debits user's collateral position (including accrued interest)
    3. Net effect should be zero or the interest amount
    """
    # Sort events by log index to ensure chronological processing
    sorted_events = sorted(
        operation.scaled_token_events,
        key=lambda e: e.event["logIndex"],
    )

    # Process transfer events first (credit the user)
    for scaled_event in sorted_events:
        if scaled_event.event_type in {
            ScaledTokenEventType.COLLATERAL_TRANSFER,
            ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
        }:
            _process_collateral_transfer(
                tx_context=tx_context,
                operation=operation,
                scaled_event=scaled_event,
            )

    # Process burn events last (debit the user)
    for scaled_event in sorted_events:
        if scaled_event.event_type == ScaledTokenEventType.COLLATERAL_BURN:
            # Skip enrichment validation for deficit coverage burns
            # The burn amount may not match standard calculations because
            # it includes interest accrued during the deficit coverage
            _process_deficit_coverage_burn(
                tx_context=tx_context,
                scaled_event=scaled_event,
            )


def _process_deficit_coverage_burn(
    *,
    tx_context: TransactionContext,
    scaled_event: ScaledTokenEvent,
) -> None:
    """
    Process a burn event within a DEFICIT_COVERAGE operation.

    Unlike regular burns, deficit coverage burns don't need enrichment validation
    because the amount includes interest that was accrued between the transfer
    and the burn within the same transaction.
    """

    # Get collateral asset
    token_address = scaled_event.event["address"]
    collateral_asset = get_asset_by_token_type(
        session=tx_context.session,
        market=tx_context.market,
        token_address=token_address,
        token_type=TokenType.A_TOKEN,
    )

    assert collateral_asset

    # Get user
    user = get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.user_address,
        block_number=scaled_event.event["blockNumber"],
    )

    # Get collateral position
    collateral_position = get_or_create_collateral_position(
        tx_context=tx_context,
        user=user,
        asset_id=collateral_asset.id,
    )

    # Calculate scaled amount directly without enrichment validation
    # The raw amount needs to be converted to scaled amount
    assert scaled_event.index is not None
    token_math = TokenMathFactory.get_token_math_for_token_revision(
        collateral_asset.a_token_revision
    )
    scaled_amount = token_math.get_collateral_burn_scaled_amount(
        amount=scaled_event.amount,
        liquidity_index=scaled_event.index,
    )

    # Process the burn directly
    assert scaled_event.balance_increase is not None
    _process_scaled_token_operation(
        event=CollateralBurnEvent(
            value=scaled_event.amount,
            balance_increase=scaled_event.balance_increase,
            index=scaled_event.index,
            scaled_amount=scaled_amount,
        ),
        scaled_token_revision=collateral_asset.a_token_revision,
        position=collateral_position,
    )


def _process_collateral_mint_with_match(
    *,
    event: LogReceipt,
    tx_context: TransactionContext,
    operation: Operation,
    scaled_event: ScaledTokenEvent,
    enriched_event: EnrichedScaledTokenEvent,
) -> None:
    """
    Process collateral (aToken) mint with operation match.
    """

    token_address = scaled_event.event["address"]
    collateral_asset = get_asset_by_token_type(
        session=tx_context.session,
        market=tx_context.market,
        token_address=token_address,
        token_type=TokenType.A_TOKEN,
    )

    assert collateral_asset

    asset_identifier = get_asset_identifier(collateral_asset)
    logger.debug(
        f"[Pool rev {tx_context.pool_revision}] Processing {asset_identifier} collateral mint "
        f"at block {event['blockNumber']}"
    )

    user = get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.user_address,
        block_number=scaled_event.event["blockNumber"],
    )

    # Get or create collateral position
    collateral_position = get_or_create_collateral_position(
        tx_context=tx_context,
        user=user,
        asset_id=collateral_asset.id,
    )

    # Use enriched event data for scaled amount, or calculate for MINT_TO_TREASURY
    if operation.operation_type == OperationType.MINT_TO_TREASURY:
        # MINT_TO_TREASURY uses MintedToTreasury event amount
        scaled_amount = _calculate_mint_to_treasury_scaled_amount(
            scaled_event=scaled_event,
            operation=operation,
        )
    else:
        assert enriched_event.scaled_amount is not None
        scaled_amount = enriched_event.scaled_amount

    # Ensure required fields are present for CollateralMintEvent
    assert scaled_event.balance_increase is not None
    assert scaled_event.index is not None
    assert scaled_amount is not None

    _process_scaled_token_operation(
        event=CollateralMintEvent(
            value=scaled_event.amount,
            balance_increase=scaled_event.balance_increase,
            index=scaled_event.index,
            scaled_amount=scaled_amount,
        ),
        scaled_token_revision=collateral_asset.a_token_revision,
        position=collateral_position,
    )


def _process_collateral_burn_with_match(
    *,
    event: LogReceipt,
    tx_context: TransactionContext,
    scaled_event: ScaledTokenEvent,
    enriched_event: EnrichedScaledTokenEvent,
) -> None:
    """
    Process collateral (aToken) burn with operation match.
    """

    # Get collateral asset first for logging
    token_address = scaled_event.event["address"]
    collateral_asset = get_asset_by_token_type(
        session=tx_context.session,
        market=tx_context.market,
        token_address=token_address,
        token_type=TokenType.A_TOKEN,
    )

    assert collateral_asset

    asset_identifier = get_asset_identifier(collateral_asset)
    logger.debug(
        f"[Pool rev {tx_context.pool_revision}] Processing {asset_identifier} collateral burn "
        f"at block {event['blockNumber']}"
    )

    # Get user
    user = get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.user_address,
        block_number=scaled_event.event["blockNumber"],
    )

    # Get collateral position
    collateral_position = get_or_create_collateral_position(
        tx_context=tx_context,
        user=user,
        asset_id=collateral_asset.id,
    )

    # Use enriched event data for scaled amount
    # Enrichment layer reliably calculates scaled_amount for all burn events
    scaled_amount: int | None = enriched_event.scaled_amount
    raw_amount = enriched_event.raw_amount

    # Fallback calculation only if enrichment didn't provide scaled_amount
    # This should not happen for normal burns, but provides a safety net
    if scaled_amount is None and raw_amount is not None:
        token_math = TokenMathFactory.get_token_math_for_token_revision(
            collateral_asset.a_token_revision
        )
        assert scaled_event.index is not None
        scaled_amount = token_math.get_collateral_burn_scaled_amount(
            amount=raw_amount,
            liquidity_index=scaled_event.index,
        )

    assert scaled_event.balance_increase is not None
    assert scaled_event.index is not None
    _process_scaled_token_operation(
        event=CollateralBurnEvent(
            value=scaled_event.amount,
            balance_increase=scaled_event.balance_increase,
            index=scaled_event.index,
            scaled_amount=scaled_amount,
        ),
        scaled_token_revision=collateral_asset.a_token_revision,
        position=collateral_position,
    )
    logger.debug(
        f"After burn position id={id(collateral_position)}, balance={collateral_position.balance}"
    )


def _process_debt_mint_with_match(
    *,
    event: LogReceipt,
    tx_context: TransactionContext,
    operation: Operation,
    scaled_event: ScaledTokenEvent,
    enriched_event: EnrichedScaledTokenEvent,
) -> None:
    """
    Process debt (vToken) mint with operation match.

    Note: In REPAY operations, a Mint event is emitted when interest > repayment.
    In this case, the Mint event represents the net effect of:
    1. Interest accrual (increasing debt)
    2. Debt repayment (burning scaled tokens)
    The actual scaled burn amount = balance_increase - amount.
    """

    # Get debt asset first for logging
    token_address = scaled_event.event["address"]
    debt_asset = get_asset_by_token_type(
        session=tx_context.session,
        market=tx_context.market,
        token_address=token_address,
        token_type=TokenType.V_TOKEN,
    )

    assert debt_asset

    asset_identifier = get_asset_identifier(debt_asset)
    logger.debug(
        f"[Pool rev {tx_context.pool_revision}] Processing {asset_identifier} debt mint "
        f"at block {event['blockNumber']}"
    )

    user = get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.user_address,
        block_number=scaled_event.event["blockNumber"],
    )

    # Get or create debt position
    debt_position = get_or_create_debt_position(
        tx_context=tx_context,
        user=user,
        asset_id=debt_asset.id,
    )

    # Check if this is a GHO token first (needed for INTEREST_ACCRUAL handling)
    is_gho = tx_context.is_gho_vtoken(token_address)

    # Use enriched event data for scaled amount
    scaled_amount: int | None = enriched_event.scaled_amount

    # Process GHO tokens through GHO-specific processor (handles discounts for all operations)
    if is_gho:
        # Use the effective discount from transaction context
        effective_discount = tx_context.user_discounts.get(user.address, user.gho_discount)

        # Process using GHO-specific processor
        gho_processor = TokenProcessorFactory.get_gho_debt_processor(debt_asset.v_token_revision)
        assert scaled_event.balance_increase is not None
        assert scaled_event.index is not None

        # For GHO_REPAY operations, extract the actual repay amount from the Repay event
        # to avoid 1 wei rounding errors from deriving from Mint event fields.
        # The processor will handle the full logic including discount calculations.
        # See debug/aave/0037 and 0038 for details.
        actual_repay_amount: int | None = None
        if operation.operation_type == OperationType.GHO_REPAY:
            assert operation.pool_event is not None
            # Repay event: Repay(address indexed reserve, address indexed user,
            #   address indexed repayer, uint256 amount, bool useATokens)
            repay_amount_data, _ = eth_abi.abi.decode(
                types=["uint256", "bool"],
                data=operation.pool_event["data"],
            )
            actual_repay_amount = repay_amount_data

        gho_result = gho_processor.process_mint_event(
            event_data=DebtMintEvent(
                caller=scaled_event.caller_address or scaled_event.user_address,
                on_behalf_of=scaled_event.user_address,
                value=scaled_event.amount,
                balance_increase=scaled_event.balance_increase,
                index=scaled_event.index,
                scaled_amount=scaled_amount,
            ),
            previous_balance=debt_position.balance,
            previous_index=debt_position.last_index or 0,
            previous_discount=effective_discount,
            actual_repay_amount=actual_repay_amount,
        )

        # Apply the calculated balance delta
        debt_position.balance += gho_result.balance_delta
        update_debt_position_index(
            tx_context=tx_context,
            debt_asset=debt_asset,
            debt_position=debt_position,
            event_index=scaled_event.index,
            event_block_number=scaled_event.event["blockNumber"],
        )

        # Refresh discount if needed
        if (
            gho_result.should_refresh_discount
            and tx_context.gho_asset.v_gho_discount_token is not None
        ):
            discount_token_balance = get_or_init_stk_aave_balance(
                user=user,
                tx_context=tx_context,
                log_index=scaled_event.event["logIndex"],
            )
            assert debt_position.last_index is not None
            _refresh_discount_rate(
                user=user,
                discount_token_balance=discount_token_balance,
                scaled_debt_balance=debt_position.balance,
                debt_index=debt_position.last_index,
                wad_ray_math=gho_processor.get_math_libraries()["wad_ray"],
            )
    else:
        # Use standard debt processor for non-GHO tokens
        assert scaled_event.balance_increase is not None
        assert scaled_event.index is not None

        # Check if this Mint event is part of a REPAY or LIQUIDATION operation
        # In REPAY/LIQUIDATION, Mint is emitted when interest > repayment, but the net effect
        # is still a burn of scaled tokens
        if operation.operation_type in {
            OperationType.GHO_LIQUIDATION,
            OperationType.GHO_REPAY,
            OperationType.LIQUIDATION,
            OperationType.REPAY_WITH_ATOKENS,
            OperationType.REPAY,
        }:
            # For liquidations, check pattern to determine if Mint events should be skipped.
            # COMBINED_BURN (Issue 0056): Multiple liquidations share one burn event.
            #   Skip Mint events - the aggregated burn handles all debt reduction.
            # SEPARATE_BURNS (Issue 0065): Each liquidation has its own burn event.
            #   Process Mint events normally as they represent individual liquidations.
            # SINGLE: Standard single liquidation, process Mint normally.
            if operation.operation_type in {
                OperationType.LIQUIDATION,
                OperationType.GHO_LIQUIDATION,
            }:
                liquidation_key = (user.address, token_address)
                pattern = tx_context.liquidation_patterns.get_pattern(user.address, token_address)

                # Only skip Mint events for COMBINED_BURN pattern
                if pattern == LiquidationPattern.COMBINED_BURN:
                    logger.debug(
                        f"_process_debt_mint_with_match: COMBINED_BURN pattern - "
                        f"skipping Mint event for {liquidation_key} (handled by aggregated burn)"
                    )
                    return
                # For SINGLE and SEPARATE_BURNS, process the Mint event normally

            # Treat as burn: calculate actual scaled burn amount from Pool event
            # Use TokenMath to match on-chain calculation
            assert operation.pool_event is not None

            # Decode the amount based on operation type
            # REPAY: (uint256 amount, bool useATokens)
            # LIQUIDATION: (uint256 debtToCover, uint256 liquidatedCollateralAmount,
            #              address liquidator, bool receiveAToken)
            if operation.operation_type in {
                OperationType.GHO_REPAY,
                OperationType.REPAY_WITH_ATOKENS,
                OperationType.REPAY,
            }:
                repay_amount, _ = eth_abi.abi.decode(
                    types=["uint256", "bool"],
                    data=operation.pool_event["data"],
                )
            elif operation.operation_type in {
                OperationType.GHO_LIQUIDATION,
                OperationType.LIQUIDATION,
            }:
                repay_amount, _, _, _ = eth_abi.abi.decode(
                    types=["uint256", "uint256", "address", "bool"],
                    data=operation.pool_event["data"],
                )
            else:
                assert_never(operation.operation_type)

            # Use token revision (not pool revision) to get correct TokenMath
            token_math = TokenMathFactory.get_token_math_for_token_revision(
                debt_asset.v_token_revision
            )
            actual_scaled_burn = token_math.get_debt_burn_scaled_amount(
                repay_amount, scaled_event.index
            )
            logger.debug(
                f"{operation.operation_type.name} with Mint event: treating as burn, "
                f"amount={repay_amount}, scaled_burn={actual_scaled_burn}"
            )
            _process_scaled_token_operation(
                event=DebtBurnEvent(
                    from_=scaled_event.user_address,
                    target=ZERO_ADDRESS,
                    value=actual_scaled_burn,
                    balance_increase=scaled_event.balance_increase,
                    index=scaled_event.index,
                    scaled_amount=actual_scaled_burn,  # Pass the correctly calculated scaled burn
                ),
                scaled_token_revision=debt_asset.v_token_revision,
                position=debt_position,
            )
        else:
            logger.debug("_process_debt_mint_with_match: handling as borrow/mint")
            _process_scaled_token_operation(
                event=DebtMintEvent(
                    caller=scaled_event.caller_address or scaled_event.user_address,
                    on_behalf_of=scaled_event.user_address,
                    value=scaled_event.amount,
                    balance_increase=scaled_event.balance_increase,
                    index=scaled_event.index,
                    scaled_amount=scaled_amount,
                ),
                scaled_token_revision=debt_asset.v_token_revision,
                position=debt_position,
            )

        update_debt_position_index(
            tx_context=tx_context,
            debt_asset=debt_asset,
            debt_position=debt_position,
            event_index=scaled_event.index,
            event_block_number=scaled_event.event["blockNumber"],
        )


def _is_bad_debt_liquidation(user: "AaveV3User", tx_context: TransactionContext) -> bool:
    """
    Check if this transaction contains a bad debt liquidation for the user.

    Bad debt liquidations emit a DEFICIT_CREATED event for the user, indicating
    the protocol is writing off debt that cannot be covered by collateral.

    Event definition:
        event DeficitCreated(
            address indexed user,
            address indexed debtAsset,
            uint256 amountCreated
        );
    """

    for evt in tx_context.events:
        if evt["topics"][0] == AaveV3PoolEvent.DEFICIT_CREATED.value:
            deficit_user = decode_address(evt["topics"][1])

            if deficit_user == user.address:
                return True
    return False


def _process_debt_burn_with_match(
    *,
    event: LogReceipt,
    tx_context: TransactionContext,
    operation: Operation,
    scaled_event: ScaledTokenEvent,
    enriched_event: EnrichedScaledTokenEvent,
) -> None:
    """
    Process debt (vToken) burn with operation match.
    """

    # Get debt asset first for logging
    token_address = scaled_event.event["address"]
    debt_asset = get_asset_by_token_type(
        session=tx_context.session,
        market=tx_context.market,
        token_address=token_address,
        token_type=TokenType.V_TOKEN,
    )

    assert debt_asset is not None

    asset_identifier = get_asset_identifier(debt_asset)
    logger.debug(
        f"[Pool rev {tx_context.pool_revision}] Processing {asset_identifier} debt burn "
        f"at block {event['blockNumber']}"
    )

    user = get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.user_address,
        block_number=scaled_event.event["blockNumber"],
    )

    # Get debt position
    debt_position = get_or_create_debt_position(
        tx_context=tx_context,
        user=user,
        asset_id=debt_asset.id,
    )

    # Use enriched event data for scaled amount
    scaled_amount: int | None = enriched_event.scaled_amount

    # Check for bad debt liquidation first - applies to both GHO and non-GHO tokens
    # Bad debt liquidations emit a DEFICIT_CREATED event and burn the FULL debt balance
    if (
        operation
        and operation.operation_type in {OperationType.LIQUIDATION, OperationType.GHO_LIQUIDATION}
        and _is_bad_debt_liquidation(user, tx_context)
    ):
        # Bad debt liquidation: The contract burns the ENTIRE debt balance
        # not just the debtToCover amount. The debt position should be set to 0.
        # This is because the protocol writes off the bad debt.
        old_balance = debt_position.balance
        debt_position.balance = 0
        logger.debug(
            f"_process_debt_burn_with_match: BAD DEBT LIQUIDATION - setting balance to 0 "
            f"(was {old_balance})"
        )

        # Only update last_index if the new index is greater than current
        current_index = debt_position.last_index or 0
        if scaled_event.index > current_index:
            debt_position.last_index = scaled_event.index

        return

    # Check if this is a GHO token and use GHO-specific processing
    if tx_context.is_gho_vtoken(token_address):
        # Use the effective discount from transaction context
        effective_discount = tx_context.user_discounts.get(user.address, user.gho_discount)

        # Process using GHO-specific processor
        gho_processor = TokenProcessorFactory.get_gho_debt_processor(debt_asset.v_token_revision)
        assert scaled_event.balance_increase is not None
        assert scaled_event.index is not None
        gho_result = gho_processor.process_burn_event(
            event_data=DebtBurnEvent(
                from_=scaled_event.from_address or scaled_event.user_address,
                target=scaled_event.target_address or scaled_event.user_address,
                value=scaled_event.amount,
                balance_increase=scaled_event.balance_increase,
                index=scaled_event.index,
                scaled_amount=scaled_amount,
            ),
            previous_balance=debt_position.balance,
            previous_index=debt_position.last_index or 0,
            previous_discount=effective_discount,
        )

        # Apply the calculated balance delta
        debt_position.balance += gho_result.balance_delta
        update_debt_position_index(
            tx_context=tx_context,
            debt_asset=debt_asset,
            debt_position=debt_position,
            event_index=scaled_event.index,
            event_block_number=scaled_event.event["blockNumber"],
        )

        # Refresh discount if needed
        if (
            gho_result.should_refresh_discount
            and tx_context.gho_asset.v_gho_discount_token is not None
        ):
            discount_token_balance = get_or_init_stk_aave_balance(
                user=user,
                tx_context=tx_context,
                log_index=scaled_event.event["logIndex"],
            )
            assert debt_position.last_index is not None
            current_index = debt_position.last_index
            _refresh_discount_rate(
                user=user,
                discount_token_balance=discount_token_balance,
                scaled_debt_balance=debt_position.balance,
                debt_index=current_index,
                wad_ray_math=gho_processor.get_math_libraries()["wad_ray"],
            )
    else:
        # Use standard debt processor for non-GHO tokens
        assert scaled_event.balance_increase is not None
        assert scaled_event.index is not None
        logger.debug("_process_debt_burn_with_match: handling with standard debt processor")
        logger.debug(f"_process_debt_burn_with_match: scaled_event.amount = {scaled_event.amount}")
        logger.debug(
            f"_process_debt_burn_with_match: scaled_event.balance_increase = "
            f"{scaled_event.balance_increase}"
        )
        logger.debug(f"_process_debt_burn_with_match: scaled_event.index = {scaled_event.index}")

        if operation and operation.operation_type in {
            OperationType.LIQUIDATION,
            OperationType.GHO_LIQUIDATION,
        }:
            pattern = tx_context.liquidation_patterns.get_pattern(user.address, token_address)

            if pattern == LiquidationPattern.SINGLE:
                # Standard single liquidation - use operation's debt_to_cover
                assert operation.debt_to_cover is not None
                debt_to_cover = operation.debt_to_cover

                token_math = TokenMathFactory.get_token_math_for_token_revision(
                    debt_asset.v_token_revision
                )
                burn_value = token_math.get_debt_burn_scaled_amount(
                    debt_to_cover, scaled_event.index
                )
                scaled_amount = burn_value

                logger.debug(
                    f"_process_debt_burn_with_match: SINGLE liquidation using "
                    f"debtToCover={debt_to_cover}, scaled_burn={burn_value}"
                )

            elif pattern == LiquidationPattern.COMBINED_BURN:
                tx_context.liquidation_patterns.mark_processed(user.address, token_address)

                # Get aggregated amount from group
                group = tx_context.liquidation_patterns.get_group(user.address, token_address)
                assert group is not None
                total_debt = group.total_debt_to_cover

                token_math = TokenMathFactory.get_token_math_for_token_revision(
                    debt_asset.v_token_revision
                )
                burn_value = token_math.get_debt_burn_scaled_amount(total_debt, scaled_event.index)
                scaled_amount = burn_value

                logger.debug(
                    f"_process_debt_burn_with_match: COMBINED_BURN ({group.liquidation_count}x) "
                    f"using aggregated debtToCover={total_debt}, scaled_burn={burn_value}"
                )

            elif pattern == LiquidationPattern.SEPARATE_BURNS:
                # Issue 0065: Each liquidation has its own burn event
                # Process each burn individually using operation's debt_to_cover
                assert operation.debt_to_cover is not None
                debt_to_cover = operation.debt_to_cover

                token_math = TokenMathFactory.get_token_math_for_token_revision(
                    debt_asset.v_token_revision
                )
                burn_value = token_math.get_debt_burn_scaled_amount(
                    debt_to_cover, scaled_event.index
                )
                scaled_amount = burn_value

                logger.debug(
                    f"_process_debt_burn_with_match: SEPARATE_BURNS using "
                    f"debtToCover={debt_to_cover}, scaled_burn={burn_value}"
                )

            else:
                assert_never(pattern)

        else:
            # Standard REPAY: use Burn event value
            burn_value = scaled_event.amount
            logger.debug(f"_process_debt_burn_with_match: REPAY - using burn_value={burn_value}")

        _process_scaled_token_operation(
            event=DebtBurnEvent(
                from_=scaled_event.from_address or scaled_event.user_address,
                target=scaled_event.target_address or scaled_event.user_address,
                value=burn_value,
                balance_increase=scaled_event.balance_increase,
                index=scaled_event.index,
                scaled_amount=scaled_amount,
            ),
            scaled_token_revision=debt_asset.v_token_revision,
            position=debt_position,
        )

        update_debt_position_index(
            tx_context=tx_context,
            debt_asset=debt_asset,
            debt_position=debt_position,
            event_index=scaled_event.index,
            event_block_number=scaled_event.event["blockNumber"],
        )
