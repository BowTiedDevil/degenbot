"""Tests for the analysis core module (Plan 009).

Core tests use Fake data — no database, no RPC, no ORM objects.
All inputs are plain dataclasses or primitives.
"""

import pytest
from eth_typing import ChecksumAddress

from degenbot.aave.analysis.core import (
    CollateralPositionData,
    CollateralPositionRecord,
    DebtPositionData,
    DebtPositionRecord,
    UserPositionSummary,
    UserRecord,
    analyze_user_position,
    build_collateral_position_data,
    build_debt_position_data,
    calculate_health_factor,
)
from degenbot.constants import ZERO_ADDRESS


def _addr(n: int) -> ChecksumAddress:
    return ChecksumAddress(f"0x{'0' * 39}{n}")


class TestHealthFactorPure:
    """Pure health factor calculation tests with plain dataclasses."""

    def test_no_debt_returns_none(self) -> None:
        collateral = CollateralPositionData(
            asset_address=ZERO_ADDRESS,
            asset_symbol="WETH",
            scaled_balance=1000,
            actual_balance=1000,
            liquidation_threshold=8000,
            ltv=7500,
            is_enabled_as_collateral=True,
            in_emode=False,
        )
        result = calculate_health_factor(
            collateral_positions=(collateral,),
            debt_positions=(),
        )
        assert result is None

    def test_safe_position(self) -> None:
        """HF > 1: position is safe."""
        collateral = CollateralPositionData(
            asset_address=ZERO_ADDRESS,
            asset_symbol="WETH",
            scaled_balance=1000,
            actual_balance=1000,
            liquidation_threshold=8000,
            ltv=7500,
            is_enabled_as_collateral=True,
            in_emode=False,
        )
        debt = DebtPositionData(
            asset_address=_addr(1),
            asset_symbol="USDC",
            scaled_balance=500,
            actual_balance=500,
        )
        result = calculate_health_factor(
            collateral_positions=(collateral,),
            debt_positions=(debt,),
        )
        assert result is not None
        assert result > 1.0

    def test_liquidatable_position(self) -> None:
        """HF < 1: position is liquidatable."""
        collateral = CollateralPositionData(
            asset_address=ZERO_ADDRESS,
            asset_symbol="WETH",
            scaled_balance=100,
            actual_balance=100,
            liquidation_threshold=8000,
            ltv=7500,
            is_enabled_as_collateral=True,
            in_emode=False,
        )
        debt = DebtPositionData(
            asset_address=_addr(1),
            asset_symbol="USDC",
            scaled_balance=100,
            actual_balance=100,
        )
        result = calculate_health_factor(
            collateral_positions=(collateral,),
            debt_positions=(debt,),
        )
        assert result is not None
        assert result < 1.0

    def test_disabled_collateral_zero_weight(self) -> None:
        """Disabled collateral contributes nothing."""
        collateral = CollateralPositionData(
            asset_address=ZERO_ADDRESS,
            asset_symbol="WETH",
            scaled_balance=1000,
            actual_balance=1000,
            liquidation_threshold=8000,
            ltv=7500,
            is_enabled_as_collateral=False,
            in_emode=False,
        )
        debt = DebtPositionData(
            asset_address=_addr(1),
            asset_symbol="USDC",
            scaled_balance=500,
            actual_balance=500,
        )
        result = calculate_health_factor(
            collateral_positions=(collateral,),
            debt_positions=(debt,),
        )
        assert result is not None
        assert result == pytest.approx(0.0)

    def test_multiple_collateral_and_debt(self) -> None:
        """HF with multiple positions on each side."""
        collateral_a = CollateralPositionData(
            asset_address=_addr(1),
            asset_symbol="WETH",
            scaled_balance=1000,
            actual_balance=1000,
            liquidation_threshold=8000,
            ltv=7500,
            is_enabled_as_collateral=True,
            in_emode=False,
        )
        collateral_b = CollateralPositionData(
            asset_address=_addr(2),
            asset_symbol="WBTC",
            scaled_balance=2000,
            actual_balance=2000,
            liquidation_threshold=7000,
            ltv=6500,
            is_enabled_as_collateral=True,
            in_emode=False,
        )
        debt_a = DebtPositionData(
            asset_address=_addr(3),
            asset_symbol="USDC",
            scaled_balance=1000,
            actual_balance=1000,
        )
        debt_b = DebtPositionData(
            asset_address=_addr(4),
            asset_symbol="DAI",
            scaled_balance=500,
            actual_balance=500,
        )
        result = calculate_health_factor(
            collateral_positions=(collateral_a, collateral_b),
            debt_positions=(debt_a, debt_b),
        )
        # Weighted collateral: (1000 * 8000) + (2000 * 7000) = 8M + 14M = 22M
        # Total debt: 1000 + 500 = 1500
        # HF = 22,000,000 / (1500 * 10000) = 22M / 15M ≈ 1.467
        assert result is not None
        assert result == pytest.approx(22_000_000 / 15_000_000, rel=1e-6)

    def test_price_adjusted_collateral(self) -> None:
        """Price-adjusted balances are used when prices are provided."""
        collateral = CollateralPositionData(
            asset_address=ZERO_ADDRESS,
            asset_symbol="WETH",
            scaled_balance=1,
            actual_balance=1,
            liquidation_threshold=8000,
            ltv=7500,
            is_enabled_as_collateral=True,
            in_emode=False,
            price=2_000_000_000,  # $2000 in 8 decimals
        )
        debt = DebtPositionData(
            asset_address=_addr(1),
            asset_symbol="USDC",
            scaled_balance=1,
            actual_balance=1,
            price=1_000_000,  # $1 in 8 decimals (6-decimal USDC)
        )
        result = calculate_health_factor(
            collateral_positions=(collateral,),
            debt_positions=(debt,),
        )
        assert result is not None
        assert result == pytest.approx(1600.0, rel=1e-6)

    def test_isolation_mode_caps_debt(self) -> None:
        """Isolation mode caps effective debt at the ceiling."""
        collateral = CollateralPositionData(
            asset_address=ZERO_ADDRESS,
            asset_symbol="WETH",
            scaled_balance=10_000,
            actual_balance=10_000,
            liquidation_threshold=8000,
            ltv=7500,
            is_enabled_as_collateral=True,
            in_emode=False,
        )
        debt = DebtPositionData(
            asset_address=_addr(1),
            asset_symbol="USDC",
            scaled_balance=5_000,
            actual_balance=5_000,
        )
        # Without isolation: HF = (10000 * 8000) / (5000 * 10000) = 1.6
        result_no_isolation = calculate_health_factor(
            collateral_positions=(collateral,),
            debt_positions=(debt,),
        )
        # With isolation ceiling of 1000: effective debt = min(5000, 1000) = 1000
        result_isolation = calculate_health_factor(
            collateral_positions=(collateral,),
            debt_positions=(debt,),
            isolation_mode_debt=5_000,
            isolation_debt_ceiling=1_000,
        )
        assert result_no_isolation is not None
        assert result_isolation is not None
        assert result_isolation > result_no_isolation

    def test_zero_balance_collateral_ignored(self) -> None:
        """Collateral with zero balance doesn't contribute."""
        collateral = CollateralPositionData(
            asset_address=ZERO_ADDRESS,
            asset_symbol="WETH",
            scaled_balance=0,
            actual_balance=0,
            liquidation_threshold=8000,
            ltv=7500,
            is_enabled_as_collateral=True,
            in_emode=False,
        )
        debt = DebtPositionData(
            asset_address=_addr(1),
            asset_symbol="USDC",
            scaled_balance=100,
            actual_balance=100,
        )
        result = calculate_health_factor(
            collateral_positions=(collateral,),
            debt_positions=(debt,),
        )
        assert result is not None
        assert result == pytest.approx(0.0)


class TestCollateralPositionDataProperties:
    """CollateralPositionData derived properties."""

    def test_effective_lt_enabled(self) -> None:
        pos = CollateralPositionData(
            asset_address=ZERO_ADDRESS,
            asset_symbol="TEST",
            scaled_balance=0,
            actual_balance=0,
            liquidation_threshold=8000,
            ltv=7500,
            is_enabled_as_collateral=True,
            in_emode=False,
        )
        assert pos.effective_liquidation_threshold == 8000

    def test_effective_lt_disabled(self) -> None:
        pos = CollateralPositionData(
            asset_address=ZERO_ADDRESS,
            asset_symbol="TEST",
            scaled_balance=0,
            actual_balance=0,
            liquidation_threshold=8000,
            ltv=7500,
            is_enabled_as_collateral=False,
            in_emode=False,
        )
        assert pos.effective_liquidation_threshold == 0

    def test_price_adjusted_balance_with_price(self) -> None:
        pos = CollateralPositionData(
            asset_address=ZERO_ADDRESS,
            asset_symbol="TEST",
            scaled_balance=0,
            actual_balance=100,
            liquidation_threshold=8000,
            ltv=7500,
            is_enabled_as_collateral=True,
            in_emode=False,
            price=2_000_000_000,
        )
        assert pos.price_adjusted_balance == 100 * 2_000_000_000

    def test_price_adjusted_balance_without_price(self) -> None:
        pos = CollateralPositionData(
            asset_address=ZERO_ADDRESS,
            asset_symbol="TEST",
            scaled_balance=0,
            actual_balance=100,
            liquidation_threshold=8000,
            ltv=7500,
            is_enabled_as_collateral=True,
            in_emode=False,
        )
        assert pos.price_adjusted_balance == 100


class TestDebtPositionDataProperties:
    """DebtPositionData derived properties."""

    def test_price_adjusted_balance_with_price(self) -> None:
        pos = DebtPositionData(
            asset_address=ZERO_ADDRESS,
            asset_symbol="USDC",
            scaled_balance=0,
            actual_balance=500,
            price=1_000_000,
        )
        assert pos.price_adjusted_balance == 500 * 1_000_000

    def test_price_adjusted_balance_without_price(self) -> None:
        pos = DebtPositionData(
            asset_address=ZERO_ADDRESS,
            asset_symbol="USDC",
            scaled_balance=0,
            actual_balance=500,
        )
        assert pos.price_adjusted_balance == 500


class TestUserPositionSummaryProperties:
    """UserPositionSummary derived properties."""

    def _make_summary(self, health_factor: float | None) -> UserPositionSummary:
        return UserPositionSummary(
            user_address=ZERO_ADDRESS,
            market_id=1,
            emode_category_id=None,
            is_isolation_mode=False,
            collateral_positions=(),
            debt_positions=(),
            health_factor=health_factor,
        )

    def test_at_risk_below_threshold(self) -> None:
        assert self._make_summary(1.05).is_at_risk is True

    def test_at_risk_above_threshold(self) -> None:
        assert self._make_summary(1.5).is_at_risk is False

    def test_liquidatable_below_one(self) -> None:
        assert self._make_summary(0.95).is_liquidatable is True

    def test_not_liquidatable_above_one(self) -> None:
        assert self._make_summary(1.05).is_liquidatable is False

    def test_none_health_factor_is_safe(self) -> None:
        summary = self._make_summary(None)
        assert summary.is_at_risk is False
        assert summary.is_liquidatable is False
        assert summary.has_debt is False

    def test_has_debt(self) -> None:
        summary = UserPositionSummary(
            user_address=ZERO_ADDRESS,
            market_id=1,
            emode_category_id=None,
            is_isolation_mode=False,
            collateral_positions=(),
            debt_positions=(
                DebtPositionData(
                    asset_address=ZERO_ADDRESS,
                    asset_symbol="USDC",
                    scaled_balance=100,
                    actual_balance=100,
                ),
            ),
        )
        assert summary.has_debt is True


class TestBuildCollateralPositionData:
    """build_collateral_position_data from flat records."""

    def test_build_from_record(self) -> None:
        record = CollateralPositionRecord(
            asset_id=1,
            balance=1000,
            underlying_address=_addr(1),
            underlying_symbol="WETH",
            liquidity_index=10**27,
            e_mode_category_id=None,
            asset_lt=8000,
            asset_ltv=7500,
        )
        result = build_collateral_position_data(
            record=record,
            user_emode=0,
            collateral_enabled=True,
        )
        assert result.asset_address == _addr(1)
        assert result.asset_symbol == "WETH"
        assert result.scaled_balance == 1000
        assert result.is_enabled_as_collateral is True
        assert result.liquidation_threshold == 8000
        assert result.ltv == 7500

    def test_build_with_emode(self) -> None:
        record = CollateralPositionRecord(
            asset_id=1,
            balance=1000,
            underlying_address=_addr(1),
            underlying_symbol="WETH",
            liquidity_index=10**27,
            e_mode_category_id=1,
            asset_lt=8000,
            asset_ltv=7500,
            emode_lt=9500,
            emode_ltv=9000,
        )
        result = build_collateral_position_data(
            record=record,
            user_emode=1,  # Same category
            collateral_enabled=True,
        )
        assert result.liquidation_threshold == 9500  # eMode override
        assert result.ltv == 9000
        assert result.in_emode is True

    def test_build_with_different_emode(self) -> None:
        record = CollateralPositionRecord(
            asset_id=1,
            balance=1000,
            underlying_address=_addr(1),
            underlying_symbol="WETH",
            liquidity_index=10**27,
            e_mode_category_id=1,
            asset_lt=8000,
            asset_ltv=7500,
            emode_lt=9500,
            emode_ltv=9000,
        )
        result = build_collateral_position_data(
            record=record,
            user_emode=2,  # Different category
            collateral_enabled=True,
        )
        assert result.liquidation_threshold == 8000  # Standard, not eMode
        assert result.ltv == 7500
        assert result.in_emode is False

    def test_build_disabled_collateral(self) -> None:
        record = CollateralPositionRecord(
            asset_id=1,
            balance=1000,
            underlying_address=_addr(1),
            underlying_symbol="WETH",
            liquidity_index=10**27,
            e_mode_category_id=None,
            asset_lt=8000,
            asset_ltv=7500,
        )
        result = build_collateral_position_data(
            record=record,
            user_emode=0,
            collateral_enabled=False,
        )
        assert result.effective_liquidation_threshold == 0

    def test_build_with_price(self) -> None:
        record = CollateralPositionRecord(
            asset_id=1,
            balance=1,
            underlying_address=_addr(1),
            underlying_symbol="WETH",
            liquidity_index=10**27,
            e_mode_category_id=None,
            asset_lt=8000,
            asset_ltv=7500,
        )
        result = build_collateral_position_data(
            record=record,
            user_emode=0,
            collateral_enabled=True,
            price=2_000_000_000,
        )
        assert result.price == 2_000_000_000


class TestBuildDebtPositionData:
    """build_debt_position_data from flat records."""

    def test_build_from_record(self) -> None:
        record = DebtPositionRecord(
            asset_id=1,
            balance=500,
            underlying_address=_addr(1),
            underlying_symbol="USDC",
            borrow_index=10**27,
            e_mode_category_id=None,
        )
        result = build_debt_position_data(
            record=record,
            user_emode=0,
        )
        assert result.asset_address == _addr(1)
        assert result.asset_symbol == "USDC"
        assert result.scaled_balance == 500
        assert result.in_emode is False

    def test_build_with_emode(self) -> None:
        record = DebtPositionRecord(
            asset_id=1,
            balance=500,
            underlying_address=_addr(1),
            underlying_symbol="USDC",
            borrow_index=10**27,
            e_mode_category_id=1,
        )
        result = build_debt_position_data(
            record=record,
            user_emode=1,
        )
        assert result.in_emode is True


class TestAnalyzeUserPosition:
    """analyze_user_position with flat records."""

    def _make_user(self, **overrides: object) -> UserRecord:
        defaults: dict = {
            "id": 1,
            "address": _addr(99),
            "market_id": 1,
            "e_mode": 0,
            "is_isolation_mode": False,
            "isolation_mode_debt": 0,
        }
        defaults.update(overrides)
        return UserRecord(**defaults)

    def _make_collateral(self, **overrides: object) -> CollateralPositionRecord:
        defaults: dict = {
            "asset_id": 1,
            "balance": 1000,
            "underlying_address": _addr(1),
            "underlying_symbol": "WETH",
            "liquidity_index": 10**27,
            "e_mode_category_id": None,
            "asset_lt": 8000,
            "asset_ltv": 7500,
        }
        defaults.update(overrides)
        return CollateralPositionRecord(**defaults)

    def _make_debt(self, **overrides: object) -> DebtPositionRecord:
        defaults: dict = {
            "asset_id": 2,
            "balance": 500,
            "underlying_address": _addr(2),
            "underlying_symbol": "USDC",
            "borrow_index": 10**27,
            "e_mode_category_id": None,
        }
        defaults.update(overrides)
        return DebtPositionRecord(**defaults)

    def test_no_debt_no_health_factor(self) -> None:
        user = self._make_user()
        result = analyze_user_position(
            user=user,
            collateral_positions=[self._make_collateral()],
            debt_positions=[],
            collateral_config_map={},
        )
        assert result.health_factor is None

    def test_safe_position(self) -> None:
        user = self._make_user()
        result = analyze_user_position(
            user=user,
            collateral_positions=[self._make_collateral()],
            debt_positions=[self._make_debt()],
            collateral_config_map={1: True},
        )
        assert result.health_factor is not None
        assert result.health_factor > 1.0

    def test_disabled_collateral_zero_hf(self) -> None:
        user = self._make_user()
        result = analyze_user_position(
            user=user,
            collateral_positions=[self._make_collateral()],
            debt_positions=[self._make_debt()],
            collateral_config_map={1: False},  # Disabled
        )
        assert result.health_factor is not None
        assert result.health_factor == pytest.approx(0.0)

    def test_isolation_mode(self) -> None:
        user = self._make_user(
            is_isolation_mode=True,
            isolation_mode_debt=5000,
            isolation_debt_ceiling=1000,
        )
        result = analyze_user_position(
            user=user,
            collateral_positions=[self._make_collateral()],
            debt_positions=[self._make_debt()],
            collateral_config_map={},
        )
        assert result.is_isolation_mode is True
        assert result.health_factor is not None

    def test_user_metadata_preserved(self) -> None:
        user = self._make_user(address=_addr(42), market_id=7, e_mode=0)
        result = analyze_user_position(
            user=user,
            collateral_positions=[],
            debt_positions=[],
            collateral_config_map={},
        )
        assert result.user_address == _addr(42)
        assert result.market_id == 7
        assert result.emode_category_id is None
