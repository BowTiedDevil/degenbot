"""Tests for GHO V5+ processor.

Verifies correct rounding behavior for GHO variable debt token processing.
Based on actual transaction at block 23088738.
"""

import pytest
from eth_typing import ChecksumAddress

from degenbot.aave.processors import DebtMintEvent, TokenProcessorFactory
from degenbot.aave.processors.base import GhoDebtTokenProcessor, GhoUserOperation
from degenbot.checksum_cache import get_checksum_address

USER_ADDRESS: ChecksumAddress = get_checksum_address("0xbfb496ACb99299e9eCE84B3FD1B3fDd0f6CDDf49")


class TestGhoV5Processor:
    """Test GHO V5+ processor with real transaction data.

    Transaction: 0x0e5b6ac766e85cfcf57cb1007840b750a44d2a771baf8228119df460936acf83
    Block: 23088738
    User: 0xbfb496ACb99299e9eCE84B3FD1B3fDd0f6CDDf49
    Asset: vGHO (0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B, revision 6)
    """

    @pytest.fixture
    def processor(self) -> GhoDebtTokenProcessor:
        """Create GHO V5 processor for revision 6."""
        return TokenProcessorFactory.get_gho_debt_processor(revision=6)

    def test_borrow_event_rounding(self, processor: GhoDebtTokenProcessor):
        """Test GHO BORROW uses ceiling rounding.

        Revision 5+ uses ceiling division (ray_div_ceil) for BORROW to match
        TokenMath.getVTokenMintScaledAmount behavior. This ensures the protocol
        never underaccounts the user's debt.

        Event data from block 23088738:
        - value: 50,043,781,461,041,674,422,932
        - balanceIncrease: 43,781,461,041,674,422,931
        - index: 1,143,509,431,396,222,220,498,421,265
        - Starting balance: 87,488,374,572,379,750,125,616
        - Expected ending balance (from contract): 131,213,418,395,542,732,018,852
        """
        # Event values from actual transaction
        event_data = DebtMintEvent(
            caller=USER_ADDRESS,
            on_behalf_of=USER_ADDRESS,
            value=50043781461041674422932,
            balance_increase=43781461041674422931,
            index=1143509431396222220498421265,
        )

        # Previous balance from database before processing this event
        previous_balance = 87488374572379750125616
        previous_index = 0  # Not used for borrow
        previous_discount = 0  # No discount in rev 6

        result = processor.process_mint_event(
            event_data=event_data,
            previous_balance=previous_balance,
            previous_index=previous_index,
            previous_discount=previous_discount,
        )

        # Verify operation type
        assert result.user_operation == GhoUserOperation.GHO_BORROW

        # Verify the calculated delta matches on-chain result
        # Using ray_div_ceil gives: 43,725,043,823,162,981,893,236
        # Using ray_div_floor would give: 43,725,043,823,162,981,893,235 (1 wei less)
        expected_delta = 43725043823162981893236
        assert result.balance_delta == expected_delta, (
            f"Expected delta {expected_delta}, got {result.balance_delta}. "
            f"Difference: {result.balance_delta - expected_delta}"
        )

        # Verify the ending balance matches contract
        expected_ending_balance = 131213418395542732018852
        actual_ending_balance = previous_balance + result.balance_delta
        diff = actual_ending_balance - expected_ending_balance
        assert actual_ending_balance == expected_ending_balance, (
            f"Expected ending balance {expected_ending_balance}, "
            f"got {actual_ending_balance}. Difference: {diff}"
        )

    def test_borrow_calculation_determinism(self, processor):
        """Test that borrow calculation is deterministic and correct.

        Verifies the exact math for:
        requested_amount = value - balance_increase = 50,000 GHO + 1 wei
        scaled_delta = ray_div_ceil(requested_amount, index)
        """
        # The exact requested amount: 50,000 GHO + interest + 1 wei
        value = 50043781461041674422932
        balance_increase = 43781461041674422931
        requested_amount = value - balance_increase

        index = 1143509431396222220498421265

        # Manual calculation with ray_div_ceil formula
        ray_value = 10**27
        numerator = requested_amount * ray_value
        floor_result = numerator // index
        # Ceiling rounds up if there's any remainder
        expected_scaled = floor_result + (1 if numerator % index != 0 else 0)

        # Verify against processor
        event_data = DebtMintEvent(
            caller=USER_ADDRESS,
            on_behalf_of=USER_ADDRESS,
            value=value,
            balance_increase=balance_increase,
            index=index,
        )

        result = processor.process_mint_event(
            event_data=event_data,
            previous_balance=0,
            previous_index=0,
            previous_discount=0,
        )

        assert result.balance_delta == expected_scaled

    def test_ceil_vs_floor_difference(self, processor):
        """Test that ceiling and floor give different results for this case.

        This test documents the difference between floor and ceiling rounding.
        Revision 5+ uses ceiling for BORROW (mint) to ensure the protocol
        never underaccounts the user's debt.
        """
        value = 50043781461041674422932
        balance_increase = 43781461041674422931
        requested_amount = value - balance_increase
        index = 1143509431396222220498421265

        wad_ray_math = processor.get_math_libraries()["wad_ray"]

        floor = wad_ray_math.ray_div_floor(requested_amount, index)
        ceiling = wad_ray_math.ray_div_ceil(requested_amount, index)

        # They should differ by at least 1 wei
        assert ceiling >= floor + 1, (
            f"Expected ceiling ({ceiling}) to be at least 1 more than floor ({floor})"
        )

        # Verify processor uses ceiling for BORROW
        event_data = DebtMintEvent(
            caller=USER_ADDRESS,
            on_behalf_of=USER_ADDRESS,
            value=value,
            balance_increase=balance_increase,
            index=index,
        )

        result = processor.process_mint_event(
            event_data=event_data,
            previous_balance=0,
            previous_index=0,
            previous_discount=0,
        )

        assert result.balance_delta == ceiling, "Processor should use ceiling rounding for BORROW"
        assert result.balance_delta != floor, "Processor should NOT use floor rounding for BORROW"

    def test_calculate_mint_scaled_amount(self, processor):
        """Test that calculate_mint_scaled_amount uses ceiling division."""
        requested_amount = 50000000000000000000001
        index = 1143509431396222220498421265

        wad_ray_math = processor.get_math_libraries()["wad_ray"]

        expected_ceil = wad_ray_math.ray_div_ceil(requested_amount, index)
        actual = processor.calculate_mint_scaled_amount(requested_amount, index)

        assert actual == expected_ceil, (
            f"calculate_mint_scaled_amount should use ceiling division. "
            f"Expected {expected_ceil}, got {actual}"
        )

    def test_calculate_burn_scaled_amount(self, processor):
        """Test that calculate_burn_scaled_amount uses floor division."""
        requested_amount = 50000000000000000000001
        index = 1143509431396222220498421265

        wad_ray_math = processor.get_math_libraries()["wad_ray"]

        expected_floor = wad_ray_math.ray_div_floor(requested_amount, index)
        actual = processor.calculate_burn_scaled_amount(requested_amount, index)

        assert actual == expected_floor, (
            f"calculate_burn_scaled_amount should use floor division. "
            f"Expected {expected_floor}, got {actual}"
        )
