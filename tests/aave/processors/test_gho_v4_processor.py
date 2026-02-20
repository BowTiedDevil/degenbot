"""Tests for GHO V4 processor.

Verifies correct rounding behavior for GHO variable debt token processing.
Based on actual transaction at block 23088738.
"""

import pytest
from eth_typing import ChecksumAddress

from degenbot.aave.processors import DebtMintEvent, TokenProcessorFactory
from degenbot.aave.processors.base import GhoDebtTokenProcessor, GhoUserOperation
from degenbot.checksum_cache import get_checksum_address

USER_ADDRESS: ChecksumAddress = get_checksum_address("0xbfb496ACb99299e9eCE84B3FD1B3fDd0f6CDDf49")


class TestGhoV4Processor:
    """Test GHO V4 processor with real transaction data.

    Transaction: 0x0e5b6ac766e85cfcf57cb1007840b750a44d2a771baf8228119df460936acf83
    Block: 23088738
    User: 0xbfb496ACb99299e9eCE84B3FD1B3fDd0f6CDDf49
    Asset: vGHO (0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B, revision 6)
    """

    @pytest.fixture
    def processor(self) -> GhoDebtTokenProcessor:
        """Create GHO V4 processor for revision 6."""
        return TokenProcessorFactory.get_gho_debt_processor(revision=6)

    def test_borrow_event_rounding(self, processor: GhoDebtTokenProcessor):
        """Test GHO BORROW uses floor rounding, not ceiling.

        This test verifies the fix for the issue where ray_div_ceil was
        incorrectly used instead of ray_div_floor, causing 1 wei discrepancies.

        Event data from block 23088738:
        - value: 50,043,781,461,041,674,422,932
        - balanceIncrease: 43,781,461,041,674,422,931
        - index: 1,143,509,431,396,222,220,498,421,265
        - Starting balance: 87,488,374,572,379,750,125,616
        - Expected ending balance (from contract): 131,213,418,395,542,732,018,851
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
        # Using ray_div_floor should give: 43,725,043,823,162,981,893,235
        # Using ray_div_ceil would give: 43,725,043,823,162,981,893,236 (1 wei more)
        expected_delta = 43725043823162981893235
        assert result.balance_delta == expected_delta, (
            f"Expected delta {expected_delta}, got {result.balance_delta}. "
            f"Difference: {result.balance_delta - expected_delta}"
        )

        # Verify the ending balance matches contract
        expected_ending_balance = 131213418395542732018851
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
        scaled_delta = ray_div_floor(requested_amount, index)
        """
        # The exact requested amount: 50,000 GHO + interest + 1 wei
        value = 50043781461041674422932
        balance_increase = 43781461041674422931
        requested_amount = value - balance_increase

        index = 1143509431396222220498421265

        # Manual calculation with ray_div_floor formula
        ray_value = 10**27
        numerator = requested_amount * ray_value
        expected_scaled = numerator // index

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

        This test documents why the fix was necessary - to show the difference
        between the incorrect (ceiling) and correct (floor) rounding.
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

        # Verify processor uses floor
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

        assert result.balance_delta == floor, "Processor should use floor rounding"
        assert result.balance_delta != ceiling, "Processor should NOT use ceiling rounding"
