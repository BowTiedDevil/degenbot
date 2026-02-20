"""Test for V1 processor pure interest accrual fix.

This test verifies that the V1 collateral processor correctly handles Mint events
with value == balance_increase (pure interest accrual) by NOT adding to the balance.

Reference: Transaction 0x4111ba18b284d459bceb74b7dc9a0ed7a56c02a612c06eb27271d8a52cc99cd7
at block 16498211 where aETH interest accrual was being double-counted.
"""

from degenbot.aave.processors import (
    CollateralMintEvent,
    TokenProcessorFactory,
)


class TestV1PureInterestAccrual:
    """Test V1 processor handling of pure interest accrual Mint events."""

    def test_pure_interest_accrual_balance_delta_is_zero(self):
        """Test that pure interest accrual (value == balance_increase) adds 0 to balance.

        When a Mint event has value == balance_increase, it represents pure interest
        accrual. The interest tokens are minted via ERC20 Transfer event, so the Mint
        event should only update the index, not add to the scaled balance.

        Log 0x50 (80) from transaction 0x4111ba18... at block 16498211:
        - value = 167247177056
        - balance_increase = 167247177056
        - index = 1000014760459154860801583308
        """
        processor = TokenProcessorFactory.get_collateral_processor(1)

        event_data = CollateralMintEvent(
            value=167247177056,
            balance_increase=167247177056,
            index=1000014760459154860801583308,
        )

        result = processor.process_mint_event(
            event_data=event_data,
            previous_balance=19999704962418970004,
            previous_index=1000014760459154860801583308,
        )

        # For pure interest accrual, balance_delta should be 0
        assert result.balance_delta == 0, (
            f"Expected balance_delta 0 for pure interest accrual, got {result.balance_delta}"
        )

        # Index should be updated
        assert result.new_index == 1000014760459154860801583308

    def test_supply_mint_adds_to_balance(self):
        """Test that supply (value > balance_increase) correctly adds to balance.

        When a Mint event has value > balance_increase, it represents a supply
        operation where the user deposits underlying tokens.
        """
        processor = TokenProcessorFactory.get_collateral_processor(1)

        event_data = CollateralMintEvent(
            value=61986449,
            balance_increase=0,
            index=1000000000000000000000000000,
        )

        result = processor.process_mint_event(
            event_data=event_data,
            previous_balance=0,
            previous_index=0,
        )

        # For supply, balance_delta should equal the deposited amount (scaled)
        assert result.balance_delta == 61986449, (
            f"Expected balance_delta 61986449 for supply, got {result.balance_delta}"
        )

    def test_withdraw_interest_accrual_subtracts_from_balance(self):
        """Test that withdraw (balance_increase > value) correctly subtracts from balance.

        When a Mint event has balance_increase > value, it represents interest
        accrual during a withdrawal operation.
        """
        processor = TokenProcessorFactory.get_collateral_processor(1)

        # Example: balance_increase=100, value=50 means 50 net interest
        event_data = CollateralMintEvent(
            value=50,
            balance_increase=100,
            index=1000000000000000000000000000,
        )

        result = processor.process_mint_event(
            event_data=event_data,
            previous_balance=1000,
            previous_index=1000000000000000000000000000,
        )

        # For withdraw with interest, balance_delta should be negative
        # delta = -ray_div(balance_increase - value, index)
        # delta = -ray_div(50, 10^27) = -50
        assert result.balance_delta == -50, (
            f"Expected balance_delta -50 for withdraw with interest, got {result.balance_delta}"
        )
        assert result.is_repay is True
