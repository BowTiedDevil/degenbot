"""Test GHO dust mint event processing.

This test verifies that GHO VariableDebtToken Mint events with
amount=0 and balance_increase=0 ("dust mints") are properly processed.

See debug/aave/0011 - GHO Dust Mint Not Processed.md for bug report.

Dust mints occur during updateDiscountDistribution calls when stkAAVE
balances change. They don't change the debt balance but still update
the user's cached lastIndex, so they must be processed.
"""

from hexbytes import HexBytes

from degenbot.checksum_cache import get_checksum_address
from degenbot.cli.aave_transaction_operations import (
    OperationType,
    TransactionOperationsParser,
)

GHO_VARIABLE_DEBT_TOKEN = get_checksum_address("0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B")
POOL_ADDRESS = get_checksum_address("0x87870Bca3F3fD6335C3f4ce8392D69350B4fA4E2")


class TestGHODustMint:
    """Test dust mint events from GHO discount updates."""

    def test_dust_mint_creates_interest_accrual_operation(self):
        """Dust mint with amount=0 and balance_increase=0 should create INTEREST_ACCRUAL operation."""
        parser = TransactionOperationsParser(
            token_type_mapping={GHO_VARIABLE_DEBT_TOKEN: "vToken"},
            pool_address=POOL_ADDRESS,
        )

        # Create dust mint event
        # Based on transaction 0x7120d824085292eafa6d540a17386f4a09168c658d17ea47d2705cd002a81636
        user_address = get_checksum_address("0x0fd3E4B5FcaC38ba6E48e9c7703805679eDFCcC4")
        caller = "0x0000000000000000000000000000000000000000"

        # Mint event data: amount, balanceIncrease, index
        # Dust mint: amount=0, balance_increase=0, index=current global index
        amount = 0
        balance_increase = 0
        index = 1000919954862378321350351390  # Example index value

        # Encode data as bytes32 values
        data_hex = (
            hex(amount)[2:].zfill(64)
            + hex(balance_increase)[2:].zfill(64)
            + hex(index)[2:].zfill(64)
        )

        events = [
            {
                "address": GHO_VARIABLE_DEBT_TOKEN,
                "topics": [
                    HexBytes(
                        "0x458f5fa412d0f69b08dd84872b0215675cc67bc1d5b6fd93300a1c3878b86196"
                    ),  # Mint
                    HexBytes("0x" + "0" * 24 + caller[2:]),  # caller
                    HexBytes("0x" + "0" * 24 + user_address[2:]),  # onBehalfOf
                ],
                "data": HexBytes("0x" + data_hex),
                "logIndex": 238,
                "blockNumber": 17859071,
                "transactionHash": HexBytes(
                    "0x7120d824085292eafa6d540a17386f4a09168c658d17ea47d2705cd002a81636"
                ),
            }
        ]

        # Parse operations
        tx_ops = parser.parse(
            events=events,
            tx_hash=HexBytes("0x7120d824085292eafa6d540a17386f4a09168c658d17ea47d2705cd002a81636"),
        )

        # Should create exactly 1 operation
        assert len(tx_ops.operations) == 1, f"Expected 1 operation, got {len(tx_ops.operations)}"

        op = tx_ops.operations[0]

        # Should be INTEREST_ACCRUAL type
        assert op.operation_type == OperationType.INTEREST_ACCRUAL, (
            f"Expected INTEREST_ACCRUAL, got {op.operation_type}"
        )

        # Should have no pool event
        assert op.pool_event is None, "INTEREST_ACCRUAL should have no pool event"

        # Should have exactly 1 scaled token event
        assert len(op.scaled_token_events) == 1, (
            f"Expected 1 scaled token event, got {len(op.scaled_token_events)}"
        )

        scaled_event = op.scaled_token_events[0]

        # Should be GHO_DEBT_MINT type
        assert scaled_event.event_type == "GHO_DEBT_MINT", (
            f"Expected GHO_DEBT_MINT, got {scaled_event.event_type}"
        )

        # Verify the dust mint values
        assert scaled_event.amount == 0, f"Expected amount=0, got {scaled_event.amount}"
        assert scaled_event.balance_increase == 0, (
            f"Expected balance_increase=0, got {scaled_event.balance_increase}"
        )
        assert scaled_event.index == index, f"Expected index={index}, got {scaled_event.index}"
        assert scaled_event.user_address == user_address, (
            f"Expected user={user_address}, got {scaled_event.user_address}"
        )

    def test_dust_mint_validation_passes(self):
        """Validation should accept dust mints with balance_increase=0."""
        parser = TransactionOperationsParser(
            token_type_mapping={GHO_VARIABLE_DEBT_TOKEN: "vToken"},
            pool_address=POOL_ADDRESS,
        )

        # Create dust mint event
        user_address = get_checksum_address("0x0fd3E4B5FcaC38ba6E48e9c7703805679eDFCcC4")
        caller = "0x0000000000000000000000000000000000000000"

        amount = 0
        balance_increase = 0
        index = 1000919954862378321350351390

        data_hex = (
            hex(amount)[2:].zfill(64)
            + hex(balance_increase)[2:].zfill(64)
            + hex(index)[2:].zfill(64)
        )

        events = [
            {
                "address": GHO_VARIABLE_DEBT_TOKEN,
                "topics": [
                    HexBytes("0x458f5fa412d0f69b08dd84872b0215675cc67bc1d5b6fd93300a1c3878b86196"),
                    HexBytes("0x" + "0" * 24 + caller[2:]),
                    HexBytes("0x" + "0" * 24 + user_address[2:]),
                ],
                "data": HexBytes("0x" + data_hex),
                "logIndex": 238,
                "blockNumber": 17859071,
                "transactionHash": HexBytes(
                    "0x7120d824085292eafa6d540a17386f4a09168c658d17ea47d2705cd002a81636"
                ),
            }
        ]

        tx_ops = parser.parse(
            events=events,
            tx_hash=HexBytes("0x7120d824085292eafa6d540a17386f4a09168c658d17ea47d2705cd002a81636"),
        )

        # Validation should pass (no exception raised)
        tx_ops.validate(events)

        # If we get here, validation passed
        assert True

    def test_interest_accrual_with_positive_balance_increase(self):
        """Normal interest accrual with balance_increase > 0 should still work."""
        parser = TransactionOperationsParser(
            token_type_mapping={GHO_VARIABLE_DEBT_TOKEN: "vToken"},
            pool_address=POOL_ADDRESS,
        )

        user_address = get_checksum_address("0x0fd3E4B5FcaC38ba6E48e9c7703805679eDFCcC4")
        caller = "0x0000000000000000000000000000000000000000"

        # Normal interest accrual: amount == balance_increase > 0
        amount = 1000000000000000000  # 1 token
        balance_increase = 1000000000000000000
        index = 1000919954862378321350351390

        data_hex = (
            hex(amount)[2:].zfill(64)
            + hex(balance_increase)[2:].zfill(64)
            + hex(index)[2:].zfill(64)
        )

        events = [
            {
                "address": GHO_VARIABLE_DEBT_TOKEN,
                "topics": [
                    HexBytes("0x458f5fa412d0f69b08dd84872b0215675cc67bc1d5b6fd93300a1c3878b86196"),
                    HexBytes("0x" + "0" * 24 + caller[2:]),
                    HexBytes("0x" + "0" * 24 + user_address[2:]),
                ],
                "data": HexBytes("0x" + data_hex),
                "logIndex": 238,
                "blockNumber": 17859071,
                "transactionHash": HexBytes(
                    "0x7120d824085292eafa6d540a17386f4a09168c658d17ea47d2705cd002a81636"
                ),
            }
        ]

        tx_ops = parser.parse(
            events=events,
            tx_hash=HexBytes("0x7120d824085292eafa6d540a17386f4a09168c658d17ea47d2705cd002a81636"),
        )

        # Should create INTEREST_ACCRUAL operation
        assert len(tx_ops.operations) == 1
        op = tx_ops.operations[0]
        assert op.operation_type == OperationType.INTEREST_ACCRUAL

        # Should have positive balance_increase
        scaled_event = op.scaled_token_events[0]
        assert scaled_event.balance_increase > 0
        assert scaled_event.balance_increase == amount
