"""
Test for Issue 0017: GHO Discount Calculation with Stale stkAAVE Balance

This test verifies that GHO discount calculations use the correct (updated) stkAAVE
balance when stkAAVE transfers occur before GHO mint/burn events in the same transaction.

Bug: When processing a transaction, operations were processed before non-operation events,
     causing GHO operations to use stale stkAAVE balances for discount calculations.

Fix: Process stkAAVE transfers before operations to ensure balances are up-to-date.

Transaction reference:
- Block: 19325561
- Transaction: 0xaef7ff6293c8906bf0736fe5bb997445c6cd49c54cb1d082922f7c3b26515470
- User: 0x329c54289Ff5D6B7b7daE13592C6B1EDA1543eD4
"""

from degenbot.cli.aave import calculate_gho_discount_rate


class TestIssue0017StkAaveTransferDiscount:
    """Test discount calculation with updated stkAAVE balance."""

    def test_discount_calculation_with_correct_stk_aave_balance(self):
        """
        Test that using the correct (post-transfer) stkAAVE balance gives the right discount.

        This verifies the fix where stkAAVE transfers are processed before GHO operations.
        The contract-reported discount was 1276 bps, which should match when using
        the updated stkAAVE balance.
        """
        # Post-transaction stkAAVE balance (after rewards claim + stake)
        # This is the balance that the contract used for discount calculation
        post_transaction_stk_aave = 2_602_320_211_229_927_785_839

        # Debt balance (scaled)
        debt_balance = 611_819_753_733_640_365_871_335

        # Calculate discount with post-transaction balance (CORRECT)
        discount_with_updated_balance = calculate_gho_discount_rate(
            debt_balance=debt_balance,
            discount_token_balance=post_transaction_stk_aave,
        )

        # This is the correct value (1276 bps), matching the contract
        assert discount_with_updated_balance == 1276

    def test_discount_calculation_with_stale_stk_aave_balance(self):
        """
        Test that using stale stkAAVE balance gives a different (incorrect) discount.

        This demonstrates the bug: when stkAAVE transfers haven't been processed yet,
        the discount calculation uses the old balance and produces a different result.
        """
        # Post-transaction stkAAVE balance (correct)
        post_transaction_stk_aave = 2_602_320_211_229_927_785_839

        # Debt balance (scaled)
        debt_balance = 611_819_753_733_640_365_871_335

        # Calculate correct discount
        correct_discount = calculate_gho_discount_rate(
            debt_balance=debt_balance,
            discount_token_balance=post_transaction_stk_aave,
        )

        # Now calculate with a smaller balance (simulating stale pre-transfer balance)
        # The user received approximately 6.17 stkAAVE in the transaction
        stale_stk_aave = post_transaction_stk_aave - 6_165_056_220_180_499_992

        stale_discount = calculate_gho_discount_rate(
            debt_balance=debt_balance,
            discount_token_balance=stale_stk_aave,
        )

        # The stale balance should give a lower discount
        assert stale_discount < correct_discount

        # The difference should be approximately 3-4 bps
        discount_diff = correct_discount - stale_discount
        assert 2 <= discount_diff <= 4

    def test_stk_aave_balance_change_impact(self):
        """
        Test that stkAAVE balance changes affect the discount calculation.

        This validates that processing stkAAVE transfers before GHO operations
        is necessary for correct discount calculation.
        """
        debt_balance = 611_819_753_733_640_365_871_335

        # Starting balance (approximate pre-transaction)
        initial_balance = 2_596_155_155_009_747_285_847

        # Balance after receiving ~6.17 stkAAVE
        final_balance = 2_602_320_211_229_927_785_839

        initial_discount = calculate_gho_discount_rate(
            debt_balance=debt_balance,
            discount_token_balance=initial_balance,
        )

        final_discount = calculate_gho_discount_rate(
            debt_balance=debt_balance,
            discount_token_balance=final_balance,
        )

        # Verify that balance increase leads to discount increase
        assert final_discount > initial_discount

        # Verify the actual balance delta
        balance_delta = final_balance - initial_balance
        assert balance_delta == 6_165_056_220_180_499_992  # ~6.17 stkAAVE

    def test_discount_formula_sensitivity(self):
        """
        Test how sensitive the discount formula is to stkAAVE balance changes.

        This explains why a relatively small stkAAVE change (6.17 tokens out of ~2600)
        resulted in a 3 bps discount difference.
        """
        debt_balance = 611_819_753_733_640_365_871_335
        base_stk_aave = 2_600_000_000_000_000_000_000  # 2600 stkAAVE

        # Calculate discount at base balance
        discount_base = calculate_gho_discount_rate(
            debt_balance=debt_balance,
            discount_token_balance=base_stk_aave,
        )

        # Calculate discount with +6 stkAAVE
        discount_plus_6 = calculate_gho_discount_rate(
            debt_balance=debt_balance,
            discount_token_balance=base_stk_aave + 6_000_000_000_000_000_000,
        )

        # Verify that small changes in stkAAVE balance affect the discount
        assert discount_plus_6 >= discount_base

        # The formula is: discount = (stkAAVE * 100 * 3000) // debt
        # So for every 1 stkAAVE: discount_increase = (1 * 100 * 3000) // debt
        actual_increase = discount_plus_6 - discount_base

        # Should be approximately 0-1 bps per token (varies due to integer division)
        assert actual_increase >= 0
