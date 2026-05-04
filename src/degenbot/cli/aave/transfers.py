"""
Transfer processing functions for Aave V3.

This module handles the movement of scaled balances when tokens are transferred
between users, including:
- Paired ERC20 Transfer + BalanceTransfer events
- Standalone BalanceTransfer events (liquidations)
- Protocol mints and burns
"""

from typing import assert_never

import eth_abi.abi
from eth_typing import ChecksumAddress
from web3.types import LogReceipt

from degenbot.cli.aave.db_assets import get_asset_by_token_type
from degenbot.cli.aave.db_positions import get_or_create_collateral_position
from degenbot.cli.aave.db_users import get_or_create_user
from degenbot.cli.aave.types import TokenType, TransactionContext
from degenbot.cli.aave_transaction_operations import Operation, OperationType, ScaledTokenEvent
from degenbot.cli.aave_utils import decode_address
from degenbot.constants import ZERO_ADDRESS


def _should_skip_collateral_transfer(
    scaled_event: ScaledTokenEvent,
    operation: Operation | None,
    tx_context: TransactionContext,
) -> bool:
    """
    Determine if this collateral transfer event should be skipped.

    Returns True if:
    1. This is a paired BalanceTransfer handled by its paired ERC20 Transfer
    2. This is part of a REPAY_WITH_ATOKENS operation (burn handles it)
    3. This is a protocol mint (from zero address)
    4. This is a direct burn handled by Burn event
    5. This is an ERC20 Transfer in a liquidation operation (BalanceTransfer handles it)

    Special handling for liquidations:
    - BalanceTransfer events are NOT skipped (they contain the liquidation fees to treasury)
    - ERC20 Transfer events ARE skipped (only the BalanceTransfer represents the actual
      scaled balance movement to the treasury)
    """
    # Skip paired BalanceTransfer events - handled by their paired ERC20 Transfer
    # BUT: Don't skip for liquidation operations - the BalanceTransfer IS the transfer to treasury
    if (
        scaled_event.index
        and operation
        and operation.balance_transfer_events
        and operation.operation_type
        not in {OperationType.LIQUIDATION, OperationType.GHO_LIQUIDATION}
    ):
        return any(
            bt_event["logIndex"] == scaled_event.event["logIndex"]
            for bt_event in operation.balance_transfer_events
        )

    # Skip ERC20 Transfers for liquidation operations - only process BalanceTransfer events
    # The BalanceTransfer events contain the liquidation fees to the treasury
    return bool(
        scaled_event.index is None
        and operation
        and operation.operation_type in {OperationType.LIQUIDATION, OperationType.GHO_LIQUIDATION}
    )


def _match_paired_balance_transfer(
    scaled_event: ScaledTokenEvent,
    operation: Operation | None,
    token_address: ChecksumAddress,
) -> tuple[LogReceipt | None, int | None, int | None]:
    """
    Find a paired BalanceTransfer event for this ERC20 Transfer.

    BalanceTransfer events contain the actual scaled balance being moved,
    while ERC20 Transfer events show aToken amounts (scaled * index / RAY).

    Args:
        scaled_event: The scaled token event being processed
        operation: The operation context (may contain paired BalanceTransfer events)
        token_address: The checksum address of the token contract

    Returns:
        Tuple of (matched_event, scaled_amount, index) or (None, None, None)
    """

    # For liquidation operations, don't match ERC20 Transfers with BalanceTransfers
    # They represent different movements and should be processed separately
    if operation.operation_type in {OperationType.LIQUIDATION, OperationType.GHO_LIQUIDATION}:
        return None, None, None

    for bt_event in operation.balance_transfer_events:
        bt_from = decode_address(bt_event["topics"][1])
        bt_to = decode_address(bt_event["topics"][2])
        bt_token = bt_event["address"]

        # Match by token, from, and to addresses (semantic matching)
        if (
            bt_token == token_address
            and bt_from == scaled_event.from_address
            and bt_to == scaled_event.target_address
        ):
            decoded_amount: int
            decoded_index: int
            decoded_amount, decoded_index = eth_abi.abi.decode(
                types=["uint256", "uint256"],
                data=bt_event["data"],
            )
            return bt_event, decoded_amount, decoded_index

    assert_never()


def _process_collateral_transfer(
    *,
    tx_context: TransactionContext,
    operation: Operation,
    scaled_event: ScaledTokenEvent,
) -> None:
    """
    Process collateral (aToken) transfer between users.

    This function handles the movement of scaled balances when aTokens are transferred
    between users. It accounts for:
    - Paired ERC20 Transfer + BalanceTransfer events
    - Standalone BalanceTransfer events (liquidations)
    - Protocol mints and burns
    """

    assert scaled_event.from_address is not None
    assert scaled_event.target_address is not None

    # Skip events that are handled elsewhere (paired BalanceTransfers, mints, burns)
    if _should_skip_collateral_transfer(scaled_event, operation, tx_context):
        return

    # Get sender and their position
    sender = get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.from_address,
        block_number=scaled_event.event["blockNumber"],
    )

    token_address = scaled_event.event["address"]
    collateral_asset = get_asset_by_token_type(
        session=tx_context.session,
        market=tx_context.market,
        token_address=token_address,
        token_type=TokenType.A_TOKEN,
    )
    assert collateral_asset is not None

    sender_position = get_or_create_collateral_position(
        tx_context=tx_context,
        user=sender,
        asset_id=collateral_asset.id,
    )

    # Determine the scaled amount and index for this transfer
    # For paired events, use BalanceTransfer data (scaled balance)
    # For standalone events, use the event data directly
    _, scaled_amount, transfer_index = _match_paired_balance_transfer(
        scaled_event=scaled_event,
        operation=operation,
        token_address=token_address,
    )

    if scaled_amount is None:
        # No paired BalanceTransfer found - use event data directly
        scaled_amount = scaled_event.amount
        transfer_index = scaled_event.index

    assert transfer_index is not None

    # Update sender's scaled balance
    sender_position.balance -= scaled_amount

    # Update sender's last_index only if the new index is higher and valid
    # This prevents older transfer indices from overwriting newer ones
    current_sender_index = sender_position.last_index or 0
    if transfer_index > current_sender_index:
        sender_position.last_index = transfer_index

    # Handle recipient
    if scaled_event.target_address != ZERO_ADDRESS:
        recipient = get_or_create_user(
            tx_context=tx_context,
            user_address=scaled_event.target_address,
            block_number=scaled_event.event["blockNumber"],
        )
        recipient_position = get_or_create_collateral_position(
            tx_context=tx_context,
            user=recipient,
            asset_id=collateral_asset.id,
        )
        recipient_position.balance += scaled_amount
        recipient_position.last_index = transfer_index
