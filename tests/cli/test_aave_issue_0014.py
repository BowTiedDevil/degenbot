"""Test GHO debt repay balance delta calculation.

This test verifies that GHO VariableDebtToken Mint events emitted from
_burnScaled (when interest > repayment) calculate the correct balance delta.

See debug/aave/0014 - GHO Debt Repay Balance Delta Calculation Bug.md for bug report.

When interest accrued exceeds the repayment amount:
- The contract emits a Mint event with value = balanceIncrease - amount
- The correct balance delta should be -(repayment_scaled + discount_scaled)
- NOT discount_scaled - repayment_scaled (the old buggy formula)

The bug caused the database balance to be incorrect when processing repayments
where interest > repayment, such as transaction 0xd08a1044fed4f8e998a2a97bed373627...
13803a64e1b56c4ef2e29a0057cf08f2 at block 18240233.
"""

from degenbot.aave.processors.base import DebtMintEvent, GhoUserOperation
from degenbot.aave.processors.debt.gho.v1 import GhoV1Processor
from degenbot.aave.processors.debt.gho.v2 import GhoV2Processor
from degenbot.checksum_cache import get_checksum_address


class TestGHODebtRepayBalanceDelta:
    """Test balance delta calculation for GHO debt repay with interest > repayment."""

    def test_repay_with_interest_exceeding_repayment(self):
        """Test balance delta when interest > repayment (net mint scenario).

        This reproduces the bug from transaction 0xd08a1044fed4f8e998a2a97bed373627...
        13803a64e1b56c4ef2e29a0057cf08f2 at block 18240233 where:
        - User repaid 24.04 GHO
        - Interest accrued was 46.60 GHO
        - Contract burns (amount_scaled + discount_scaled) from scaled balance

        The bug caused the code to use: balance_delta = discount_scaled - repayment_scaled
        The correct formula is: balance_delta = -(repayment_scaled + discount_scaled)
        """
        processor = GhoV2Processor()

        # Values from the actual failing transaction
        # Mint event: value = 22556776016625358317, balanceIncrease = 46601036735819693373
        # This means: amount_repaid = balanceIncrease - value = 24044260719194335056
        # Which matches the Repay event amount: 24044260719194335056 GHO

        mint_value = 22556776016625358317  # Net interest after repayment
        balance_increase = 46601036735819693373  # Interest accrued (after discount)
        index = 1003370062812878921100054929  # Variable borrow index
        user_address = get_checksum_address("0x5b85B47670778b204041D6457dB8b5F5D36fa97a")

        # Previous state (before transaction)
        previous_balance = 75403592044877533741860
        previous_index = 1000000000000000000000000000
        previous_discount = 0  # Assume 0 discount for simplicity

        event_data = DebtMintEvent(
            caller=user_address,
            on_behalf_of=user_address,
            value=mint_value,
            balance_increase=balance_increase,
            index=index,
        )

        result = processor.process_mint_event(
            event_data=event_data,
            previous_balance=previous_balance,
            previous_index=previous_index,
            previous_discount=previous_discount,
        )

        # Should be a REPAY operation
        assert result.user_operation == GhoUserOperation.GHO_REPAY, (
            f"Expected GHO_REPAY, got {result.user_operation}"
        )

        # Calculate expected balance delta
        ray = 10**27
        amount_repaid = balance_increase - mint_value
        repayment_scaled = (amount_repaid * ray) // index

        # With 0 discount, balance_delta should be -repayment_scaled
        expected_balance_delta = -repayment_scaled

        assert result.balance_delta == expected_balance_delta, (
            f"Expected balance_delta={expected_balance_delta}, got {result.balance_delta}\n"
            f"This indicates the bug is present."
        )

        # The balance should DECREASE (we're burning tokens)
        assert result.balance_delta < 0, (
            f"Expected negative balance_delta (burning), got {result.balance_delta}"
        )

    def test_repay_with_discount_and_interest_exceeding(self):
        """Test balance delta with non-zero discount when interest > repayment."""
        processor = GhoV2Processor()

        mint_value = 22556776016625358317
        balance_increase = 46601036735819693373
        index = 1003370062812878921100054929
        user_address = get_checksum_address("0x5b85B47670778b204041D6457dB8b5F5D36fa97a")

        previous_balance = 75403592044877533741860
        previous_index = 1000000000000000000000000000
        previous_discount = 1000  # 10% discount (in basis points where 10000 = 100%)

        event_data = DebtMintEvent(
            caller=user_address,
            on_behalf_of=user_address,
            value=mint_value,
            balance_increase=balance_increase,
            index=index,
        )

        result = processor.process_mint_event(
            event_data=event_data,
            previous_balance=previous_balance,
            previous_index=previous_index,
            previous_discount=previous_discount,
        )

        # Should be REPAY operation
        assert result.user_operation == GhoUserOperation.GHO_REPAY

        # Calculate expected balance delta
        ray = 10**27
        amount_repaid = balance_increase - mint_value
        repayment_scaled = (amount_repaid * ray) // index

        # The correct formula is: -(repayment_scaled + discount_scaled)
        expected_balance_delta = -(repayment_scaled + result.discount_scaled)

        assert result.balance_delta == expected_balance_delta, (
            f"Expected balance_delta={expected_balance_delta}, got {result.balance_delta}"
        )


class TestGHOV1ProcessorRepay:
    """Test GHO v1 processor has same fix."""

    def test_v1_repay_with_interest_exceeding(self):
        """Test v1 processor also has correct formula."""
        processor = GhoV1Processor()

        mint_value = 22556776016625358317
        balance_increase = 46601036735819693373
        index = 1003370062812878921100054929
        user_address = get_checksum_address("0x5b85B47670778b204041D6457dB8b5F5D36fa97a")

        previous_balance = 75403592044877533741860
        previous_index = 1000000000000000000000000000
        previous_discount = 0

        event_data = DebtMintEvent(
            caller=user_address,
            on_behalf_of=user_address,
            value=mint_value,
            balance_increase=balance_increase,
            index=index,
        )

        result = processor.process_mint_event(
            event_data=event_data,
            previous_balance=previous_balance,
            previous_index=previous_index,
            previous_discount=previous_discount,
        )

        # Should be REPAY operation
        assert result.user_operation == GhoUserOperation.GHO_REPAY

        # Calculate expected balance delta
        ray = 10**27
        amount_repaid = balance_increase - mint_value
        repayment_scaled = (amount_repaid * ray) // index

        # With 0 discount, balance_delta should be -repayment_scaled
        expected_balance_delta = -repayment_scaled

        assert result.balance_delta == expected_balance_delta, (
            f"V1 processor also has bug. Expected {expected_balance_delta}, "
            f"got {result.balance_delta}"
        )
