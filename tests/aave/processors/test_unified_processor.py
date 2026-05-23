"""Tests for unified token processor implementation.

These tests verify that the strategy tables capture the correct rounding behavior
for each revision and that the unified processor produces identical results
to the original revision-specific processors.
"""

import pytest

from degenbot.aave.processors.base import (
    CollateralBurnEvent,
    CollateralMintEvent,
    DebtBurnEvent,
    DebtMintEvent,
)
from degenbot.aave.processors.factory import TokenProcessorFactory
from degenbot.aave.processors.strategies import (
    COLLATERAL_STRATEGIES,
    DEBT_STRATEGIES,
    GHO_DISCOUNT_STRATEGIES,
    GHO_STRATEGIES,
    RoundingMode,
)

# Test constants
RAY = 10**27
INDEX = 2 * RAY  # Index of 2.0


class TestCollateralStrategies:
    """Test that collateral processor rounding matches strategy tables."""

    @pytest.mark.parametrize("revision", [1, 2, 3, 4, 5])
    def test_collateral_revision_has_strategy(self, revision: int) -> None:
        """Every supported collateral revision has a strategy."""
        assert revision in COLLATERAL_STRATEGIES

    @pytest.mark.parametrize(
        "revision,expected_mint,expected_burn",
        [
            (1, RoundingMode.HALF_UP, RoundingMode.HALF_UP),
            (2, RoundingMode.HALF_UP, RoundingMode.HALF_UP),
            (3, RoundingMode.HALF_UP, RoundingMode.HALF_UP),
            (4, RoundingMode.HALF_UP, RoundingMode.CEIL),
            (5, RoundingMode.HALF_UP, RoundingMode.CEIL),
        ],
    )
    def test_collateral_strategy_rounding(
        self,
        revision: int,
        expected_mint: RoundingMode,
        expected_burn: RoundingMode,
    ) -> None:
        """Verify collateral strategy rounding modes match expected values."""
        strategy = COLLATERAL_STRATEGIES[revision]
        assert strategy.mint_rounding == expected_mint
        assert strategy.burn_rounding == expected_burn

    @pytest.mark.parametrize("revision", [1, 3, 4, 5])
    def test_collateral_mint_supply_uses_correct_rounding(self, revision: int) -> None:
        """Collateral mint for supply uses the mint rounding mode.

        For a SUPPLY, value > balance_increase, so we calculate:
        scaled_delta = (value - balance_increase) / index
        """
        processor = TokenProcessorFactory.get_collateral_processor(revision)
        strategy = COLLATERAL_STRATEGIES[revision]

        # Amount that will produce a non-integer scaled value
        # 1000000000000000001 / 2 = 500000000000000000.5
        raw_amount = 1000000000000000001

        event = CollateralMintEvent(
            value=raw_amount + 100,  # balance_increase + raw_amount
            balance_increase=100,
            index=INDEX,
        )

        result = processor.process_mint_event(
            event_data=event,
            previous_balance=0,
            previous_index=INDEX,
        )

        # Calculate expected based on rounding mode
        if strategy.mint_rounding == RoundingMode.HALF_UP:
            # 500000000000000000.5 rounds up to 500000000000000001
            expected_delta = 500000000000000001
        elif strategy.mint_rounding == RoundingMode.FLOOR:
            expected_delta = 500000000000000000
        else:  # CEIL
            expected_delta = 500000000000000001

        assert result.balance_delta == expected_delta

    @pytest.mark.parametrize("revision", [1, 3, 4, 5])
    def test_collateral_burn_uses_correct_rounding(self, revision: int) -> None:
        """Collateral burn uses the burn rounding mode.

        For a WITHDRAW, we calculate:
        scaled_delta = (value + balance_increase) / index
        """
        processor = TokenProcessorFactory.get_collateral_processor(revision)
        strategy = COLLATERAL_STRATEGIES[revision]

        # Amount that will produce a non-integer scaled value
        raw_amount = 1000000000000000001

        event = CollateralBurnEvent(
            value=raw_amount - 100,  # total to burn = raw_amount
            balance_increase=100,
            index=INDEX,
        )

        result = processor.process_burn_event(
            event_data=event,
            previous_balance=1000000000000000000,
            previous_index=INDEX,
        )

        # Calculate expected based on rounding mode
        if strategy.burn_rounding == RoundingMode.HALF_UP:
            # 500000000000000000.5 rounds up to 500000000000000001
            expected_delta = -500000000000000001
        elif strategy.burn_rounding == RoundingMode.FLOOR:
            expected_delta = -500000000000000000
        else:  # CEIL
            expected_delta = -500000000000000001

        assert result.balance_delta == expected_delta


class TestDebtStrategies:
    """Test that debt processor rounding matches strategy tables."""

    @pytest.mark.parametrize("revision", [1, 2, 3, 4, 5])
    def test_debt_revision_has_strategy(self, revision: int) -> None:
        """Every supported debt revision has a strategy."""
        assert revision in DEBT_STRATEGIES

    @pytest.mark.parametrize(
        "revision,expected_mint,expected_burn",
        [
            (1, RoundingMode.HALF_UP, RoundingMode.HALF_UP),
            (2, RoundingMode.HALF_UP, RoundingMode.HALF_UP),
            (3, RoundingMode.HALF_UP, RoundingMode.HALF_UP),
            (4, RoundingMode.CEIL, RoundingMode.FLOOR),
            (5, RoundingMode.CEIL, RoundingMode.FLOOR),
        ],
    )
    def test_debt_strategy_rounding(
        self,
        revision: int,
        expected_mint: RoundingMode,
        expected_burn: RoundingMode,
    ) -> None:
        """Verify debt strategy rounding modes match expected values."""
        strategy = DEBT_STRATEGIES[revision]
        assert strategy.mint_rounding == expected_mint
        assert strategy.burn_rounding == expected_burn

    @pytest.mark.parametrize("revision", [1, 3, 4, 5])
    def test_debt_mint_borrow_uses_correct_rounding(self, revision: int) -> None:
        """Debt mint for BORROW uses the mint rounding mode.

        For a BORROW, value > balance_increase, so we calculate:
        scaled_delta = (value - balance_increase) / index
        """
        processor = TokenProcessorFactory.get_debt_processor(revision)
        strategy = DEBT_STRATEGIES[revision]

        # Amount that will produce a non-integer scaled value
        raw_amount = 1000000000000000001

        event = DebtMintEvent(
            caller="0x" + "00" * 20,
            on_behalf_of="0x" + "00" * 20,
            value=raw_amount + 100,  # balance_increase + raw_amount
            balance_increase=100,
            index=INDEX,
        )

        result = processor.process_mint_event(
            event_data=event,
            previous_balance=0,
            previous_index=INDEX,
        )

        # Calculate expected based on rounding mode
        if strategy.mint_rounding == RoundingMode.HALF_UP:
            # 500000000000000000.5 rounds up to 500000000000000001
            expected_delta = 500000000000000001
        elif strategy.mint_rounding == RoundingMode.FLOOR:
            expected_delta = 500000000000000000
        else:  # CEIL
            expected_delta = 500000000000000001

        assert result.balance_delta == expected_delta
        assert result.is_repay is False

    @pytest.mark.parametrize("revision", [1, 3, 4, 5])
    def test_debt_burn_uses_correct_rounding(self, revision: int) -> None:
        """Debt burn uses the burn rounding mode.

        For a REPAY, we calculate:
        scaled_delta = (value + balance_increase) / index
        """
        processor = TokenProcessorFactory.get_debt_processor(revision)
        strategy = DEBT_STRATEGIES[revision]

        # Amount that will produce a non-integer scaled value
        raw_amount = 1000000000000000001

        event = DebtBurnEvent(
            from_="0x" + "00" * 20,
            target="0x" + "00" * 20,
            value=raw_amount - 100,  # total to burn = raw_amount
            balance_increase=100,
            index=INDEX,
        )

        result = processor.process_burn_event(
            event_data=event,
            previous_balance=1000000000000000000,
            previous_index=INDEX,
        )

        # Calculate expected based on rounding mode
        if strategy.burn_rounding == RoundingMode.HALF_UP:
            # 500000000000000000.5 rounds up to 500000000000000001
            expected_delta = -500000000000000001
        elif strategy.burn_rounding == RoundingMode.FLOOR:
            expected_delta = -500000000000000000
        else:  # CEIL
            expected_delta = -500000000000000001

        assert result.balance_delta == expected_delta


class TestGhoStrategies:
    """Test that GHO processor strategies match expected behavior."""

    @pytest.mark.parametrize("revision", [1, 2, 3, 4, 5, 6])
    def test_gho_revision_has_strategies(self, revision: int) -> None:
        """Every supported GHO revision has both rounding and discount strategies."""
        assert revision in GHO_STRATEGIES
        assert revision in GHO_DISCOUNT_STRATEGIES

    @pytest.mark.parametrize(
        "revision,expected_supports_discount,expected_has_method",
        [
            (1, True, False),
            (2, True, True),
            (3, True, True),
            (4, False, False),
            (5, False, False),
            (6, False, False),
        ],
    )
    def test_gho_discount_strategy(
        self,
        revision: int,
        expected_supports_discount: bool,
        expected_has_method: bool,
    ) -> None:
        """Verify GHO discount strategies match expected values."""
        strategy = GHO_DISCOUNT_STRATEGIES[revision]
        assert strategy.supports_discount == expected_supports_discount
        assert strategy.has_discounted_balance_method == expected_has_method

    @pytest.mark.parametrize(
        "revision,expected_mint,expected_burn",
        [
            (1, RoundingMode.HALF_UP, RoundingMode.HALF_UP),
            (2, RoundingMode.HALF_UP, RoundingMode.HALF_UP),
            (3, RoundingMode.HALF_UP, RoundingMode.HALF_UP),
            (4, RoundingMode.HALF_UP, RoundingMode.HALF_UP),
            (5, RoundingMode.CEIL, RoundingMode.FLOOR),
            (6, RoundingMode.CEIL, RoundingMode.FLOOR),
        ],
    )
    def test_gho_strategy_rounding(
        self,
        revision: int,
        expected_mint: RoundingMode,
        expected_burn: RoundingMode,
    ) -> None:
        """Verify GHO strategy rounding modes match expected values."""
        strategy = GHO_STRATEGIES[revision]
        assert strategy.mint_rounding == expected_mint
        assert strategy.burn_rounding == expected_burn

    @pytest.mark.parametrize("revision", [1, 2, 3, 4, 5])
    def test_gho_supports_discount_matches_strategy(self, revision: int) -> None:
        """GHO processor supports_discount() matches discount strategy."""
        processor = TokenProcessorFactory.get_gho_debt_processor(revision)
        strategy = GHO_DISCOUNT_STRATEGIES[revision]
        assert processor.supports_discount() == strategy.supports_discount

    @pytest.mark.parametrize("revision", [1, 2, 3])
    def test_gho_discount_revisions_refresh_after_balance_change(self, revision: int) -> None:
        """GHO revisions with discount support refresh after balance changes."""
        processor = TokenProcessorFactory.get_gho_debt_processor(revision)

        event = DebtMintEvent(
            caller="0x" + "00" * 20,
            on_behalf_of="0x" + "00" * 20,
            value=1000,
            balance_increase=0,
            index=INDEX,
        )

        result = processor.process_mint_event(
            event_data=event,
            previous_balance=0,
            previous_index=INDEX,
            previous_discount=1000,  # 10% discount
        )

        assert result.should_refresh_discount is True

    @pytest.mark.parametrize("revision", [4, 5])
    def test_gho_non_discount_revisions_no_refresh(self, revision: int) -> None:
        """GHO revisions without discount don't refresh."""
        processor = TokenProcessorFactory.get_gho_debt_processor(revision)

        event = DebtMintEvent(
            caller="0x" + "00" * 20,
            on_behalf_of="0x" + "00" * 20,
            value=1000,
            balance_increase=0,
            index=INDEX,
        )

        result = processor.process_mint_event(
            event_data=event,
            previous_balance=0,
            previous_index=INDEX,
            previous_discount=0,
        )

        assert result.should_refresh_discount is False
        assert result.discount_scaled == 0

    @pytest.mark.parametrize("revision", [1, 2, 3])
    def test_gho_discount_accrual(self, revision: int) -> None:
        """GHO revisions with discount accrue discount on action."""
        processor = TokenProcessorFactory.get_gho_debt_processor(revision)

        # Previous balance of 1e27, index goes from 1.0 to 2.0
        # Balance increase = 1e27 * (2.0 - 1.0) = 1e27
        # With 10% discount: discount = 1e26, discount_scaled = 1e26 / 2e27 = 5e25
        discount_scaled = processor.accrue_debt_on_action(
            previous_scaled_balance=RAY,
            previous_index=RAY,
            discount_percent=1000,  # 10%
            current_index=INDEX,  # 2.0
        )

        assert discount_scaled > 0  # Should have some discount

    @pytest.mark.parametrize("revision", [4, 5])
    def test_gho_no_discount_accrual(self, revision: int) -> None:
        """GHO revisions without discount return 0 for accrual."""
        processor = TokenProcessorFactory.get_gho_debt_processor(revision)

        discount_scaled = processor.accrue_debt_on_action(
            previous_scaled_balance=RAY,
            previous_index=RAY,
            discount_percent=1000,
            current_index=INDEX,
        )

        assert discount_scaled == 0


class TestProcessorBehaviorPreservation:
    """Test that processor behavior is preserved across the refactoring.

    These tests verify specific edge cases that could break during migration.
    """

    def test_collateral_v1_v3_identical_behavior(self) -> None:
        """V1 and V3 collateral processors produce identical results."""
        v1 = TokenProcessorFactory.get_collateral_processor(1)
        v3 = TokenProcessorFactory.get_collateral_processor(3)

        event = CollateralMintEvent(
            value=1000000000000000001,
            balance_increase=0,
            index=INDEX,
        )

        result_v1 = v1.process_mint_event(
            event_data=event,
            previous_balance=0,
            previous_index=INDEX,
        )
        result_v3 = v3.process_mint_event(
            event_data=event,
            previous_balance=0,
            previous_index=INDEX,
        )

        assert result_v1.balance_delta == result_v3.balance_delta
        assert result_v1.is_repay == result_v3.is_repay

    def test_collateral_v4_v5_identical_behavior(self) -> None:
        """V4 and V5 collateral processors produce identical results."""
        v4 = TokenProcessorFactory.get_collateral_processor(4)
        v5 = TokenProcessorFactory.get_collateral_processor(5)

        # Test burn (where V4/V5 use CEIL)
        event = CollateralBurnEvent(
            value=1000000000000000001,
            balance_increase=0,
            index=INDEX,
        )

        result_v4 = v4.process_burn_event(
            event_data=event,
            previous_balance=1000000000000000000,
            previous_index=INDEX,
        )
        result_v5 = v5.process_burn_event(
            event_data=event,
            previous_balance=1000000000000000000,
            previous_index=INDEX,
        )

        assert result_v4.balance_delta == result_v5.balance_delta

    def test_debt_v1_v3_identical_behavior(self) -> None:
        """V1 and V3 debt processors produce identical results."""
        v1 = TokenProcessorFactory.get_debt_processor(1)
        v3 = TokenProcessorFactory.get_debt_processor(3)

        event = DebtMintEvent(
            caller="0x" + "00" * 20,
            on_behalf_of="0x" + "00" * 20,
            value=1000000000000000001,
            balance_increase=0,
            index=INDEX,
        )

        result_v1 = v1.process_mint_event(
            event_data=event,
            previous_balance=0,
            previous_index=INDEX,
        )
        result_v3 = v3.process_mint_event(
            event_data=event,
            previous_balance=0,
            previous_index=INDEX,
        )

        assert result_v1.balance_delta == result_v3.balance_delta
        assert result_v1.is_repay == result_v3.is_repay

    def test_debt_v4_v5_identical_behavior(self) -> None:
        """V4 and V5 debt processors produce identical results."""
        v4 = TokenProcessorFactory.get_debt_processor(4)
        v5 = TokenProcessorFactory.get_debt_processor(5)

        # Test burn (where V4/V5 use FLOOR)
        event = DebtBurnEvent(
            from_="0x" + "00" * 20,
            target="0x" + "00" * 20,
            value=1000000000000000001,
            balance_increase=0,
            index=INDEX,
        )

        result_v4 = v4.process_burn_event(
            event_data=event,
            previous_balance=1000000000000000000,
            previous_index=INDEX,
        )
        result_v5 = v5.process_burn_event(
            event_data=event,
            previous_balance=1000000000000000000,
            previous_index=INDEX,
        )

        assert result_v4.balance_delta == result_v5.balance_delta

    def test_gho_v2_v3_identical_behavior(self) -> None:
        """V2 and V3 GHO processors produce identical results."""
        v2 = TokenProcessorFactory.get_gho_debt_processor(2)
        v3 = TokenProcessorFactory.get_gho_debt_processor(3)

        event = DebtMintEvent(
            caller="0x" + "00" * 20,
            on_behalf_of="0x" + "00" * 20,
            value=1000000000000000001,
            balance_increase=0,
            index=INDEX,
        )

        result_v2 = v2.process_mint_event(
            event_data=event,
            previous_balance=0,
            previous_index=INDEX,
            previous_discount=1000,
        )
        result_v3 = v3.process_mint_event(
            event_data=event,
            previous_balance=0,
            previous_index=INDEX,
            previous_discount=1000,
        )

        assert result_v2.balance_delta == result_v3.balance_delta
        assert result_v2.discount_scaled == result_v3.discount_scaled
        assert result_v2.should_refresh_discount == result_v3.should_refresh_discount

    def test_gho_v5_v6_identical_behavior(self) -> None:
        """V5 and V6 GHO processors produce identical results."""
        v5 = TokenProcessorFactory.get_gho_debt_processor(5)
        v6 = TokenProcessorFactory.get_gho_debt_processor(6)

        event = DebtMintEvent(
            caller="0x" + "00" * 20,
            on_behalf_of="0x" + "00" * 20,
            value=1000000000000000001,
            balance_increase=0,
            index=INDEX,
        )

        result_v5 = v5.process_mint_event(
            event_data=event,
            previous_balance=0,
            previous_index=INDEX,
            previous_discount=0,
        )
        result_v6 = v6.process_mint_event(
            event_data=event,
            previous_balance=0,
            previous_index=INDEX,
            previous_discount=0,
        )

        assert result_v5.balance_delta == result_v6.balance_delta
        assert result_v5.discount_scaled == result_v6.discount_scaled
        assert result_v5.should_refresh_discount == result_v6.should_refresh_discount


class TestFactoryInterface:
    """Test that factory interface is preserved."""

    def test_get_collateral_processor_has_required_methods(self) -> None:
        """Factory returns processor with collateral methods."""
        processor = TokenProcessorFactory.get_collateral_processor(1)
        assert hasattr(processor, "process_mint_event")
        assert hasattr(processor, "process_burn_event")
        assert hasattr(processor, "get_math_libraries")
        assert hasattr(processor, "revision")

    def test_get_debt_processor_has_required_methods(self) -> None:
        """Factory returns processor with debt methods."""
        processor = TokenProcessorFactory.get_debt_processor(1)
        assert hasattr(processor, "process_mint_event")
        assert hasattr(processor, "process_burn_event")
        assert hasattr(processor, "get_math_libraries")
        assert hasattr(processor, "revision")

    def test_get_gho_debt_processor_has_required_methods(self) -> None:
        """Factory returns processor with GHO debt methods."""
        processor = TokenProcessorFactory.get_gho_debt_processor(1)
        assert hasattr(processor, "process_mint_event")
        assert hasattr(processor, "process_burn_event")
        assert hasattr(processor, "supports_discount")
        assert hasattr(processor, "accrue_debt_on_action")
        assert hasattr(processor, "get_discounted_balance")
        assert hasattr(processor, "get_math_libraries")
        assert hasattr(processor, "revision")

    def test_get_collateral_processor_raises_for_invalid_revision(self) -> None:
        """Factory raises ValueError for unsupported revision."""
        with pytest.raises(ValueError, match="No processor for collateral revision"):
            TokenProcessorFactory.get_collateral_processor(99)

    def test_get_debt_processor_raises_for_invalid_revision(self) -> None:
        """Factory raises ValueError for unsupported revision."""
        with pytest.raises(ValueError, match="No processor for debt revision"):
            TokenProcessorFactory.get_debt_processor(99)

    def test_get_gho_debt_processor_raises_for_invalid_revision(self) -> None:
        """Factory raises ValueError for unsupported revision."""
        with pytest.raises(ValueError, match="No processor for GHO revision"):
            TokenProcessorFactory.get_gho_debt_processor(99)
