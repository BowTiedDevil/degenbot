"""
Liquidation processing functions for Aave V3.

This module handles liquidation event processing including multi-liquidation patterns
like COMBINED_BURN and SEPARATE_BURNS.
"""

from typing import assert_never

from eth_typing import ChecksumAddress

from degenbot.aave.events import AaveV3ScaledTokenEvent
from degenbot.aave.liquidation_patterns import detect_liquidation_patterns
from degenbot.cli.aave.db_assets import get_asset_by_token_type
from degenbot.cli.aave.types import TokenType, TransactionContext
from degenbot.cli.aave.utils import _get_v_token_for_underlying
from degenbot.cli.aave_transaction_operations import Operation
from degenbot.cli.aave_utils import decode_address


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

    Since operations are parsed from Pool contract events, the burn event may not be matched
    to a liquidation if the burn has a lower log index than the LiquidationCall.

    This function finds unassigned debt burns and matches them to liquidation operations
    retrospectively using semantic matching (user + debt asset).

    See debug/aave/0060 for detailed analysis.
    """

    if not liquidation_operations:
        return

    # Find all unassigned debt burn events
    for event in tx_context.events:
        if event["logIndex"] in assigned_log_indices:
            continue

        topic = event["topics"][0]
        if topic != AaveV3ScaledTokenEvent.BURN.value:
            continue

        assert_never()


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
    debt_reserve = get_asset_by_token_type(
        session=tx_context.session,
        market=tx_context.market,
        token_address=burn_token_address,
        token_type=TokenType.V_TOKEN,
    )

    if debt_reserve is None:
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
        debt_reserve_addr = decode_address(op.pool_event["topics"][2])

        # Get vToken address for the debt asset
        debt_v_token = _get_v_token_for_underlying(
            session=tx_context.session,
            market=tx_context.market,
            underlying_address=debt_reserve_addr,
        )

        if debt_v_token is None:
            continue

        if debt_v_token == burn_token_address:
            return op

    return None
