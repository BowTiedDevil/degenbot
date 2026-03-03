"""Tests for Aave V3 transaction operations parser.

These tests verify the operation-based event parsing handles all patterns
identified in the bug reports in debug/aave/.
"""

import eth_abi
import pytest
from hexbytes import HexBytes
from web3.types import LogReceipt

from degenbot.checksum_cache import get_checksum_address
from degenbot.aave.events import AaveV3PoolEvent, AaveV3ScaledTokenEvent
from degenbot.cli.aave_transaction_operations import (
    GHO_TOKEN_ADDRESS,
    GHO_VARIABLE_DEBT_TOKEN_ADDRESS,
    OperationType,
    TransactionOperationsParser,
    TransactionValidationError,
)

# Test token addresses - used for classification
TEST_COLLATERAL_TOKEN = get_checksum_address("0x" + "1" * 40)  # aToken
TEST_DEBT_TOKEN = get_checksum_address("0x" + "2" * 40)  # vToken

# Token type mapping for parser
TEST_TOKEN_TYPE_MAPPING = {
    TEST_COLLATERAL_TOKEN: "aToken",
    TEST_DEBT_TOKEN: "vToken",
}


class EventFactory:
    """Factory for creating test events."""

    @staticmethod
    def create_supply_event(reserve: str, user: str, amount: int, log_index: int) -> LogReceipt:
        """Create a SUPPLY pool event."""

        topics = [
            AaveV3PoolEvent.SUPPLY.value,
            HexBytes("0x" + "0" * 24 + reserve[2:]),
            HexBytes("0x" + "0" * 24 + user[2:]),
        ]

        data = eth_abi.encode(
            ["address", "uint256"],
            [get_checksum_address("0x" + "0" * 40), amount],
        )

        return {
            "address": get_checksum_address("0x" + "0" * 40),
            "topics": topics,
            "data": HexBytes(data),
            "logIndex": log_index,
            "blockNumber": 1000000,
            "transactionHash": HexBytes("0x" + "00" * 32),
        }

    @staticmethod
    def create_withdraw_event(reserve: str, user: str, amount: int, log_index: int) -> LogReceipt:
        """Create a WITHDRAW pool event."""

        topics = [
            AaveV3PoolEvent.WITHDRAW.value,
            HexBytes("0x" + "0" * 24 + reserve[2:]),
            HexBytes("0x" + "0" * 24 + user[2:]),
        ]

        data = eth_abi.encode(["uint256"], [amount])

        return {
            "address": get_checksum_address("0x" + "0" * 40),
            "topics": topics,
            "data": HexBytes(data),
            "logIndex": log_index,
            "blockNumber": 1000000,
            "transactionHash": HexBytes("0x" + "00" * 32),
        }

    @staticmethod
    def create_liquidation_call_event(
        *,
        collateral_asset: str,
        debt_asset: str,
        user: str,
        debt_to_cover: int,
        liquidated_collateral: int,
        log_index: int,
    ) -> LogReceipt:
        """Create a LIQUIDATION_CALL pool event."""

        topics = [
            AaveV3PoolEvent.LIQUIDATION_CALL.value,
            HexBytes("0x" + "0" * 24 + collateral_asset[2:]),
            HexBytes("0x" + "0" * 24 + debt_asset[2:]),
            HexBytes("0x" + "0" * 24 + user[2:]),
        ]

        data = eth_abi.encode(
            ["uint256", "uint256", "address", "bool"],
            [
                debt_to_cover,
                liquidated_collateral,
                get_checksum_address("0x" + "0" * 40),
                False,
            ],
        )

        return {
            "address": get_checksum_address("0x" + "0" * 40),
            "topics": topics,
            "data": HexBytes(data),
            "logIndex": log_index,
            "blockNumber": 1000000,
            "transactionHash": HexBytes("0x" + "00" * 32),
        }

    @staticmethod
    def create_repay_event(
        *,
        reserve: str,
        user: str,
        amount: int,
        use_a_tokens: bool,
        log_index: int,
    ) -> LogReceipt:
        """Create a REPAY pool event."""

        topics = [
            AaveV3PoolEvent.REPAY.value,
            HexBytes("0x" + "0" * 24 + reserve[2:]),
            HexBytes("0x" + "0" * 24 + user[2:]),
        ]

        data = eth_abi.encode(
            ["uint256", "bool"],
            [amount, use_a_tokens],
        )

        return {
            "address": get_checksum_address("0x" + "0" * 40),
            "topics": topics,
            "data": HexBytes(data),
            "logIndex": log_index,
            "blockNumber": 1000000,
            "transactionHash": HexBytes("0x" + "00" * 32),
        }

    @staticmethod
    def create_borrow_event(
        *,
        reserve: str,
        user: str,
        on_behalf_of: str,
        amount: int,
        log_index: int,
    ) -> LogReceipt:
        """Create a BORROW pool event."""

        topics = [
            AaveV3PoolEvent.BORROW.value,
            HexBytes("0x" + "0" * 24 + reserve[2:]),
            HexBytes("0x" + "0" * 24 + on_behalf_of[2:]),
        ]

        # BORROW data: caller, amount, interestRateMode, borrowRate
        data = eth_abi.encode(
            ["address", "uint256", "uint8", "uint256"],
            [get_checksum_address(user), amount, 2, 0],
        )

        return {
            "address": get_checksum_address("0x" + "0" * 40),
            "topics": topics,
            "data": HexBytes(data),
            "logIndex": log_index,
            "blockNumber": 1000000,
            "transactionHash": HexBytes("0x" + "00" * 32),
        }

    @staticmethod
    def create_collateral_mint_event(
        user: str, amount: int, balance_increase: int, log_index: int
    ) -> LogReceipt:
        """Create a collateral Mint event."""

        caller = get_checksum_address("0x" + "0" * 40)

        topics = [
            AaveV3ScaledTokenEvent.MINT.value,
            HexBytes("0x" + "0" * 24 + caller[2:]),
            HexBytes("0x" + "0" * 24 + user[2:]),
        ]

        data = eth_abi.encode(
            ["uint256", "uint256", "uint256"],
            [amount, balance_increase, 1000000000000000000000000000],
        )

        return {
            "address": TEST_COLLATERAL_TOKEN,
            "topics": topics,
            "data": HexBytes(data),
            "logIndex": log_index,
            "blockNumber": 1000000,
            "transactionHash": HexBytes("0x" + "00" * 32),
        }

    @staticmethod
    def create_collateral_burn_event(
        user: str, amount: int, balance_increase: int, log_index: int
    ) -> LogReceipt:
        """Create a collateral Burn event."""

        target = get_checksum_address("0x" + "0" * 40)

        topics = [
            AaveV3ScaledTokenEvent.BURN.value,
            HexBytes("0x" + "0" * 24 + user[2:]),
            HexBytes("0x" + "0" * 24 + target[2:]),
        ]

        data = eth_abi.encode(
            ["uint256", "uint256", "uint256"],
            [amount, balance_increase, 1000000000000000000000000000],
        )

        return {
            "address": TEST_COLLATERAL_TOKEN,
            "topics": topics,
            "data": HexBytes(data),
            "logIndex": log_index,
            "blockNumber": 1000000,
            "transactionHash": HexBytes("0x" + "00" * 32),
        }

    @staticmethod
    def create_debt_mint_event(
        user: str,
        amount: int,
        balance_increase: int,
        log_index: int,
        contract_address: str | None = None,
    ) -> LogReceipt:
        """Create a debt Mint event (interest accrual)."""

        caller = get_checksum_address("0x" + "0" * 40)

        topics = [
            AaveV3ScaledTokenEvent.MINT.value,
            HexBytes("0x" + "0" * 24 + caller[2:]),
            HexBytes("0x" + "0" * 24 + user[2:]),
        ]

        data = eth_abi.encode(
            ["uint256", "uint256", "uint256"],
            [amount, balance_increase, 1000000000000000000000000000],
        )

        return {
            "address": contract_address if contract_address else TEST_DEBT_TOKEN,
            "topics": topics,
            "data": HexBytes(data),
            "logIndex": log_index,
            "blockNumber": 1000000,
            "transactionHash": HexBytes("0x" + "00" * 32),
        }

    @staticmethod
    def create_debt_burn_event(
        user: str, amount: int, balance_increase: int, log_index: int
    ) -> LogReceipt:
        """Create a debt Burn event."""

        target = get_checksum_address("0x" + "0" * 40)

        topics = [
            AaveV3ScaledTokenEvent.BURN.value,
            HexBytes("0x" + "0" * 24 + user[2:]),
            HexBytes("0x" + "0" * 24 + target[2:]),
        ]

        data = eth_abi.encode(
            ["uint256", "uint256", "uint256"],
            [amount, balance_increase, 1000000000000000000000000000],
        )

        return {
            "address": TEST_DEBT_TOKEN,
            "topics": topics,
            "data": HexBytes(data),
            "logIndex": log_index,
            "blockNumber": 1000000,
            "transactionHash": HexBytes("0x" + "00" * 32),
        }

    @staticmethod
    def create_gho_burn_event(
        user: str, amount: int, balance_increase: int, log_index: int
    ) -> LogReceipt:
        """Create a GHO debt Burn event."""

        target = get_checksum_address("0x" + "0" * 40)

        topics = [
            AaveV3ScaledTokenEvent.BURN.value,
            HexBytes("0x" + "0" * 24 + user[2:]),
            HexBytes("0x" + "0" * 24 + target[2:]),
        ]

        data = eth_abi.encode(
            ["uint256", "uint256", "uint256"],
            [amount, balance_increase, 1000000000000000000000000000],
        )

        return {
            "address": GHO_VARIABLE_DEBT_TOKEN_ADDRESS,
            "topics": topics,
            "data": HexBytes(data),
            "logIndex": log_index,
            "blockNumber": 1000000,
            "transactionHash": HexBytes("0x" + "00" * 32),
        }

    @staticmethod
    def create_collateral_balance_transfer_event(
        from_user: str, to_user: str, amount: int, log_index: int
    ) -> LogReceipt:
        """Create a collateral BalanceTransfer event."""

        topics = [
            AaveV3ScaledTokenEvent.BALANCE_TRANSFER.value,
            HexBytes("0x" + "0" * 24 + from_user[2:]),
            HexBytes("0x" + "0" * 24 + to_user[2:]),
        ]

        # BalanceTransfer data: amount, index
        data = eth_abi.encode(
            ["uint256", "uint256"],
            [amount, 1000000000000000000000000000],
        )

        return {
            "address": TEST_COLLATERAL_TOKEN,
            "topics": topics,
            "data": HexBytes(data),
            "logIndex": log_index,
            "blockNumber": 1000000,
            "transactionHash": HexBytes("0x" + "00" * 32),
        }


class TestOperationParsing:
    """Test parsing transactions into operations."""

    def test_supply_operation_parsed_correctly(self):
        """Standard SUPPLY -> COLLATERAL_MINT operation."""
        user = get_checksum_address("0x1234567890123456789012345678901234567890")
        reserve = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")

        # Create events
        mint_event = EventFactory.create_collateral_mint_event(
            user=user,
            amount=1000000000000000000,
            balance_increase=999999999999999999,  # Less than amount for deposit
            log_index=10,
        )

        supply_event = EventFactory.create_supply_event(
            reserve=reserve,
            user=user,
            amount=1000000000000000000,
            log_index=12,  # SUPPLY comes after Mint
        )

        parser = TransactionOperationsParser(token_type_mapping=TEST_TOKEN_TYPE_MAPPING)
        tx_ops = parser.parse([mint_event, supply_event], HexBytes("0x" + "00" * 32))

        assert len(tx_ops.operations) == 1
        op = tx_ops.operations[0]

        assert op.operation_type == OperationType.SUPPLY
        assert len(op.scaled_token_events) == 1
        assert op.scaled_token_events[0].event_type == "COLLATERAL_MINT"
        assert op.is_valid()

    def test_liquidation_parsed_as_single_operation(self):
        """Liquidation with LIQUIDATION_CALL -> debt burn + collateral burn."""
        user = get_checksum_address("0x1234567890123456789012345678901234567890")
        collateral_asset = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
        debt_asset = get_checksum_address("0xA0b86a33E6441e6C7D3D4B4b8B8B8B8B8B8B8B8B")

        # Create events
        liquidation_event = EventFactory.create_liquidation_call_event(
            collateral_asset=collateral_asset,
            debt_asset=debt_asset,
            user=user,
            debt_to_cover=500000000000000000,
            liquidated_collateral=300000000000000000,
            log_index=100,
        )

        debt_burn_event = EventFactory.create_debt_burn_event(
            user=user,
            amount=500000000000000000,
            balance_increase=500000000000000000,
            log_index=97,  # Before liquidation
        )

        collateral_burn_event = EventFactory.create_collateral_burn_event(
            user=user,
            amount=300000000000000000,
            balance_increase=300000000000000000,
            log_index=104,  # After liquidation
        )

        parser = TransactionOperationsParser(token_type_mapping=TEST_TOKEN_TYPE_MAPPING)
        tx_ops = parser.parse(
            [debt_burn_event, liquidation_event, collateral_burn_event],
            HexBytes("0x" + "00" * 32),
        )

        assert len(tx_ops.operations) == 1
        op = tx_ops.operations[0]

        assert op.operation_type == OperationType.LIQUIDATION
        assert len(op.scaled_token_events) == 2

        # Verify both burns are present
        debt_burns = [e for e in op.scaled_token_events if e.is_debt]
        collateral_burns = [e for e in op.scaled_token_events if e.is_collateral]

        assert len(debt_burns) == 1
        assert len(collateral_burns) == 1
        assert op.is_valid()

    def test_repay_with_atokens_parsed_correctly(self):
        """Repay with aTokens has debt burn + collateral burn."""
        user = get_checksum_address("0x1234567890123456789012345678901234567890")
        reserve = get_checksum_address("0xA0b86a33E6441e6C7D3D4B4b8B8B8B8B8B8B8B8B")

        # Create events
        repay_event = EventFactory.create_repay_event(
            reserve=reserve,
            user=user,
            amount=1000000000000000000,
            use_a_tokens=True,
            log_index=100,
        )

        debt_burn_event = EventFactory.create_debt_burn_event(
            user=user,
            amount=1000000000000000000,
            balance_increase=1000000000000000000,
            log_index=98,
        )

        collateral_burn_event = EventFactory.create_collateral_burn_event(
            user=user,
            amount=1000000000000000000,
            balance_increase=1000000000000000000,
            log_index=99,
        )

        parser = TransactionOperationsParser(token_type_mapping=TEST_TOKEN_TYPE_MAPPING)
        tx_ops = parser.parse(
            [debt_burn_event, collateral_burn_event, repay_event],
            HexBytes("0x" + "00" * 32),
        )

        assert len(tx_ops.operations) == 1
        op = tx_ops.operations[0]

        assert op.operation_type == OperationType.REPAY_WITH_ATOKENS
        assert len(op.scaled_token_events) == 2
        assert op.is_valid()

    def test_gho_liquidation_detected_by_address(self):
        """GHO liquidation detected by GHO token address."""
        user = get_checksum_address("0x1234567890123456789012345678901234567890")
        collateral_asset = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")

        # Create events
        # Note: debt_asset should be GHO_TOKEN_ADDRESS (the GHO token),
        # not GHO_VARIABLE_DEBT_TOKEN_ADDRESS (the debt token contract)
        liquidation_event = EventFactory.create_liquidation_call_event(
            collateral_asset=collateral_asset,
            debt_asset=GHO_TOKEN_ADDRESS,
            user=user,
            debt_to_cover=500000000000000000,
            liquidated_collateral=300000000000000000,
            log_index=100,
        )

        gho_burn_event = EventFactory.create_gho_burn_event(
            user=user,
            amount=500000000000000000,
            balance_increase=500000000000000000,
            log_index=97,
        )

        collateral_burn_event = EventFactory.create_collateral_burn_event(
            user=user,
            amount=300000000000000000,
            balance_increase=300000000000000000,
            log_index=104,
        )

        parser = TransactionOperationsParser(token_type_mapping=TEST_TOKEN_TYPE_MAPPING)
        tx_ops = parser.parse(
            [gho_burn_event, liquidation_event, collateral_burn_event],
            HexBytes("0x" + "00" * 32),
        )

        assert len(tx_ops.operations) == 1
        op = tx_ops.operations[0]

        assert op.operation_type == OperationType.GHO_LIQUIDATION

    def test_multi_operation_tx_parsed_correctly(self):
        """ParaSwap-style multi-operation transaction."""
        user1 = get_checksum_address("0x1234567890123456789012345678901234567890")
        user2 = get_checksum_address("0x0987654321098765432109876543210987654321")
        reserve = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")

        # Operation 1: Withdraw
        withdraw_event = EventFactory.create_withdraw_event(
            reserve=reserve,
            user=user1,
            amount=1000000000000000000,
            log_index=50,
        )

        burn_event_1 = EventFactory.create_collateral_burn_event(
            user=user1,
            amount=1000000000000000000,
            balance_increase=1000000000000000000,
            log_index=52,  # After withdraw
        )

        # Operation 2: Supply
        supply_event = EventFactory.create_supply_event(
            reserve=reserve,
            user=user2,
            amount=2000000000000000000,
            log_index=100,
        )

        mint_event = EventFactory.create_collateral_mint_event(
            user=user2,
            amount=2000000000000000000,
            balance_increase=1999999999999999999,  # Less than amount for deposit
            log_index=102,  # After supply
        )

        parser = TransactionOperationsParser()
        tx_ops = parser.parse(
            [withdraw_event, burn_event_1, supply_event, mint_event],
            HexBytes("0x" + "00" * 32),
        )

        assert len(tx_ops.operations) == 2

        # Verify operation types
        assert tx_ops.operations[0].operation_type == OperationType.WITHDRAW
        assert tx_ops.operations[1].operation_type == OperationType.SUPPLY


class TestOperationValidation:
    """Test strict validation of operations."""

    def test_validation_fails_on_incomplete_liquidation(self):
        """Missing collateral burn causes validation error."""
        user = get_checksum_address("0x1234567890123456789012345678901234567890")
        collateral_asset = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
        debt_asset = get_checksum_address("0xA0b86a33E6441e6C7D3D4B4b8B8B8B8B8B8B8B8B")

        liquidation_event = EventFactory.create_liquidation_call_event(
            collateral_asset=collateral_asset,
            debt_asset=debt_asset,
            user=user,
            debt_to_cover=500000000000000000,
            liquidated_collateral=300000000000000000,
            log_index=100,
        )

        debt_burn_event = EventFactory.create_debt_burn_event(
            user=user,
            amount=500000000000000000,
            balance_increase=500000000000000000,
            log_index=97,
        )

        # Missing collateral burn!

        parser = TransactionOperationsParser()
        tx_ops = parser.parse(
            [debt_burn_event, liquidation_event],
            HexBytes("0x" + "00" * 32),
        )

        with pytest.raises(TransactionValidationError) as exc_info:
            tx_ops.validate([debt_burn_event, liquidation_event])

        error_str = str(exc_info.value)
        assert "Expected at least 1 collateral event" in error_str
        assert "DEBUG NOTE" in error_str
        assert "logIndex" in error_str

    def test_flash_loan_liquidation_validates_with_zero_debt_burns(self):
        """Flash loan liquidation validates with 0 debt burns (only collateral burn).

        Regression test for issue #0033 - Flash loan liquidations don't emit debt burn events
        because the debt is repaid via flash loan mechanics rather than standard debt token burns.
        Transaction: 0xcb087ea4d8d1b7c890318c3eccd7f730f24a1f1b55b25c156b9649e543de0588
        """
        user = get_checksum_address("0x1234567890123456789012345678901234567890")
        collateral_asset = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
        debt_asset = get_checksum_address("0xA0b86a33E6441e6C7D3D4B4b8B8B8B8B8B8B8B8B")

        # Flash loan liquidation pattern:
        # 1. Debt mint (flash borrow) - not assigned to liquidation operation
        # 2. Collateral burn - assigned to liquidation operation
        # 3. LiquidationCall - the pool event
        # No debt burn because flash loan is repaid through swap, not burn

        debt_mint_event = EventFactory.create_debt_mint_event(
            user=user,
            amount=500000000000000000,
            balance_increase=499999999999999999,  # Less than amount for borrow
            log_index=97,
        )

        liquidation_event = EventFactory.create_liquidation_call_event(
            collateral_asset=collateral_asset,
            debt_asset=debt_asset,
            user=user,
            debt_to_cover=500000000000000000,
            liquidated_collateral=300000000000000000,
            log_index=100,
        )

        collateral_burn_event = EventFactory.create_collateral_burn_event(
            user=user,
            amount=300000000000000000,
            balance_increase=300000000000000000,
            log_index=104,
        )

        parser = TransactionOperationsParser(token_type_mapping=TEST_TOKEN_TYPE_MAPPING)
        tx_ops = parser.parse(
            [debt_mint_event, liquidation_event, collateral_burn_event],
            HexBytes("0x" + "00" * 32),
        )

        # Should have 1 operation (LIQUIDATION)
        assert len(tx_ops.operations) == 1
        op = tx_ops.operations[0]
        assert op.operation_type == OperationType.LIQUIDATION

        # Should have 0 debt burns and 1 collateral burn
        debt_burns = [e for e in op.scaled_token_events if e.is_debt]
        collateral_burns = [e for e in op.scaled_token_events if e.is_collateral]
        assert len(debt_burns) == 0
        assert len(collateral_burns) == 1

        # Validation should pass - no exception raised
        tx_ops.validate([debt_mint_event, liquidation_event, collateral_burn_event])

    def test_liquidation_with_interest_accrual_mint(self):
        """Liquidation correctly processes interest accrual mint events.

        Regression test for issue #0015 - Interest accrual mints during liquidation
        were being skipped, causing debt balance mismatches.
        Transaction: 0xcb087ea4d8d1b7c890318c3eccd7f730f24a1f1b55b25c156b9649e543de0588
        """
        user = get_checksum_address("0x09D86D566092bEc46D449e72087ee788937599D2")
        collateral_asset = get_checksum_address("0xc011a73ee8576fb46f5e1c5751ca3b9fe0af2a6f")
        debt_asset = get_checksum_address("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")

        # Interest accrual mint (balance_increase >= amount)
        interest_mint_event = EventFactory.create_debt_mint_event(
            user=user,
            amount=11627951177,
            balance_increase=33823939319,  # >= amount, indicates interest accrual
            log_index=102,
        )

        liquidation_event = EventFactory.create_liquidation_call_event(
            collateral_asset=collateral_asset,
            debt_asset=debt_asset,
            user=user,
            debt_to_cover=22195988142,
            liquidated_collateral=9048585995794641865737,
            log_index=100,
        )

        collateral_burn_event = EventFactory.create_collateral_burn_event(
            user=user,
            amount=9048585995794641865737,
            balance_increase=11005547540567006197,
            log_index=104,
        )

        parser = TransactionOperationsParser(token_type_mapping=TEST_TOKEN_TYPE_MAPPING)
        tx_ops = parser.parse(
            [interest_mint_event, liquidation_event, collateral_burn_event],
            HexBytes("0x" + "00" * 32),
        )

        # Should have 2 operations: INTEREST_ACCRUAL and LIQUIDATION
        assert len(tx_ops.operations) == 2

        # Find operations by type
        interest_ops = [
            op for op in tx_ops.operations if op.operation_type == OperationType.INTEREST_ACCRUAL
        ]
        liquidation_ops = [
            op for op in tx_ops.operations if op.operation_type == OperationType.LIQUIDATION
        ]

        assert len(interest_ops) == 1, (
            f"Expected 1 INTEREST_ACCRUAL operation, got {len(interest_ops)}"
        )
        assert len(liquidation_ops) == 1, (
            f"Expected 1 LIQUIDATION operation, got {len(liquidation_ops)}"
        )

        interest_op = interest_ops[0]
        assert len(interest_op.scaled_token_events) == 1

        liquidation_op = liquidation_ops[0]
        assert len(liquidation_op.scaled_token_events) == 1  # collateral burn

        # Validation should pass
        tx_ops.validate([interest_mint_event, liquidation_event, collateral_burn_event])

    def test_repay_with_zero_debt_burns_validates(self):
        """Interest-only repayment has 0 debt burns (only interest accrual mint)."""
        # Regression test for issue #0029
        # Transaction: 0x96b71f9698a072992a4e0a4ed1ade34c1872911dda9790d94946fa38360d302d
        user = get_checksum_address("0xE873793b15e6bEc6c7118D8125E40C122D46714D")
        reserve = get_checksum_address("0xdAC17F958D2ee523a2206206994597C13D831ec7")

        # Create events: interest accrual mint + repay (no burn since only interest covered)
        interest_mint_event = EventFactory.create_debt_mint_event(
            user=user,
            amount=26804,  # Scaled tokens minted as interest
            balance_increase=26904,
            log_index=183,
        )

        repay_event = EventFactory.create_repay_event(
            reserve=reserve,
            user=user,
            amount=100000000,  # 100 USDT (6 decimals)
            use_a_tokens=False,
            log_index=186,
        )

        parser = TransactionOperationsParser(token_type_mapping=TEST_TOKEN_TYPE_MAPPING)
        tx_ops = parser.parse(
            [interest_mint_event, repay_event],
            HexBytes("0x" + "00" * 32),
        )

        # When balance_increase > amount, the DEBT_MINT represents interest accrual
        # during repayment and is processed as a separate INTEREST_ACCRUAL operation
        assert len(tx_ops.operations) == 2
        repay_op = tx_ops.operations[0]
        interest_op = tx_ops.operations[1]

        assert repay_op.operation_type == OperationType.REPAY
        assert interest_op.operation_type == OperationType.INTEREST_ACCRUAL
        # Should have 0 debt burns in REPAY (interest-only repayment)
        debt_burns = [e for e in repay_op.scaled_token_events if e.is_debt]
        assert len(debt_burns) == 0
        # INTEREST_ACCRUAL should have the debt mint
        interest_mints = [e for e in interest_op.scaled_token_events if e.is_debt]
        assert len(interest_mints) == 1

        # Validation should pass - no exception raised
        tx_ops.validate([interest_mint_event, repay_event])
        assert repay_op.is_valid()
        assert interest_op.is_valid()

    def test_repay_with_one_debt_burn_validates(self):
        """Standard principal repayment has 1 debt burn."""
        user = get_checksum_address("0x1234567890123456789012345678901234567890")
        reserve = get_checksum_address("0xA0b86a33E6441e6C7D3D4B4b8B8B8B8B8B8B8B8B")

        debt_burn_event = EventFactory.create_debt_burn_event(
            user=user,
            amount=1000000000000000000,
            balance_increase=1000000000000000000,
            log_index=98,
        )

        repay_event = EventFactory.create_repay_event(
            reserve=reserve,
            user=user,
            amount=1000000000000000000,
            use_a_tokens=False,
            log_index=100,
        )

        parser = TransactionOperationsParser(token_type_mapping=TEST_TOKEN_TYPE_MAPPING)
        tx_ops = parser.parse(
            [debt_burn_event, repay_event],
            HexBytes("0x" + "00" * 32),
        )

        assert len(tx_ops.operations) == 1
        op = tx_ops.operations[0]

        assert op.operation_type == OperationType.REPAY
        # Should have 1 debt burn (principal repayment)
        debt_burns = [e for e in op.scaled_token_events if e.is_debt]
        assert len(debt_burns) == 1

        # Validation should pass with 1 burn - no exception raised
        tx_ops.validate([debt_burn_event, repay_event])
        assert op.is_valid()

    def test_liquidation_with_collateral_transfer_validates(self):
        """Liquidation where collateral is transferred to treasury instead of burned.

        Regression test for issue #0034
        Transaction: 0x621380dc92a951489f4717300d242ad2db640be2d2be5eb66e108b455cccaad2
        Block: 19904828

        In some liquidations, the protocol takes a liquidation fee by transferring
        collateral to the treasury via SCALED_TOKEN_BALANCE_TRANSFER instead of
        burning it. The validation must accept either burns OR transfers.
        """
        user = get_checksum_address("0x4835C915243Ea1d094B17f5E4115e371e4880717")
        treasury = get_checksum_address("0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c")
        collateral_asset = get_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
        debt_asset = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")

        # Interest accrual mint (happens before liquidation)
        interest_mint = EventFactory.create_collateral_mint_event(
            user=user,
            amount=4941689,  # Small interest amount
            balance_increase=4941689,
            log_index=7,
        )

        # Collateral transferred to treasury (liquidation fee)
        collateral_transfer = EventFactory.create_collateral_balance_transfer_event(
            from_user=user,
            to_user=treasury,
            amount=35,  # Small liquidation fee
            log_index=12,
        )

        liquidation_event = EventFactory.create_liquidation_call_event(
            collateral_asset=collateral_asset,
            debt_asset=debt_asset,
            user=user,
            debt_to_cover=1350043617,
            liquidated_collateral=4231,
            log_index=14,
        )

        parser = TransactionOperationsParser(token_type_mapping=TEST_TOKEN_TYPE_MAPPING)
        tx_ops = parser.parse(
            [interest_mint, collateral_transfer, liquidation_event],
            HexBytes("0x" + "00" * 32),
        )

        # Should have 2 operations: LIQUIDATION + INTEREST_ACCRUAL
        assert len(tx_ops.operations) == 2

        # Find the liquidation operation
        liq_ops = [op for op in tx_ops.operations if op.operation_type == OperationType.LIQUIDATION]
        assert len(liq_ops) == 1
        liq_op = liq_ops[0]

        # Should have 1 collateral event (the transfer, not a burn)
        collateral_events = [e for e in liq_op.scaled_token_events if e.is_collateral]
        assert len(collateral_events) == 1
        assert collateral_events[0].event_type == "COLLATERAL_TRANSFER"

        # Find the interest accrual operation
        interest_ops = [
            op for op in tx_ops.operations if op.operation_type == OperationType.INTEREST_ACCRUAL
        ]
        assert len(interest_ops) == 1
        interest_op = interest_ops[0]
        assert len(interest_op.scaled_token_events) == 1
        assert interest_op.scaled_token_events[0].event_type == "COLLATERAL_MINT"

        # Validation should pass
        tx_ops.validate([interest_mint, collateral_transfer, liquidation_event])
        assert liq_op.is_valid()
        assert interest_op.is_valid()

    def test_repay_with_atokens_zero_debt_events(self):
        """REPAY_WITH_ATOKENS with 0 debt events (interest-only repayment edge case).

        Edge case where accrued interest covers the debt, so no debt burn event
        is emitted by Aave's _burnScaled function. Transaction at 0x3482a0ec...
        demonstrated this behavior.
        """
        user = get_checksum_address("0x1234567890123456789012345678901234567890")
        reserve = get_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")

        # Create events - NO debt burn event (only interest mint + collateral burn + repay)
        interest_mint_event = EventFactory.create_debt_mint_event(
            user=user,
            amount=14224026,  # Interest accrued
            balance_increase=14224026,
            log_index=448,
        )

        collateral_burn_event = EventFactory.create_collateral_burn_event(
            user=user,
            amount=1905745357,  # ~1,904 USDC
            balance_increase=14224026,
            log_index=451,
        )

        repay_event = EventFactory.create_repay_event(
            reserve=reserve,
            user=user,
            amount=1905745357,
            use_a_tokens=True,
            log_index=452,
        )

        parser = TransactionOperationsParser(token_type_mapping=TEST_TOKEN_TYPE_MAPPING)
        tx_ops = parser.parse(
            [interest_mint_event, collateral_burn_event, repay_event],
            HexBytes("0x" + "00" * 32),
        )

        # Should have 2 operations: REPAY_WITH_ATOKENS + INTEREST_ACCRUAL
        assert len(tx_ops.operations) == 2

        # Find the repay operation
        repay_ops = [
            op for op in tx_ops.operations if op.operation_type == OperationType.REPAY_WITH_ATOKENS
        ]
        assert len(repay_ops) == 1
        repay_op = repay_ops[0]

        # Should have only 1 scaled token event (the collateral burn)
        # The interest mint is not matched to this operation
        assert len(repay_op.scaled_token_events) == 1
        assert repay_op.scaled_token_events[0].event_type == "COLLATERAL_BURN"

        # Find the interest accrual operation
        interest_ops = [
            op for op in tx_ops.operations if op.operation_type == OperationType.INTEREST_ACCRUAL
        ]
        assert len(interest_ops) == 1
        interest_op = interest_ops[0]
        assert len(interest_op.scaled_token_events) == 1
        assert interest_op.scaled_token_events[0].event_type == "DEBT_MINT"

        # Validation should pass
        tx_ops.validate([interest_mint_event, collateral_burn_event, repay_event])
        assert repay_op.is_valid()
        assert interest_op.is_valid()

    def test_gho_liquidation_dust_validates_with_zero_debt_burns(self):
        """Dust GHO liquidation validates with 0 GHO debt burns (only collateral transfer).

        Regression test for issue #0041 - Dust liquidations have debtToCover so small
        (effectively zero) that no GHO debt principal is burned. Only collateral is transferred.
        Transaction: 0x0ad468f0bd8e9b63a3cb464f27e686d28be9c3c54a7aee2791716388908cf769
        """
        user = get_checksum_address("0x1234567890123456789012345678901234567890")
        collateral_asset = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")

        # Dust liquidation pattern:
        # 1. LIQUIDATION_CALL pool event
        # 2. Collateral transfer to liquidator - assigned to liquidation
        # No GHO debt burn because debtToCover rounds to zero
        # Interest mints are separate operations not associated with liquidation

        liquidation_event = EventFactory.create_liquidation_call_event(
            collateral_asset=collateral_asset,
            debt_asset=GHO_TOKEN_ADDRESS,
            user=user,
            debt_to_cover=0,  # Dust amount
            liquidated_collateral=300000000000000000,
            log_index=404,
        )

        # For dust liquidation, collateral is transferred (not burned)
        collateral_transfer_event = EventFactory.create_collateral_balance_transfer_event(
            from_user=user,
            to_user=get_checksum_address("0xf00E2de0E78DFf055A92AD4719a179CE275b6Ef7"),
            amount=300000000000000000,
            log_index=400,
        )

        # Update the token type mapping to include the collateral asset
        test_mapping = TEST_TOKEN_TYPE_MAPPING.copy()
        test_mapping[collateral_asset] = "aToken"

        parser = TransactionOperationsParser(token_type_mapping=test_mapping)
        tx_ops = parser.parse(
            [liquidation_event, collateral_transfer_event],
            HexBytes("0x" + "00" * 32),
        )

        # Should have 1 operation (GHO_LIQUIDATION)
        assert len(tx_ops.operations) == 1
        op = tx_ops.operations[0]
        assert op.operation_type == OperationType.GHO_LIQUIDATION

        # Should have 0 GHO debt burns and 1 collateral transfer
        gho_burns = [e for e in op.scaled_token_events if e.event_type == "GHO_DEBT_BURN"]
        collateral_transfers = [
            e for e in op.scaled_token_events if e.event_type == "COLLATERAL_TRANSFER"
        ]
        assert len(gho_burns) == 0, "Dust liquidation should have 0 GHO debt burns"
        assert len(collateral_transfers) == 1, "Should have 1 collateral transfer"

        # Validation should pass - no exception raised
        tx_ops.validate([liquidation_event, collateral_transfer_event])
        assert op.is_valid()


class TestBug0013InterestAccrualMintMatching:
    """Test for Bug #0013: Interest accrual mint incorrectly matched to BORROW operation.

    Issue: When a transaction contains multiple GHO debt mint events, including
    an interest accrual mint (where value == balance_increase) and an actual
    borrow mint (where value > balance_increase), the parser incorrectly
    matched the interest accrual mint to the BORROW operation instead of the
    actual borrow mint.

    Reference: debug/aave/0013 - Interest Accrual Mint Matched to Borrow
    Transaction: 0x1116737166520b7c1dfb24a1f42c135fd37179fa6e9b016dcaa16419930a0743
    Block: 18076682
    User: 0x4bd5Eb24EB381DE15a168F213E16c32924Cd65D0
    """

    def test_gho_borrow_with_interest_accrual_mint(self):
        """Test that BORROW operation matches actual borrow mint, not interest accrual."""
        user = get_checksum_address("0x4bd5Eb24EB381DE15a168F213E16c32924Cd65D0")

        # Create debt token to reserve mapping for GHO
        debt_token_to_reserve = {
            GHO_VARIABLE_DEBT_TOKEN_ADDRESS: GHO_TOKEN_ADDRESS,
        }

        # First mint event: Interest accrual (value == balance_increase)
        # LogIndex 78 from actual transaction
        interest_accrual_mint = EventFactory.create_debt_mint_event(
            user=user,
            amount=974800826599076528,  # 0.97 GHO
            balance_increase=974800826599076528,  # Same as amount (interest accrual)
            log_index=78,
        )
        # Change address to GHO variable debt token
        interest_accrual_mint["address"] = GHO_VARIABLE_DEBT_TOKEN_ADDRESS

        # Third mint event: Actual borrow (value > balance_increase)
        # LogIndex 112 from actual transaction - the actual 1010 GHO borrow
        borrow_mint = EventFactory.create_debt_mint_event(
            user=user,
            amount=1010000000000000000000,  # 1010 GHO
            balance_increase=0,  # 0 (actual borrow)
            log_index=112,
        )
        # Change address to GHO variable debt token
        borrow_mint["address"] = GHO_VARIABLE_DEBT_TOKEN_ADDRESS

        # BORROW pool event
        # LogIndex 114 from actual transaction
        borrow_event = EventFactory.create_borrow_event(
            reserve=GHO_TOKEN_ADDRESS,
            user=user,
            on_behalf_of=user,
            amount=1000000000000000000000,  # 1000 GHO borrowed
            log_index=114,
        )

        # Create parser with debt token mapping
        parser = TransactionOperationsParser(
            token_type_mapping={},
            debt_token_to_reserve=debt_token_to_reserve,
        )

        events = [interest_accrual_mint, borrow_mint, borrow_event]
        tx_ops = parser.parse(events, HexBytes("0x" + "00" * 32))

        # Should have 2 operations:
        # 1. INTEREST_ACCRUAL for the interest accrual mint
        # 2. GHO_BORROW for the actual borrow
        assert len(tx_ops.operations) == 2, f"Expected 2 operations, got {len(tx_ops.operations)}"

        # Find the BORROW operation
        borrow_ops = [
            op for op in tx_ops.operations if op.operation_type == OperationType.GHO_BORROW
        ]
        assert len(borrow_ops) == 1, "Should have exactly 1 GHO_BORROW operation"
        borrow_op = borrow_ops[0]

        # The BORROW operation should have exactly 1 scaled token event
        assert len(borrow_op.scaled_token_events) == 1, "BORROW should have 1 scaled token event"

        # That event should be the actual borrow mint (logIndex 112), not interest accrual (logIndex 78)
        matched_mint = borrow_op.scaled_token_events[0]
        assert matched_mint.event["logIndex"] == 112, (
            f"BORROW should match mint at logIndex 112 (actual borrow), "
            f"but matched mint at logIndex {matched_mint.event['logIndex']}"
        )
        assert matched_mint.amount == 1010000000000000000000, (
            f"Matched mint should have amount=1010 GHO (actual borrow), "
            f"but got amount={matched_mint.amount}"
        )

        # Find the INTEREST_ACCRUAL operation
        interest_ops = [
            op for op in tx_ops.operations if op.operation_type == OperationType.INTEREST_ACCRUAL
        ]
        assert len(interest_ops) == 1, "Should have exactly 1 INTEREST_ACCRUAL operation"
        interest_op = interest_ops[0]

        # The INTEREST_ACCRUAL operation should have the interest accrual mint (logIndex 78)
        assert len(interest_op.scaled_token_events) == 1, (
            "INTEREST_ACCRUAL should have 1 scaled token event"
        )
        interest_mint = interest_op.scaled_token_events[0]
        assert interest_mint.event["logIndex"] == 78, (
            f"INTEREST_ACCRUAL should have mint at logIndex 78, "
            f"but has mint at logIndex {interest_mint.event['logIndex']}"
        )
        assert interest_mint.amount == interest_mint.balance_increase, (
            "Interest accrual mint should have amount == balance_increase"
        )

        # Validation should pass
        tx_ops.validate(events)


class TestBug0022BorrowAmountMatching:
    """Test for issue #0022: Borrow should match debt mint by amount.
    
    When multiple debt mints exist for the same user in a transaction,
    the borrow operation should match the mint with the same amount value.
    """

    def test_borrow_matches_debt_mint_by_amount(self):
        """BORROW operation matches DEBT_MINT with same amount.
        
        Regression test for issue #0022.
        Transaction: 0x37416a998da98779737e6c62607defcf9d0a7fbfd38651e54b8c058710eb3992
        
        When multiple debt mints exist for the same user/token,
        the borrow should match the mint with the same amount.
        """
        user = get_checksum_address("0xB22e3d2418C2B909C14883F35EA0BDcBA566e9c6")
        weth_reserve = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
        variable_debt_weth = get_checksum_address("0xeA51d7853EEFb32b6ee06b1C12E6dcCA88Be0fFE")
        
        # Token type mapping
        token_type_mapping = {
            variable_debt_weth: "vToken",
        }
        
        # Debt token to reserve mapping
        debt_token_to_reserve = {
            variable_debt_weth: weth_reserve,
        }
        
        # Small interest accrual mint (log 182 in original tx)
        # This has value != balance_increase (interest accrual before repay)
        small_mint = EventFactory.create_debt_mint_event(
            user=user,
            amount=266163817852323386,
            balance_increase=266164750831695501,  # Slightly larger than amount
            log_index=182,
            contract_address=variable_debt_weth,
        )
        
        # Large flash loan borrow mint (log 187 in original tx)
        # This has balance_increase=0 (pure borrow)
        large_mint = EventFactory.create_debt_mint_event(
            user=user,
            amount=614800334026855555114,
            balance_increase=0,
            log_index=187,
            contract_address=variable_debt_weth,
        )
        
        # Borrow event (log 189 in original tx)
        # Amount matches the large mint
        borrow_event = EventFactory.create_borrow_event(
            reserve=weth_reserve,
            user=user,
            on_behalf_of=user,
            amount=614800334026855555114,
            log_index=189,
        )
        
        parser = TransactionOperationsParser(
            token_type_mapping=token_type_mapping,
            debt_token_to_reserve=debt_token_to_reserve,
        )
        
        events = [small_mint, large_mint, borrow_event]
        tx_ops = parser.parse(events, HexBytes("0x" + "00" * 32))
        
        # Should have 2 operations: BORROW and INTEREST_ACCRUAL
        assert len(tx_ops.operations) == 2, f"Expected 2 operations, got {len(tx_ops.operations)}"
        
        # Find the BORROW operation
        borrow_ops = [
            op for op in tx_ops.operations 
            if op.operation_type == OperationType.BORROW
        ]
        assert len(borrow_ops) == 1, "Should have exactly 1 BORROW operation"
        
        borrow_op = borrow_ops[0]
        # Should have exactly 1 scaled token event
        assert len(borrow_op.scaled_token_events) == 1, "BORROW should have 1 scaled token event"
        
        # Should match the large mint (same amount as borrow)
        matched_mint = borrow_op.scaled_token_events[0]
        assert matched_mint.event["logIndex"] == 187, (
            f"BORROW should match mint at logIndex 187 (large borrow), "
            f"but matched mint at logIndex {matched_mint.event['logIndex']}"
        )
        assert matched_mint.amount == 614800334026855555114, (
            f"Matched mint should have amount=614800334026855555114, "
            f"but got amount={matched_mint.amount}"
        )
        
        # The small mint should be in a separate INTEREST_ACCRUAL operation
        interest_ops = [
            op for op in tx_ops.operations 
            if op.operation_type == OperationType.INTEREST_ACCRUAL
        ]
        assert len(interest_ops) == 1, "Should have exactly 1 INTEREST_ACCRUAL operation"
        assert len(interest_ops[0].scaled_token_events) == 1
        interest_mint = interest_ops[0].scaled_token_events[0]
        assert interest_mint.event["logIndex"] == 182, (
            f"INTEREST_ACCRUAL should have mint at logIndex 182, "
            f"but has mint at logIndex {interest_mint.event['logIndex']}"
        )
        assert interest_mint.amount == 266163817852323386
        
        # Validation should pass
        tx_ops.validate(events)

    def test_borrow_uses_fallback_when_no_exact_match(self):
        """BORROW falls back to first matching mint when no exact amount match."""
        user = get_checksum_address("0x" + "1" * 40)
        reserve = get_checksum_address("0x" + "2" * 40)
        debt_token = get_checksum_address("0x" + "3" * 40)
        
        token_type_mapping = {debt_token: "vToken"}
        debt_token_to_reserve = {debt_token: reserve}
        
        # Create mint with different amount than borrow
        mint = EventFactory.create_debt_mint_event(
            user=user,
            amount=1000,
            balance_increase=0,
            log_index=100,
            contract_address=debt_token,
        )
        
        # Create borrow with different amount (no exact match)
        borrow_event = EventFactory.create_borrow_event(
            reserve=reserve,
            user=user,
            on_behalf_of=user,
            amount=2000,  # Different from mint amount
            log_index=200,
        )
        
        parser = TransactionOperationsParser(
            token_type_mapping=token_type_mapping,
            debt_token_to_reserve=debt_token_to_reserve,
        )
        
        events = [mint, borrow_event]
        tx_ops = parser.parse(events, HexBytes("0x" + "00" * 32))
        
        # Should still create a BORROW operation using fallback
        borrow_ops = [
            op for op in tx_ops.operations 
            if op.operation_type == OperationType.BORROW
        ]
        assert len(borrow_ops) == 1, "Should have BORROW operation via fallback"
        
        # Should use the fallback mint (even though amounts don't match)
        borrow_op = borrow_ops[0]
        assert len(borrow_op.scaled_token_events) == 1
        
        # Validation should pass
        tx_ops.validate(events)
