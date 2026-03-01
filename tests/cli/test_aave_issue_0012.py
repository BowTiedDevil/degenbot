"""Test for Issue 0012: Interest Accrual During Debt Swap Not Processed.

Transaction: 0xa044d93a1aced198395d3293d4456fcb09a9a734d2949b5e2dff66338fa89625
Block: 17996836
User: 0xC5Ec4153F98729f4eaf61013B54B704Eb282ECF4

When a user performs a debt swap via flash loan, if the accrued interest on the
repaid debt exceeds the repayment amount, the Aave contract emits a Mint event
where balance_increase > amount. This represents net interest being minted to
the user's position during the burn operation.

The bug: This Mint event was being skipped because the logic assumed all
DEBT_MINT events during REPAY operations should be handled by the REPAY operation
itself. However, when balance_increase > amount (net interest after repayment),
the Mint event should be processed as INTEREST_ACCRUAL to properly reduce the
debt balance.

Root cause: src/degenbot/cli/aave_transaction_operations.py line 1300
The condition was skipping DEBT_MINT events when has_repay=True, even if they
represent interest accrual that needs separate processing.
"""

import pytest
from eth_abi.abi import encode
from hexbytes import HexBytes

from degenbot.aave.events import AaveV3PoolEvent, AaveV3ScaledTokenEvent
from degenbot.cli.aave_transaction_operations import (
    OperationType,
    TransactionOperationsParser,
)
from degenbot.functions import get_checksum_address

USDC_VTOKEN = get_checksum_address("0x72E95b8931767C79bA4EeE721354d6E99a61D004")
AAVE_POOL = get_checksum_address("0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2")


@pytest.fixture
def token_type_mapping():
    """Token type mapping for tests."""
    return {
        USDC_VTOKEN: "vToken",
    }


class TestIssue0012InterestAccrualDuringDebtSwap:
    """Test that interest accrual during debt swap is processed correctly."""

    def test_mint_event_with_balance_increase_greater_than_amount(self, token_type_mapping):
        """When balance_increase > amount during debt swap, create INTEREST_ACCRUAL."""

        tx_hash = HexBytes("0xa044d93a1aced198395d3293d4456fcb09a9a734d2949b5e2dff66338fa89625")

        user = get_checksum_address("0xC5Ec4153F98729f4eaf61013B54B704Eb282ECF4")
        reserve = get_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")

        # Mint event from transaction:
        # value=39551487, balance_increase=44551487 (interest > repayment)
        # This represents repayment of 5,000,000 USDC with 44,551,487 interest
        # Net minted: 39,551,487 (scaled)
        mint_amount = 39551487
        balance_increase = 44551487
        index = 1021476544057225178919224444

        mint_event = {
            "address": USDC_VTOKEN,
            "topics": [
                AaveV3ScaledTokenEvent.MINT.value,
                HexBytes(
                    encode(
                        types=["address"],
                        args=[user],
                    )
                ),
                HexBytes(
                    encode(
                        types=["address"],
                        args=[user],
                    )
                ),
            ],
            "data": HexBytes(
                encode(
                    types=["uint256", "uint256", "uint256"],
                    args=[mint_amount, balance_increase, index],
                )
            ),
            "logIndex": 288,
            "blockNumber": 17996836,
            "transactionHash": tx_hash,
        }

        # Repay event
        repay_amount = 5000000
        use_a_tokens = False

        repay_event = {
            "address": AAVE_POOL,
            "topics": [
                AaveV3PoolEvent.REPAY.value,
                HexBytes(
                    encode(
                        types=["address"],
                        args=[reserve],
                    )
                ),
                HexBytes(
                    encode(
                        types=["address"],
                        args=[user],
                    )
                ),
                HexBytes(
                    encode(
                        types=["address"],
                        args=[user],
                    )
                ),
            ],
            "data": HexBytes(
                encode(
                    types=["uint256", "bool"],
                    args=[repay_amount, use_a_tokens],
                )
            ),
            "logIndex": 291,
            "blockNumber": 17996836,
            "transactionHash": tx_hash,
        }

        parser = TransactionOperationsParser(token_type_mapping=token_type_mapping)
        tx_ops = parser.parse(
            events=[mint_event, repay_event],
            tx_hash=tx_hash,
        )

        # Should create 2 operations: REPAY + INTEREST_ACCRUAL
        assert len(tx_ops.operations) == 2

        # Find the INTEREST_ACCRUAL operation
        interest_ops = [
            op for op in tx_ops.operations if op.operation_type == OperationType.INTEREST_ACCRUAL
        ]
        assert len(interest_ops) == 1, (
            f"Expected 1 INTEREST_ACCRUAL operation, got {len(interest_ops)}"
        )

        interest_op = interest_ops[0]

        # INTEREST_ACCRUAL should contain the debt mint
        debt_mints = [e for e in interest_op.scaled_token_events if e.is_debt]
        assert len(debt_mints) == 1

        (mint,) = debt_mints
        assert mint.amount == 39551487
        assert mint.balance_increase == 44551487
        assert mint.balance_increase > mint.amount  # Key: interest > repayment

        # Validation should pass
        tx_ops.validate([mint_event, repay_event])
        assert interest_op.is_valid()


class TestDebtProcessorMintCalculation:
    """Test that the debt processor correctly calculates balance delta."""

    def test_mint_with_balance_increase_greater_than_amount(self):
        """Test REPAY path when balance_increase > amount."""
        from degenbot.aave.processors.debt.v1 import DebtV1Processor
        from degenbot.aave.processors.base import DebtMintEvent

        processor = DebtV1Processor()

        # Mint event: value=39551487, balance_increase=44551487
        # amount_repaid = 44551487 - 39551487 = 5000000
        # balance_delta should be negative (debt reduction)
        event_data = DebtMintEvent(
            caller="0xC5Ec4153F98729f4eaf61013B54B704Eb282ECF4",
            on_behalf_of="0xC5Ec4153F98729f4eaf61013B54B704Eb282ECF4",
            value=39551487,
            balance_increase=44551487,
            index=1021476544057225178919224444,
            scaled_amount=None,
        )

        result = processor.process_mint_event(
            event_data=event_data,
            previous_balance=0,  # Not used in calculation
            previous_index=0,  # Not used in calculation
        )

        # Should be REPAY path (is_repay=True)
        assert result.is_repay is True

        # Balance delta should be negative (debt reduction)
        assert result.balance_delta < 0

        # Calculate expected: -ray_div(5000000, index)
        # (5000000 * 10^27) // 1021476544057225178919224444 = 4894875
        expected_delta = -4894875
        assert result.balance_delta == expected_delta, (
            f"Expected balance_delta={expected_delta}, got {result.balance_delta}"
        )
