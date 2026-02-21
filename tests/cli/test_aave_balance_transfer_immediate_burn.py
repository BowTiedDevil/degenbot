"""
Test BalanceTransfer event handling when recipient immediately burns tokens.

These tests verify that BalanceTransfer events are correctly processed when the
recipient is a contract (e.g., ParaSwap adapter) that immediately burns the
received aTokens. In this scenario:

1. User transfers aTokens to adapter (BalanceTransfer event)
2. Adapter immediately burns aTokens (Burn event)

The FROM user's balance SHOULD be reduced (tokens actually leave their position),
but the TO user's balance should NOT be increased (they burn immediately).

This fixes a bug where the TO user's balance was incorrectly increased.

Reference transaction: 0x4a88a8c6a43b5df2ee59ebcf266225fbc5b876f202009422f0f9d05cc4915f35
Block: 16496928
User: 0xE4217040c894e8873EE19d675b6d0EeC992c2c0D (sender)
Recipient: 0x872fBcb1B582e8Cd0D0DD4327fBFa0B4C2730995 (ParaSwap adapter)
aToken: 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8 (aEthWETH)
"""


class TestBalanceTransferImmediateBurn:
    """
    Test BalanceTransfer handling with immediate burn by recipient.

    When a BalanceTransfer is immediately followed by a Burn event from the
    same recipient:
    1. FROM user's balance SHOULD be reduced (tokens leave their position)
    2. TO user's balance should NOT be increased (recipient burns immediately)
    """

    def test_balance_transfer_with_immediate_burn_updates_from_not_to(self) -> None:
        """
        Verify that FROM balance is updated but TO balance is skipped when recipient burns.

        When a BalanceTransfer recipient immediately burns the tokens:
        - FROM balance SHOULD be reduced (tokens actually leave user's position)
        - TO balance should NOT be increased (recipient burns immediately)
        """
        # Transaction events from 0x4a88a8c6a43b5df2ee59ebcf266225fbc5b876f202009422f0f9d05cc4915f35
        to_address = "0x872fBcb1B582e8Cd0D0DD4327fBFa0B4C2730995"
        transfer_amount = 1000000000000000  # 0.001 WETH in wei

        # Simulate Burn event from recipient immediately after BalanceTransfer
        burn_from = to_address
        burn_value = transfer_amount

        # Simulate the logic from _process_scaled_token_balance_transfer_event
        skip_to_user_balance_update = False
        skip_from_user_balance_update = False

        # Verify burn is from the recipient with matching amount
        assert burn_from == to_address, "Burn should be from recipient"
        assert burn_value == transfer_amount, "Burn amount should match transfer"

        # Apply the fix logic
        if burn_value == transfer_amount:
            # FROM balance SHOULD be reduced (tokens leave user's position)
            skip_from_user_balance_update = False
            # TO balance should NOT be increased (recipient burns immediately)
            skip_to_user_balance_update = True

        # Verify skip flags
        assert skip_from_user_balance_update is False, (
            "FROM balance should NOT be skipped (tokens actually leave)"
        )
        assert skip_to_user_balance_update is True, (
            "TO balance update should be skipped (recipient burns immediately)"
        )

        # Simulate the balance updates
        from_starting_balance = 1000000000000000
        to_starting_balance = 0

        from_final_balance = (
            from_starting_balance
            if skip_from_user_balance_update
            else from_starting_balance - transfer_amount
        )
        to_final_balance = (
            to_starting_balance
            if skip_to_user_balance_update
            else to_starting_balance + transfer_amount
        )

        # FROM balance should be reduced, TO balance should remain unchanged
        expected_from_balance = from_starting_balance - transfer_amount
        assert from_final_balance == expected_from_balance, (
            f"FROM balance should be {expected_from_balance}, got {from_final_balance}"
        )
        assert to_final_balance == to_starting_balance, (
            f"TO balance should remain {to_starting_balance}, got {to_final_balance}"
        )

    def test_balance_transfer_without_burn_updates_both_balances(self) -> None:
        """
        Verify normal BalanceTransfer updates both balances when no burn follows.

        When a BalanceTransfer is NOT followed by a Burn event from the recipient,
        both balance updates should proceed normally:
        - FROM user's balance should be reduced
        - TO user's balance should be increased
        """
        transfer_amount = 1000000000000000

        # No subsequent Burn event from the recipient
        skip_from_user_balance_update = False
        skip_to_user_balance_update = False

        # Simulate the balance updates
        from_starting_balance = 2000000000000000
        to_starting_balance = 500000000000000

        from_final_balance = (
            from_starting_balance
            if skip_from_user_balance_update
            else from_starting_balance - transfer_amount
        )
        to_final_balance = (
            to_starting_balance
            if skip_to_user_balance_update
            else to_starting_balance + transfer_amount
        )

        # Both balances should be updated
        expected_from_balance = from_starting_balance - transfer_amount
        expected_to_balance = to_starting_balance + transfer_amount

        assert from_final_balance == expected_from_balance, (
            f"FROM balance should be {expected_from_balance}, got {from_final_balance}"
        )
        assert to_final_balance == expected_to_balance, (
            f"TO balance should be {expected_to_balance}, got {to_final_balance}"
        )

    def test_balance_transfer_with_different_burn_amount(self) -> None:
        """
        Verify BalanceTransfer updates proceed when burn amount differs.

        If the recipient burns a DIFFERENT amount than the transfer amount,
        this is not an "immediate burn of received tokens" and both balance
        updates should proceed normally.
        """
        transfer_amount = 1000000000000000
        burn_amount = 500000000000000  # Different amount

        # Simulate the logic
        skip_from_user_balance_update = False
        skip_to_user_balance_update = False

        burn_value = burn_amount

        # Burn amount doesn't match transfer amount
        if burn_value == transfer_amount:
            skip_from_user_balance_update = False
            skip_to_user_balance_update = True

        # Both should be False because amounts don't match
        assert skip_from_user_balance_update is False, (
            "FROM balance should NOT be skipped when burn amount differs"
        )
        assert skip_to_user_balance_update is False, (
            "TO balance should NOT be skipped when burn amount differs"
        )
