"""
Liquidation processing functions for Aave V3.

This module handles liquidation event processing including multi-liquidation patterns
like COMBINED_BURN and SEPARATE_BURNS.
"""

import eth_abi.abi
from eth_typing import ChecksumAddress
from web3.types import LogReceipt

from degenbot.aave.enrichment import ScaledEventEnricher
from degenbot.aave.events import AaveV3ScaledTokenEvent
from degenbot.aave.models import EnrichedScaledTokenEvent
from degenbot.cli.aave.db_assets import get_asset_by_token_type
from degenbot.cli.aave.types import TokenType, TransactionContext
from degenbot.cli.aave.utils import _get_v_token_for_underlying
from degenbot.cli.aave_transaction_operations import (
    Operation,
    ScaledTokenEvent,
    ScaledTokenEventType,
)
from degenbot.cli.aave_utils import decode_address
from degenbot.logging import logger


def _preprocess_liquidation_aggregates(
    tx_context: TransactionContext,
    operations: list["Operation"],
) -> None:
    """
    Preprocess liquidations to detect patterns and prepare for processing.

    Detects whether multiple liquidations share the same debt asset and
    determines if they use combined or separate burn events.

    See debug/aave/0056 and debug/aave/0065 for pattern details.
    """
    from degenbot.aave.liquidation_patterns import detect_liquidation_patterns

    tx_context.liquidation_patterns = detect_liquidation_patterns(
        operations=operations,
        scaled_token_events=tx_context.scaled_token_events,
        get_v_token_for_underlying=lambda addr: _get_v_token_for_underlying(
            session=tx_context.session,
            market=tx_context.market,
            underlying_address=addr,
        ),
    )


def _process_deferred_debt_burns(
    *,
    tx_context: TransactionContext,
    liquidation_operations: list[Operation],
    assigned_log_indices: set[int],
) -> None:
    """
    Process debt burns that couldn't be matched during initial operation parsing.

    This handles the case where Burn events are emitted BEFORE LiquidationCall events
    in Aave V3. The protocol emits events in this order:
    1. Reserve state update
    2. Debt token burn
    3. Collateral token operations
    4. LiquidationCall event (at the end)

    Since operations are parsed from Pool events, the burn event may not be matched
    to a liquidation if the burn has a lower log index than the LiquidationCall.

    This function finds unassigned debt burns and matches them to liquidation operations
    retrospectively using semantic matching (user + debt asset).

    See debug/aave/0060 for detailed analysis.
    """
    from degenbot.cli.aave.token_processor import _process_debt_burn_with_match

    if not liquidation_operations:
        return

    # Find all unassigned debt burn events
    for event in tx_context.events:
        if event["logIndex"] in assigned_log_indices:
            continue

        topic = event["topics"][0]
        event_address = event["address"]

        # Check if this is a debt burn event
        if topic != AaveV3ScaledTokenEvent.BURN.value:
            continue

        # Decode burn event to get user and amount
        from_addr = decode_address(event["topics"][1])
        target = decode_address(event["topics"][2])
        amount, balance_increase, index = eth_abi.abi.decode(
            types=["uint256", "uint256", "uint256"],
            data=event["data"],
        )

        # Find matching liquidation operation
        matching_operation = _find_matching_liquidation_for_burn(
            user_address=from_addr,
            burn_token_address=event_address,
            liquidation_operations=liquidation_operations,
            tx_context=tx_context,
        )

        if matching_operation is None:
            continue

        # Create scaled event from the burn
        scaled_event = ScaledTokenEvent(
            event=event,
            event_type=ScaledTokenEventType.DEBT_BURN,
            user_address=from_addr,
            caller_address=None,
            from_address=from_addr,
            target_address=target,
            amount=amount,
            balance_increase=balance_increase,
            index=index,
        )

        # Enrich and process the burn
        enricher = ScaledEventEnricher(
            pool_revision=tx_context.pool_revision,
            token_revisions={},
            session=tx_context.session,
        )
        enriched_event = enricher.enrich(scaled_event, matching_operation)

        pool_log_idx = (
            matching_operation.pool_event["logIndex"] if matching_operation.pool_event else "N/A"
        )
        logger.debug(
            f"Processing deferred debt burn at logIndex {event['logIndex']} "
            f"for liquidation at logIndex {pool_log_idx}"
        )

        _process_debt_burn_with_match(
            event=event,
            tx_context=tx_context,
            operation=matching_operation,
            scaled_event=scaled_event,
            enriched_event=enriched_event,
        )

        # Mark as assigned
        assigned_log_indices.add(event["logIndex"])


def _find_matching_liquidation_for_burn(
    *,
    user_address: ChecksumAddress,
    burn_token_address: ChecksumAddress,
    liquidation_operations: list[Operation],
    tx_context: TransactionContext,
) -> Operation | None:
    """
    Find a liquidation operation that matches a debt burn event.

    Matching is based on:
    1. User address must match
    2. Debt asset (vToken) must match

    This uses semantic matching since the burn event may have been emitted
    before the LiquidationCall event.
    """
    # Get the debt asset for this burn token
    debt_asset = get_asset_by_token_type(
        session=tx_context.session,
        market=tx_context.market,
        token_address=burn_token_address,
        token_type=TokenType.V_TOKEN,
    )

    if debt_asset is None:
        return None

    # Find matching liquidation operation
    for op in liquidation_operations:
        if op.pool_event is None:
            continue

        # Check if user matches
        liquidation_user = decode_address(op.pool_event["topics"][3])
        if liquidation_user != user_address:
            continue

        # Check if debt asset matches
        debt_asset_addr = decode_address(op.pool_event["topics"][2])

        # Get vToken address for the debt asset
        debt_v_token = _get_v_token_for_underlying(
            session=tx_context.session,
            market=tx_context.market,
            underlying_address=debt_asset_addr,
        )

        if debt_v_token is None:
            continue

        if debt_v_token == burn_token_address:
            return op

    return None
