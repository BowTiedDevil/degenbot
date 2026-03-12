from dataclasses import dataclass, field

import eth_abi
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from sqlalchemy.orm import Session
from web3 import Web3
from web3.types import LogReceipt

from degenbot.aave.events import ERC20Event
from degenbot.checksum_cache import get_checksum_address
from degenbot.cli.aave_utils import decode_address
from degenbot.database.models.aave import (
    AaveGhoToken,
    AaveV3CollateralPosition,
    AaveV3DebtPosition,
    AaveV3Market,
)


@dataclass
class TransactionContext:
    """Context for processing a single transaction as a sequence of events."""

    w3: Web3
    tx_hash: HexBytes
    block_number: int
    events: list[LogReceipt]
    market: AaveV3Market
    session: Session
    gho_asset: AaveGhoToken

    # Snapshot of user discount percents at the start of transaction processing
    # Key: user address, Value: discount percent at transaction start
    user_discounts: dict[ChecksumAddress, int] = field(default_factory=dict)

    # Track discount percent updates by log index for transactions with multiple updates.
    # Key: user address, Value: list of (log_index, old_discount_percent) tuples sorted by
    # log_index. This allows determining the discount in effect at any point in the transaction.
    discount_updates_by_log_index: dict[ChecksumAddress, list[tuple[int, int]]] = field(
        default_factory=dict
    )

    # Set of user addresses with stkAAVE Transfer events in this transaction
    # Used to skip balance initialization when events provide authoritative values
    stk_aave_transfer_users: set[ChecksumAddress] = field(default_factory=set)

    # Track which stkAAVE transfer events have been processed (by log index)
    # This prevents double-counting in pending delta calculations
    processed_stk_aave_transfers: set[int] = field(default_factory=set)

    # Track BalanceTransfer events that have been processed in this transaction.
    # Key: (token_address, recipient_address), Value: (log_index, scaled_amount)
    # Used to match burns with preceding transfers for exact amount cancellation.
    processed_balance_transfers: dict[tuple[ChecksumAddress, ChecksumAddress], tuple[int, int]] = (
        field(default_factory=dict)
    )

    # Track modified positions to ensure we use the same object across operations.
    # Key: (user_address, asset_id), Value: position object
    modified_positions: dict[
        tuple[ChecksumAddress, int], AaveV3CollateralPosition | AaveV3DebtPosition
    ] = field(default_factory=dict)

    # Store the most recent REPAY paybackAmount for accurate interest accrual calculations.
    # When a debt burn occurs as part of interest accrual during a repay, we need the
    # original paybackAmount (not the reverse-calculated value from the Burn event) to
    # avoid 1 wei rounding errors from rayDivFloor/rayMulCeil asymmetry.
    last_repay_amount: int = 0

    # Store the most recent WITHDRAW amount for accurate interest accrual calculations.
    # When a collateral burn occurs as part of interest accrual during a withdraw, we need the
    # original withdrawAmount (not the reverse-calculated value from the Burn event) to
    # avoid 1 wei rounding errors from rayDivCeil/rayMulFloor asymmetry.
    last_withdraw_amount: int = 0

    def get_effective_discount_at_log_index(
        self,
        user_address: ChecksumAddress,
        log_index: int,
        default_discount: int,
    ) -> int:
        """
        Get the discount percent in effect at a specific log index.

        When a user has multiple DiscountPercentUpdated events in a transaction,
        each Mint/Burn event must use the discount that was in effect at that
        specific point in time (before any subsequent discount updates).

        Args:
            user_address: The user's address
            log_index: The log index to check
            default_discount: The fallback discount if no updates occurred before this log_index

        Returns:
            The discount percent in effect at the given log index
        """
        updates = self.discount_updates_by_log_index.get(user_address, [])
        if not updates:
            return default_discount

        # Find the most recent discount update before this log_index
        # updates is a list of (log_index, old_discount_percent) tuples sorted by log_index
        effective_discount = default_discount
        for update_log_index, old_discount in updates:
            if update_log_index < log_index:
                effective_discount = old_discount
            else:
                break

        return effective_discount

    def get_pending_stk_aave_delta_at_log_index(
        self,
        user_address: ChecksumAddress,
        log_index: int,
        discount_token: ChecksumAddress,
    ) -> int:
        """
        Calculate the net pending stkAAVE balance delta for a user at a specific log index.

        When stkAAVE transfer events occur after the current event (higher log index),
        but the transfer was initiated before the current event (due to reentrancy),
        the GHO debt token contract uses the post-transfer balance. This method
        calculates the net delta from pending transfers to determine the balance
        that was used by the contract.

        Event definition:
            event Transfer(
                address indexed from,
                address indexed to,
                uint256 value
            );

        Args:
            user_address: The user's address
            log_index: The current log index being processed
            discount_token: The stkAAVE token address

        Returns:
            The net pending balance delta (positive for incoming, negative for outgoing)
        """

        net_delta = 0

        for event in self.events:
            # Only process TRANSFER events from the discount token
            if event["topics"][0] != ERC20Event.TRANSFER.value:
                continue
            if get_checksum_address(event["address"]) != discount_token:
                continue

            transfer_log_index = event["logIndex"]
            # Only consider transfers that occur AFTER the current event
            if transfer_log_index <= log_index:
                continue

            # Skip transfers that have already been processed (balance already updated)
            if transfer_log_index in self.processed_stk_aave_transfers:
                continue

            from_addr = decode_address(event["topics"][1])
            to_addr = decode_address(event["topics"][2])
            (value,) = eth_abi.abi.decode(types=["uint256"], data=event["data"])

            if from_addr == user_address:
                net_delta -= value
            if to_addr == user_address:
                net_delta += value

        return net_delta
