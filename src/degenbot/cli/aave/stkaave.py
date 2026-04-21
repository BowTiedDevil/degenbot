"""stkAAVE balance tracking module for GHO discount calculations."""

import eth_abi.abi
from eth_typing import ChecksumAddress
from web3.types import LogReceipt

from degenbot.cli.aave.db_users import get_or_create_user
from degenbot.cli.aave.types import TransactionContext
from degenbot.cli.aave_utils import decode_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.database.models.aave import AaveV3User
from degenbot.functions import encode_function_calldata, raw_call
from degenbot.logging import logger


def get_or_init_stk_aave_balance(
    *,
    user: AaveV3User,
    tx_context: TransactionContext,
    log_index: int | None = None,
) -> int:
    """
    Get user's last-known stkAAVE balance.

    If the balance is unknown, perform a contract call at the previous block to ensure
    the balance check is performed before any events in the current block are processed.

    When log_index is provided and there are pending stkAAVE transfers for this user
    (transfers with log_index > current log_index), returns the predicted balance
    including the pending delta. This handles the reentrancy case where the GHO
    debt token contract sees the post-transfer balance before the Transfer event is emitted.
    """

    discount_token = tx_context.gho_asset.v_gho_discount_token

    if user.stk_aave_balance is None:
        balance: int
        (balance,) = raw_call(
            w3=tx_context.provider,
            address=discount_token,
            calldata=encode_function_calldata(
                function_prototype="balanceOf(address)",
                function_arguments=[user.address],
            ),
            return_types=["uint256"],
            block_identifier=tx_context.block_number - 1,
        )
        user.stk_aave_balance = balance

    assert user.stk_aave_balance is not None
    return user.stk_aave_balance


def process_stk_aave_transfer_event(
    *,
    event: LogReceipt,
    contract_address: ChecksumAddress,
    tx_context: TransactionContext,
) -> None:
    """
    Process a Transfer event on the stkAAVE token.

    This function updates the stkAAVE balance for Aave V3 users only. If either user is not in
    `AaveV3UsersTable` at the time, it will be skipped.

    Reference:
    ```
    event Transfer(
        address indexed from,
        address indexed to,
        uint256 value
    );
    ```
    """

    logger.debug(f"Processing stkAAVE transfer event at block {event['blockNumber']}")

    assert contract_address == tx_context.gho_asset.v_gho_discount_token

    from_address = decode_address(event["topics"][1])
    to_address = decode_address(event["topics"][2])

    if from_address == to_address:
        return

    (value,) = eth_abi.abi.decode(types=["uint256"], data=event["data"])

    logger.debug(f"stkAAVE transfer: {from_address} -> {to_address}, value={value}")

    # Get or create users involved in the transfer
    block_number = event["blockNumber"]

    from_user = (
        get_or_create_user(
            tx_context=tx_context,
            user_address=from_address,
            block_number=block_number,
        )
        if from_address != ZERO_ADDRESS
        else None
    )
    to_user = (
        get_or_create_user(
            tx_context=tx_context,
            user_address=to_address,
            block_number=block_number,
        )
        if to_address != ZERO_ADDRESS
        else None
    )

    # Ensure balances are known for both users
    # Skip initialization if there's a stkAAVE transfer for this user in this transaction
    # (the transfer event will set the balance correctly)
    if from_user is not None and from_user.stk_aave_balance is None:
        get_or_init_stk_aave_balance(
            user=from_user,
            tx_context=tx_context,
        )
    if to_user is not None and to_user.stk_aave_balance is None:
        get_or_init_stk_aave_balance(
            user=to_user,
            tx_context=tx_context,
        )

    # Apply balance changes
    if from_user is not None:
        assert from_user.stk_aave_balance is not None
        assert from_user.stk_aave_balance >= 0, f"{from_user.address} stkAAVE balance < 0!"
        from_user_old_balance = from_user.stk_aave_balance
        from_user.stk_aave_balance -= value

        logger.debug(
            f"stkAAVE balance update: {from_address}, "
            f"before: {from_user_old_balance}, "
            f"after: {from_user.stk_aave_balance}, "
            f"delta: -{value}"
        )
    if to_user is not None:
        assert to_user.stk_aave_balance is not None
        assert to_user.stk_aave_balance >= 0, f"{to_user.address} stkAAVE balance < 0!"
        to_user_old_balance = to_user.stk_aave_balance
        to_user.stk_aave_balance += value

        logger.debug(
            f"stkAAVE balance update: {to_address}"
            f"before: {to_user_old_balance}, "
            f"after: {to_user.stk_aave_balance}, "
            f"delta: +{value}"
        )

    # Mark this transfer as processed to prevent double-counting in pending delta calculations
    tx_context.processed_stk_aave_transfers.add(event["logIndex"])
