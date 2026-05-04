"""
Transaction processing orchestration for Aave V3.

This module contains the main transaction processing orchestrator that handles:
- Pre-processing discount updates
- Pre-fetching GHO users
- Parsing operations
- Processing stkAAVE transfers
- Processing operations
- Processing deferred burns
- Processing remaining events
"""

from operator import itemgetter
from typing import TYPE_CHECKING

import eth_abi.abi
from sqlalchemy import select

from degenbot.aave.events import (
    AaveV3GhoDebtTokenEvent,
    AaveV3OracleEvent,
    AaveV3PoolConfigEvent,
    AaveV3PoolEvent,
    AaveV3ScaledTokenEvent,
    ERC20Event,
    ScaledTokenEventType,
)
from degenbot.aave.operation_types import OperationType
from degenbot.aave.pipeline import EventPipeline, PositionContext
from degenbot.aave.types import Operation
from degenbot.aave.utils import decode_address
from degenbot.cli.aave.constants import LIQUIDATION_OPERATION_TYPES
from degenbot.cli.aave.db_assets import get_asset_by_token_type, get_contract
from degenbot.cli.aave.db_positions import (
    get_or_create_collateral_position,
    get_or_create_debt_position,
)
from degenbot.cli.aave.db_users import get_or_create_user, is_discount_supported
from degenbot.cli.aave.event_handlers import (
    _process_address_set_event,
    _process_asset_source_updated_event,
    _process_discount_percent_updated_event,
    _process_discount_rate_strategy_updated_event,
    _process_discount_token_updated_event,
    _process_pool_data_provider_updated_event,
    _process_price_oracle_updated_event,
    _process_reserve_data_update_event,
    _process_reserve_used_as_collateral_disabled_event,
    _process_reserve_used_as_collateral_enabled_event,
    _process_scaled_token_upgrade_event,
    _process_user_e_mode_set_event,
    _update_contract_revision,
)
from degenbot.cli.aave.liquidation_processor import (
    _preprocess_liquidation_aggregates,
    _process_deferred_debt_burns,
)
from degenbot.cli.aave.stkaave import get_or_init_stk_aave_balance, process_stk_aave_transfer_event
from degenbot.cli.aave.token_processor import (
    _is_bad_debt_liquidation,
    _process_debt_burn_with_match,
    _process_deficit_coverage_operation,
    _refresh_discount_rate,
)
from degenbot.cli.aave.transfers import _process_collateral_transfer
from degenbot.cli.aave.types import TokenType, TransactionContext
from degenbot.cli.aave.verification import update_debt_position_index
from degenbot.cli.aave_transaction_operations import TransactionOperationsParser
from degenbot.database.models.aave import AaveV3User
from degenbot.functions import encode_function_calldata, raw_call
from degenbot.logging import logger

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.aave.types import ScaledTokenEvent


def _process_transaction(tx_context: TransactionContext) -> None:
    """
    Process transaction using operation-based parsing.
    """

    # Cache GHO vToken address for reuse
    gho_vtoken_address = tx_context.gho_vtoken_address

    # First, build a map of discount updates in this transaction to get old values
    # Track all updates with their log indices to handle multiple updates per transaction
    for event in tx_context.events:
        topic = event["topics"][0]
        if topic == AaveV3GhoDebtTokenEvent.DISCOUNT_PERCENT_UPDATED.value:
            user_address = decode_address(event["topics"][1])
            (old_discount_percent,) = eth_abi.abi.decode(types=["uint256"], data=event["data"])
            if user_address not in tx_context.discount_updates_by_log_index:
                tx_context.discount_updates_by_log_index[user_address] = []
            tx_context.discount_updates_by_log_index[user_address].append((
                event["logIndex"],
                old_discount_percent,
            ))

    # Sort updates by log index for each user
    for user_address in tx_context.discount_updates_by_log_index:
        tx_context.discount_updates_by_log_index[user_address].sort(key=itemgetter(0))

    # Pre-fetch all users from GHO mint/burn events to avoid N+1 queries
    gho_user_addresses: set[ChecksumAddress] = set()
    for event in tx_context.events:
        topic = event["topics"][0]
        event_address = event["address"]

        if (
            topic
            in {
                AaveV3ScaledTokenEvent.MINT.value,
                AaveV3ScaledTokenEvent.BURN.value,
            }
            and gho_vtoken_address is not None
            and event_address == gho_vtoken_address
        ):
            # Mint event: topics[1] = caller, topics[2] = onBehalfOf (user)
            # Burn event: topics[1] = from (user), topics[2] = target
            if topic == AaveV3ScaledTokenEvent.MINT.value:
                user_address = decode_address(event["topics"][2])
            else:  # SCALED_TOKEN_BURN
                user_address = decode_address(event["topics"][1])
            gho_user_addresses.add(user_address)

    # Fetch all GHO users in a single query
    gho_users = (
        {
            user.address: user
            for user in tx_context.session.scalars(
                select(AaveV3User).where(
                    AaveV3User.address.in_(gho_user_addresses),
                    AaveV3User.market_id == tx_context.market.id,
                )
            )
        }
        if gho_user_addresses
        else {}
    )

    # Capture user discount percents before processing events
    # This ensures calculations use the discount in effect at the start of the transaction
    for event in tx_context.events:
        topic = event["topics"][0]
        event_address = event["address"]

        # Capture GHO user discount percents for mint/burn events
        if (
            topic
            in {
                AaveV3ScaledTokenEvent.MINT.value,
                AaveV3ScaledTokenEvent.BURN.value,
            }
            and gho_vtoken_address is not None
            and event_address == gho_vtoken_address
        ):
            # Mint event: topics[1] = caller, topics[2] = onBehalfOf (user)
            # Burn event: topics[1] = from (user), topics[2] = target
            if topic == AaveV3ScaledTokenEvent.MINT.value:
                user_address = decode_address(event["topics"][2])
            else:  # SCALED_TOKEN_BURN
                user_address = decode_address(event["topics"][1])
            if user_address not in tx_context.user_discounts:
                # If there are DiscountPercentUpdated events for this user in this
                # transaction, use the OLD discount value that was in effect at the
                # start of the transaction (before any updates in this tx)
                user = gho_users.get(user_address)
                if user is not None:
                    # Get discount that was in effect at this specific log index
                    tx_context.user_discounts[user_address] = (
                        tx_context.get_effective_discount_at_log_index(
                            user_address=user_address,
                            log_index=event["logIndex"],
                            default_discount=user.gho_discount,
                        )
                    )
                    continue

                # User doesn't exist in database yet - fetch discount from contract
                # This happens when a user with an existing GHO debt position
                # is first encountered during event processing
                if gho_vtoken_address is None or not is_discount_supported(
                    session=tx_context.session,
                    market=tx_context.market,
                ):
                    # Discount mechanism not available (no vToken or revision 4+)
                    tx_context.user_discounts[user_address] = 0
                    continue

                (discount_percent,) = raw_call(
                    provider=tx_context.provider,
                    address=gho_vtoken_address,
                    calldata=encode_function_calldata(
                        function_prototype="getDiscountPercent(address)",
                        function_arguments=[user_address],
                    ),
                    return_types=["uint256"],
                    block_identifier=tx_context.block_number,
                )
                tx_context.user_discounts[user_address] = discount_percent

    logger.debug(
        f"[Pool rev {tx_context.pool_revision}] Processing transaction at block "
        f"{tx_context.block_number}"
    )

    # Parse events into operations
    pool_contract = get_contract(
        session=tx_context.session,
        market=tx_context.market,
        contract_name="POOL",
    )
    assert pool_contract is not None

    parser = TransactionOperationsParser(
        market=tx_context.market,
        session=tx_context.session,
        pool_address=pool_contract.address,
    )
    tx_operations = parser.parse(
        events=tx_context.events,
        tx_hash=tx_context.tx_hash,
    )

    # Strict validation - fail immediately on any issue
    tx_operations.validate(tx_context.events)

    logger.debug(f"\n=== OPERATIONS FOR TX {tx_context.tx_hash.to_0x_hex()} ===")
    for op in tx_operations.operations:
        logger.debug(f"Operation {op.operation_id}: {op.operation_type.name}")
        if op.pool_event:
            logger.debug(f"  Pool event: logIndex={op.pool_event['logIndex']}")
        for scaled_ev in op.scaled_token_events:
            logger.debug(
                f"  Scaled event: logIndex={scaled_ev.event['logIndex']}, "
                f"type={scaled_ev.event_type}, user={scaled_ev.user_address}, "
                f"amount={scaled_ev.amount}, index={scaled_ev.index}"
            )
        for transfer_ev in op.transfer_events:
            logger.debug(f"  Transfer event: logIndex={transfer_ev['logIndex']}")
        for balance_transfer_ev in op.balance_transfer_events:
            logger.debug(f"  BalanceTransfer event: logIndex={balance_transfer_ev['logIndex']}")
    logger.debug("=== END OPERATIONS ===\n")

    # Collect all scaled token events from operations for pattern detection
    tx_context.scaled_token_events = [
        scaled_ev for op in tx_operations.operations for scaled_ev in op.scaled_token_events
    ]

    # Preprocess liquidations to detect patterns for multi-liquidation scenarios
    # This handles both combined burn (Issue 0056) and separate burns (Issue 0065) patterns.
    _preprocess_liquidation_aggregates(tx_context, tx_operations.operations)

    # Process stkAAVE transfers BEFORE operations to ensure stkAAVE balances
    # are up-to-date when GHO debt operations calculate discount rates.
    # This handles cases where stkAAVE transfers (e.g., rewards claims) occur
    # before GHO mint/burn events in the same transaction.
    if tx_context.gho_asset and tx_context.gho_asset.v_gho_discount_token:
        discount_token = tx_context.gho_asset.v_gho_discount_token
        for event in tx_context.events:
            topic = event["topics"][0]
            event_address = event["address"]
            if topic == ERC20Event.TRANSFER.value and event_address == discount_token:
                process_stk_aave_transfer_event(
                    event=event,
                    contract_address=event_address,
                    tx_context=tx_context,
                )

    # Pre-process WITHDRAW operations to extract withdrawAmounts before processing scaled events.
    # This ensures INTEREST_ACCRUAL collateral burns can use the original withdrawAmount to avoid
    # 1 wei rounding errors from reverse-calculating from Burn event values.
    # Note: REPAY paybackAmounts are now extracted during operation processing via extraction_data
    for operation in tx_operations.operations:
        if (
            operation.operation_type == OperationType.WITHDRAW
            and operation.pool_event is not None
            and operation.pool_event.get("data")
        ):
            decoded = eth_abi.abi.decode(["uint256"], operation.pool_event["data"])
            tx_context.last_withdraw_amount = decoded[0]
            # Store the token and user addresses for matching with INTEREST_ACCRUAL burns
            assert operation.scaled_token_events
            first_event = operation.scaled_token_events[0]
            tx_context.last_withdraw_token_address = first_event.event["address"]
            tx_context.last_withdraw_user_address = first_event.user_address
            logger.debug(
                f"Pre-processed WITHDRAW amount: {tx_context.last_withdraw_amount} "
                f"for operation {operation.operation_id}"
            )

    # Process each operation in chronological order (sorted by pool event or minimum scaled event
    # log index)
    # This ensures events are processed in the order they appear in the transaction.
    # Operations with pool events are sorted by pool event log index.
    # Operations without pool events (INTEREST_ACCRUAL, etc.) are sorted by minimum scaled event
    # log index.
    def _get_operation_sort_key(op: Operation) -> int:
        if op.pool_event is not None:
            # Use pool event log index for operations with pool events
            return op.pool_event["logIndex"]

        # Use minimum scaled event log index for operations without pool events
        assert op.scaled_token_events
        return min(ev.event["logIndex"] for ev in op.scaled_token_events)

    sorted_operations = sorted(tx_operations.operations, key=_get_operation_sort_key)
    for operation in sorted_operations:
        logger.debug(
            f"Processing operation {operation.operation_id}: {operation.operation_type.name}"
        )
        _process_operation(
            operation=operation,
            tx_context=tx_context,
        )

    # Build a map of assigned log indices and liquidation operations for deferred processing
    # This handles cases where debt burn events are emitted BEFORE LiquidationCall events
    # See debug/aave/0060 for details on out-of-order event emission
    assigned_log_indices: set[int] = set()
    liquidation_operations: list[Operation] = []
    for op in tx_operations.operations:
        assigned_log_indices.update(
            scaled_ev.event["logIndex"] for scaled_ev in op.scaled_token_events
        )
        assigned_log_indices.update(transfer_ev["logIndex"] for transfer_ev in op.transfer_events)
        assigned_log_indices.update(
            balance_transfer_ev["logIndex"] for balance_transfer_ev in op.balance_transfer_events
        )
        if op.pool_event:
            assigned_log_indices.add(op.pool_event["logIndex"])
        # Track liquidation operations for deferred burn matching
        if op.operation_type in LIQUIDATION_OPERATION_TYPES:
            liquidation_operations.append(op)

    # Process deferred debt burns that couldn't be matched during initial parsing
    # This handles the case where Burn events are emitted before LiquidationCall events
    _process_deferred_debt_burns(
        tx_context=tx_context,
        liquidation_operations=liquidation_operations,
        assigned_log_indices=assigned_log_indices,
    )

    for event in tx_context.events:
        if event["logIndex"] in assigned_log_indices:
            continue

        topic = event["topics"][0]
        event_address = event["address"]

        # Dispatch to appropriate handler for non-operation events
        if topic == AaveV3PoolEvent.RESERVE_DATA_UPDATED.value:
            _process_reserve_data_update_event(
                session=tx_context.session,
                event=event,
                market=tx_context.market,
            )
        elif topic == AaveV3PoolEvent.USER_E_MODE_SET.value:
            _process_user_e_mode_set_event(
                event=event,
                tx_context=tx_context,
            )
        elif topic == AaveV3PoolConfigEvent.POOL_UPDATED.value:
            _update_contract_revision(
                session=tx_context.session,
                provider=tx_context.provider,
                market=tx_context.market,
                contract_name="POOL",
                new_address=decode_address(event["topics"][2]),
                revision_function_prototype="POOL_REVISION",
            )
            pool_contract = get_contract(
                session=tx_context.session,
                market=tx_context.market,
                contract_name="POOL",
            )
            assert pool_contract is not None
            assert pool_contract.revision is not None
            tx_context.pool_revision = pool_contract.revision
        elif topic == AaveV3PoolConfigEvent.POOL_CONFIGURATOR_UPDATED.value:
            _update_contract_revision(
                session=tx_context.session,
                provider=tx_context.provider,
                market=tx_context.market,
                contract_name="POOL_CONFIGURATOR",
                new_address=decode_address(event["topics"][2]),
                revision_function_prototype="CONFIGURATOR_REVISION",
            )
        elif topic == AaveV3PoolConfigEvent.POOL_DATA_PROVIDER_UPDATED.value:
            _process_pool_data_provider_updated_event(
                session=tx_context.session,
                market=tx_context.market,
                event=event,
            )
        elif topic == AaveV3PoolConfigEvent.ADDRESS_SET.value:
            _process_address_set_event(
                session=tx_context.session,
                market=tx_context.market,
                event=event,
            )
        elif topic == AaveV3PoolConfigEvent.PRICE_ORACLE_UPDATED.value:
            _process_price_oracle_updated_event(
                session=tx_context.session,
                market=tx_context.market,
                event=event,
            )
        elif topic == AaveV3OracleEvent.ASSET_SOURCE_UPDATED.value:
            _process_asset_source_updated_event(
                session=tx_context.session,
                market=tx_context.market,
                event=event,
            )
        elif topic == AaveV3PoolConfigEvent.UPGRADED.value:
            _process_scaled_token_upgrade_event(
                event=event,
                tx_context=tx_context,
            )
        elif topic == AaveV3GhoDebtTokenEvent.DISCOUNT_PERCENT_UPDATED.value:
            _process_discount_percent_updated_event(
                event=event,
                tx_context=tx_context,
            )
        elif topic == AaveV3GhoDebtTokenEvent.DISCOUNT_TOKEN_UPDATED.value:
            _process_discount_token_updated_event(
                event=event,
                gho_asset=tx_context.gho_asset,
            )
        elif topic == AaveV3GhoDebtTokenEvent.DISCOUNT_RATE_STRATEGY_UPDATED.value:
            _process_discount_rate_strategy_updated_event(
                event=event,
                gho_asset=tx_context.gho_asset,
            )
        elif topic == AaveV3PoolEvent.RESERVE_USED_AS_COLLATERAL_ENABLED.value:
            _process_reserve_used_as_collateral_enabled_event(
                session=tx_context.session,
                event=event,
                market_id=tx_context.market.id,
            )
        elif topic == AaveV3PoolEvent.RESERVE_USED_AS_COLLATERAL_DISABLED.value:
            _process_reserve_used_as_collateral_disabled_event(
                session=tx_context.session,
                event=event,
                market_id=tx_context.market.id,
            )


def _process_operation(
    *,
    operation: Operation,
    tx_context: TransactionContext,
) -> None:
    """
    Process a single operation.

    Uses EventPipeline for extraction, enrichment, and processor dispatch.
    Falls back to specialized handlers for liquidation debt burns (pattern logic)
    and collateral transfers (sender + recipient handling).
    """

    logger.debug(
        f"[Pool rev {tx_context.pool_revision}] Processing operation {operation.operation_id}: "
        f"{operation.operation_type.name}"
    )

    if operation.operation_type == OperationType.STKAAVE_TRANSFER:
        return

    if operation.operation_type == OperationType.DEFICIT_COVERAGE:
        _process_deficit_coverage_operation(
            operation=operation,
            tx_context=tx_context,
        )
        return

    pipeline = EventPipeline(
        pool_revision=tx_context.pool_revision,
        token_revisions={},
    )

    sorted_scaled_events = sorted(
        operation.scaled_token_events,
        key=lambda e: e.event["logIndex"],
    )
    for scaled_event in sorted_scaled_events:
        event_type = scaled_event.event_type

        if event_type in {
            ScaledTokenEventType.COLLATERAL_TRANSFER,
            ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
        }:
            _process_collateral_transfer(
                tx_context=tx_context,
                operation=operation,
                scaled_event=scaled_event,
            )
            continue

        # Liquidation debt burns need pattern-based logic not in the pipeline
        if (
            event_type in {ScaledTokenEventType.DEBT_BURN, ScaledTokenEventType.GHO_DEBT_BURN}
            and operation.operation_type in LIQUIDATION_OPERATION_TYPES
        ):
            _process_debt_burn_with_match(
                event=scaled_event.event,
                tx_context=tx_context,
                operation=operation,
                scaled_event=scaled_event,
                enriched_event=None,
            )
            continue

        _process_scaled_event_via_pipeline(
            pipeline=pipeline,
            scaled_event=scaled_event,
            operation=operation,
            tx_context=tx_context,
        )


def _process_scaled_event_via_pipeline(
    *,
    pipeline: EventPipeline,
    scaled_event: "ScaledTokenEvent",
    operation: Operation,
    tx_context: TransactionContext,
) -> None:
    """Process a single scaled event through the pipeline and apply the delta to DB."""
    token_address = scaled_event.event["address"]
    is_collateral = scaled_event.event_type in {
        ScaledTokenEventType.COLLATERAL_MINT,
        ScaledTokenEventType.COLLATERAL_BURN,
        ScaledTokenEventType.COLLATERAL_TRANSFER,
        ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
    }
    is_gho = tx_context.is_gho_vtoken(token_address)

    if is_collateral:
        asset = get_asset_by_token_type(
            session=tx_context.session,
            market=tx_context.market,
            token_address=token_address,
            token_type=TokenType.A_TOKEN,
        )
        assert asset is not None
        user = get_or_create_user(
            tx_context=tx_context,
            user_address=scaled_event.user_address,
            block_number=scaled_event.event["blockNumber"],
        )
        position = get_or_create_collateral_position(
            tx_context=tx_context,
            user=user,
            asset_id=asset.id,
        )
        token_revision = asset.a_token_revision
        is_bad_debt = False
    else:
        asset = get_asset_by_token_type(
            session=tx_context.session,
            market=tx_context.market,
            token_address=token_address,
            token_type=TokenType.V_TOKEN,
        )
        assert asset is not None
        user = get_or_create_user(
            tx_context=tx_context,
            user_address=scaled_event.user_address,
            block_number=scaled_event.event["blockNumber"],
        )
        position = get_or_create_debt_position(
            tx_context=tx_context,
            user=user,
            asset_id=asset.id,
        )
        token_revision = asset.v_token_revision
        is_bad_debt = operation.operation_type in {
            OperationType.LIQUIDATION,
            OperationType.GHO_LIQUIDATION,
        } and _is_bad_debt_liquidation(user, tx_context)

    previous_discount = 0
    if is_gho:
        previous_discount = tx_context.user_discounts.get(user.address, user.gho_discount)

    position_context = PositionContext(
        previous_balance=position.balance,
        previous_index=position.last_index or 0,
        previous_discount=previous_discount,
        token_revision=token_revision,
        pool_revision=tx_context.pool_revision,
        is_gho=is_gho,
        is_bad_debt=is_bad_debt,
    )

    delta = pipeline.process(scaled_event, operation, position_context)

    # Apply PositionDelta to DB position
    if delta.set_balance_to_zero:
        position.balance = 0
    else:
        position.balance += delta.balance_delta

    if delta.new_index > (position.last_index or 0):
        position.last_index = delta.new_index

    # GHO discount refresh
    if (
        delta.should_refresh_discount
        and tx_context.gho_asset is not None
        and tx_context.gho_asset.v_gho_discount_token is not None
    ):
        discount_token_balance = get_or_init_stk_aave_balance(
            user=user,
            tx_context=tx_context,
            log_index=scaled_event.event["logIndex"],
        )
        assert position.last_index is not None
        _refresh_discount_rate(
            user=user,
            discount_token_balance=discount_token_balance,
            scaled_debt_balance=position.balance,
            debt_index=position.last_index,
            wad_ray_math=None,
        )

    # Debt position index update for verification
    if not is_collateral:
        update_debt_position_index(
            tx_context=tx_context,
            debt_reserve=asset,
            debt_position=position,
            event_index=scaled_event.index,
            event_block_number=scaled_event.event["blockNumber"],
        )
