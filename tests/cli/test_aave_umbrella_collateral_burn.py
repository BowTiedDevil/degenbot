"""Test for Issue 0024: Umbrella Staking Collateral Burn Without Pool Event.

Bug Report: debug/aave/0024 - Umbrella Staking Collateral Burn Without Pool Event.md

This test verifies that unassigned COLLATERAL_BURN events (burns without a corresponding
WITHDRAW, REPAY, or LIQUIDATION_CALL pool event) are properly processed. These events
occur during Aave Umbrella staking contract creation where aTokens are burned directly
without a pool operation.
"""

import pytest
from degenbot.cli.aave_transaction_operations import (
    Operation,
    OperationType,
    TransactionOperationsParser,
)
from degenbot.aave.events import AaveV3ScaledTokenEvent
from hexbytes import HexBytes


class TestUmbrellaCollateralBurn:
    """Test COLLATERAL_BURN events without pool events (umbrella/staking operations)."""

    def test_collateral_burn_without_pool_event_creates_interest_accrual_operation(self):
        """Test that standalone COLLATERAL_BURN creates INTEREST_ACCRUAL operation.

        This reproduces the bug from Issue 0024 where aTokens burned during
        umbrella contract creation were not processed because there was no
        matching WITHDRAW/REPAY/LIQUIDATION pool event.
        """
        # Token addresses
        aave_pool = "0x5300A1a15135EA4dc7aD5a167152C01EFc9b192A"
        user_address = "0xD400fc38ED4732893174325693a63C30ee3881a8"
        token_address = "0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c"

        # Simulate events from umbrella contract creation transaction
        # Event 1: Transfer IN from Pool to User (ERC20 Transfer)
        transfer_in = {
            "address": token_address,
            "topics": [
                HexBytes(
                    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
                ),  # Transfer
                HexBytes(f"0x000000000000000000000000{aave_pool[2:]}"),  # from: Pool
                HexBytes(f"0x000000000000000000000000{user_address[2:]}"),  # to: User
            ],
            "data": HexBytes(
                "0x000000000000000000000000000000000000000000000000000000000a099c2b"
            ),  # amount: 168401963
            "logIndex": 19,
            "blockNumber": 22638170,
        }

        # Event 2: BalanceTransfer from Pool to User
        balance_transfer = {
            "address": token_address,
            "topics": [
                HexBytes(
                    "0x4beccb90f994c31aced7a23b5611020728a23d8ec5cddd1a3e9d97b96fda8666"
                ),  # BalanceTransfer
                HexBytes(f"0x000000000000000000000000{aave_pool[2:]}"),  # from: Pool
                HexBytes(f"0x000000000000000000000000{user_address[2:]}"),  # to: User
            ],
            "data": HexBytes(
                "0x0000000000000000000000000000000000000000000000000000000008defed7"  # amount: 148831959
                "0000000000000000000000000000000000000000000000000000000000000000"  # index: 0
            ),
            "logIndex": 21,
            "blockNumber": 22638170,
        }

        # Event 3: Transfer OUT from User to Zero (ERC20 Transfer to burn)
        transfer_out = {
            "address": token_address,
            "topics": [
                HexBytes(
                    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
                ),  # Transfer
                HexBytes(f"0x000000000000000000000000{user_address[2:]}"),  # from: User
                HexBytes(
                    "0x0000000000000000000000000000000000000000000000000000000000000000"
                ),  # to: 0x0
            ],
            "data": HexBytes(
                "0x000000000000000000000000000000000000000000000000000000000a099c2b"
            ),  # amount: 168401963
            "logIndex": 23,
            "blockNumber": 22638170,
        }

        # Event 4: Burn event (the key event that's not matched to any pool operation)
        burn_event = {
            "address": token_address,
            "topics": [
                HexBytes(
                    "0x4cf25bc1d991c17529c25213d3cc0cda295eeaad5f13f361969b12ea48015f90"
                ),  # Burn
                HexBytes(f"0x000000000000000000000000{user_address[2:]}"),  # from: User
                HexBytes(f"0x000000000000000000000000{token_address[2:]}"),  # target: Token
            ],
            "data": HexBytes(
                "0x000000000000000000000000000000000000000000000000000000000a099c2b"  # amount: 168401963
                "0000000000000000000000000000000000000000000000000000000000000000"  # balance_increase: 0
                "0000000000000000000000000000000000000000000000000000000000000000"  # index: 0
            ),
            "logIndex": 24,
            "blockNumber": 22638170,
        }

        # Create parser with token type mapping
        token_type_mapping = {
            token_address: "aToken",
        }

        parser = TransactionOperationsParser(
            token_type_mapping=token_type_mapping,
            pool_address=aave_pool,
            debt_token_to_reserve={},
        )

        # Parse the events
        all_events = [transfer_in, balance_transfer, transfer_out, burn_event]
        tx_hash = HexBytes("0xaa900e1ac9ece8a1a0db38c111ccfe5b5fb735a838278995a7e6534a8fc32a63")

        tx_operations = parser.parse(events=all_events, tx_hash=tx_hash)

        # Verify that operations were created
        assert len(tx_operations.operations) > 0, "Expected at least one operation"

        # Find the burn operation
        burn_operations = [
            op
            for op in tx_operations.operations
            if any(ev.event["logIndex"] == 24 for ev in op.scaled_token_events)
        ]

        # The key assertion: the burn event should be assigned to an operation
        assert len(burn_operations) == 1, (
            f"Expected exactly 1 operation containing the burn event, "
            f"found {len(burn_operations)}. The burn event was not processed!"
        )

        burn_op = burn_operations[0]

        # Verify it's an INTEREST_ACCRUAL operation (standalone scaled token operation)
        assert burn_op.operation_type == OperationType.INTEREST_ACCRUAL, (
            f"Expected INTEREST_ACCRUAL operation for standalone burn, got {burn_op.operation_type}"
        )

        # Verify no pool event is attached
        assert burn_op.pool_event is None, "Standalone burn should not have a pool event"

        # Verify the burn event is in scaled_token_events
        assert len(burn_op.scaled_token_events) == 1, (
            f"Expected exactly 1 scaled token event, got {len(burn_op.scaled_token_events)}"
        )

        burn_scaled_event = burn_op.scaled_token_events[0]
        assert burn_scaled_event.event_type == "COLLATERAL_BURN", (
            f"Expected COLLATERAL_BURN event type, got {burn_scaled_event.event_type}"
        )
        assert burn_scaled_event.user_address == user_address, (
            f"Expected user {user_address}, got {burn_scaled_event.user_address}"
        )
