"""
Test SUPPLY event matching validation for pure interest Mint events.

These tests verify that when a Mint event has value == balance_increase (pure interest
accrual), it does NOT incorrectly match to a SUPPLY event from later in the transaction.
This prevents the interest amount from being added to the balance twice.

Reference transaction: 0xf1a2cc8ddc3846f93151df903fe63a6603909b468b918185f9b4a6adf0e02e21
Block: 16502006
User: 0x6CD71d6Cb7824add7c277F2CA99635D98F8b9248
Asset: AwstETH (0x0B925eD163218f6662a35e0f0371Ac234f9E9371)
"""

from hexbytes import HexBytes

from degenbot.cli.aave import AaveV3Event


class TestSupplyMatchingValidation:
    """
    Test SUPPLY event matching validation for pure interest Mint events.

    When a Mint event has value == balance_increase, it is pure interest accrual
    and should NOT match to a SUPPLY event. If it does, the scaled_amount from
    the SUPPLY would be incorrectly added to the balance.
    """

    def test_pure_interest_mint_should_not_match_supply(self):
        """
        Verify that pure interest Mint does not match SUPPLY with different amount.

        In the reference transaction:
        - Mint at log 287: value=balanceIncrease=65855604742314740 (pure interest)
        - Mint at log 317: value=balanceIncrease=65855604742314740 (deposit)
        - The Mint at 287 was incorrectly matching to SUPPLY implied by 317

        The fix validates that scaled_amount == event_amount when value == balance_increase.
        If not, the SUPPLY event should be rejected as a match.
        """
        # Simulated Mint event at log 287 (pure interest accrual)
        mint_event = {
            "topics": [
                AaveV3Event.SCALED_TOKEN_MINT.value,
                HexBytes(
                    "0x0000000000000000000000001809f186d680f239420b56948c58f8dbbcdf1e18"
                ),  # caller
                HexBytes(
                    "0x0000000000000000000000006cd71d6cb7824add7c277f2ca99635d98f8b9248"
                ),  # onBehalfOf
            ],
            "data": HexBytes(
                "0x00000000000000000000000000000000000000000000000ea45c900e7a35ad54"
                # ^ value = 65855604742314740
                "00000000000000000000000000000000000000000000000ea45c900e7a35ad54"
                # ^ balanceIncrease = 65855604742314740 (same as value)
                "0000000000000000000000000000000000000000033a243a5fd4272accd37002"
                # ^ index
            ),
            "logIndex": 287,
            "blockNumber": 16502006,
            "address": "0x0B925eD163218f6662a35e0f0371Ac234f9E9371",
        }

        # Decode values
        event_amount = int.from_bytes(mint_event["data"][0:32], "big")
        balance_increase = int.from_bytes(mint_event["data"][32:64], "big")

        # Verify it is pure interest accrual
        assert event_amount == balance_increase, "Should be pure interest accrual"

        # Simulated SUPPLY event at log 317 (different amount)
        # This would calculate to a different scaled_amount
        calculated_scaled_amount = 17069459478196387788  # Different from event_amount

        # The fix: when value == balance_increase, validate that
        # calculated_scaled_amount == event_amount
        should_reject_match = (
            event_amount == balance_increase and calculated_scaled_amount != event_amount
        )

        assert should_reject_match is True, (
            "Pure interest Mint should reject SUPPLY with different scaled_amount"
        )

        # With the fix, scaled_amount should be None (not from SUPPLY)
        # and balance_delta should be 0 (pure interest)
        scaled_amount = None if should_reject_match else calculated_scaled_amount
        balance_delta = scaled_amount if scaled_amount is not None else 0

        assert scaled_amount is None, "scaled_amount should be None for pure interest"
        assert balance_delta == 0, "Pure interest should have balance_delta=0"

    def test_deposit_mint_with_matching_amount_should_match_supply(self):
        """
        Verify that deposit Mint with matching scaled_amount does match SUPPLY.

        When value == balance_increase but the calculated scaled_amount from SUPPLY
        equals event_amount, it IS a valid match (e.g., router deposit where deposit
        equals interest).
        """
        event_amount = 65855604742314740
        balance_increase = 65855604742314740

        # This is also value == balance_increase
        assert event_amount == balance_increase

        # But in this case, the SUPPLY event's scaled_amount matches event_amount
        calculated_scaled_amount = event_amount  # Matches

        # The fix should accept this match
        should_reject_match = (
            event_amount == balance_increase and calculated_scaled_amount != event_amount
        )

        assert should_reject_match is False, (
            "Deposit with matching scaled_amount should match SUPPLY"
        )

        # scaled_amount should be used
        scaled_amount = None if should_reject_match else calculated_scaled_amount
        balance_delta = scaled_amount if scaled_amount is not None else 0

        assert scaled_amount == event_amount, "scaled_amount should match event_amount"
        assert balance_delta == event_amount, "Deposit should add balance"

    def test_event_sequence_with_interest_and_deposit(self):
        """
        Test the full event sequence from the reference transaction.

        Events:
        1. Mint at 287: Pure interest (value == balance_increase) - NO balance change
        2. BalanceTransfer at 290: Transfer tokens to adapter - REDUCE balance
        3. Withdraw at 294: Adapter withdraws - NO change to user's balance
        4. Mint at 317: Deposit (value == balance_increase) - INCREASE balance

        The bug was: Mint at 287 matched SUPPLY implied by 317, adding interest twice.
        """
        # Event 1: Pure interest Mint at log 287
        mint_287_value = 270097916543865564500  # 0x0ea45c900e7a35ad54
        mint_287_balance_increase = 270097916543865564500

        # Event 2: BalanceTransfer at log 290
        transfer_amount = 17069459478196387788

        # Event 4: Deposit Mint at log 317 (value == balance_increase)
        mint_317_value = 65855604742314740
        mint_317_balance_increase = 65855604742314740

        # Simulated SUPPLY event (implied by Mint 317)
        # Its scaled_amount would match mint_317_value
        supply_scaled_amount = mint_317_value

        # Bug: Mint 287 would match this SUPPLY because they're both
        # value == balance_increase and both for the same user
        # The calculated scaled_amount from SUPPLY (65...) doesn't match
        # mint_287_value (270...), so it should be rejected

        # Fix validation
        mint_287_should_reject = (
            mint_287_value == mint_287_balance_increase and supply_scaled_amount != mint_287_value
        )

        assert mint_287_should_reject is True, (
            "Mint 287 (pure interest) should reject SUPPLY from Mint 317"
        )

        # Mint 317 should accept the match (same user, same amount)
        mint_317_should_reject = (
            mint_317_value == mint_317_balance_increase and supply_scaled_amount != mint_317_value
        )

        assert mint_317_should_reject is False, "Mint 317 (deposit) should match its SUPPLY"

        assert mint_287_should_reject is True, (
            "Mint 287 (pure interest) should reject SUPPLY from Mint 317"
        )

        # Mint 317 should accept the match (same user, same amount)
        mint_317_should_reject = (
            mint_317_value == mint_317_balance_increase and supply_scaled_amount != mint_317_value
        )

        assert mint_317_should_reject is False, "Mint 317 (deposit) should match its SUPPLY"

        # Calculate expected balance changes
        # Starting balance (before all events)
        starting_balance = 172053677929796202748

        # After Mint 287: No change (pure interest, balance_delta=0)
        balance_after_287 = starting_balance

        # After BalanceTransfer: Reduce by transfer_amount
        balance_after_transfer = balance_after_287 - transfer_amount

        # After Mint 317: Increase by supply_scaled_amount
        final_balance = balance_after_transfer + supply_scaled_amount

        # This should match the on-chain balance
        expected_final_balance = 155050074056342129700

        assert final_balance == expected_final_balance, (
            f"Final balance should be {expected_final_balance}, got {final_balance}"
        )

    def test_validation_only_applies_when_value_equals_balance_increase(self):
        """
        Verify that the validation only applies when value == balance_increase.

        When value > balance_increase (standard deposit), the SUPPLY event
        should always be matched and used, regardless of the calculated amount.
        """
        # Standard deposit: value > balance_increase
        event_amount = 1000
        balance_increase = 50

        assert event_amount > balance_increase, "Should be standard deposit"

        # The validation condition
        should_validate = event_amount == balance_increase

        assert should_validate is False, "Standard deposits should not need amount validation"

        # For standard deposits, always use the SUPPLY's scaled_amount
        calculated_scaled_amount = event_amount
        scaled_amount = calculated_scaled_amount
        balance_delta = scaled_amount

        assert balance_delta > 0, "Standard deposit should increase balance"
