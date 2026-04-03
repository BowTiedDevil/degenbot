"""Tests for Aave V3 position analysis module."""

import pytest

from degenbot.aave.position_analysis import (
    BASIS_POINTS,
    HEALTH_FACTOR_AT_RISK_THRESHOLD,
    HEALTH_FACTOR_LIQUIDATABLE_THRESHOLD,
    CollateralPositionData,
    DebtPositionData,
    PositionAnalysisResult,
    UserPositionSummary,
    calculate_health_factor,
)
from degenbot.constants import ZERO_ADDRESS


class TestCollateralPositionData:
    """Tests for CollateralPositionData."""

    def test_effective_liquidation_threshold_when_enabled(self) -> None:
        """Returns liquidation threshold when collateral is enabled."""
        pos = CollateralPositionData(
            asset_address=ZERO_ADDRESS,
            asset_symbol="TEST",
            scaled_balance=1000,
            actual_balance=1000,
            liquidation_threshold=8000,  # 80%
            ltv=7500,
            is_enabled_as_collateral=True,
            in_emode=False,
        )
        assert pos.effective_liquidation_threshold == 8000

    def test_effective_liquidation_threshold_when_disabled(self) -> None:
        """Returns 0 when collateral is disabled by user."""
        pos = CollateralPositionData(
            asset_address=ZERO_ADDRESS,
            asset_symbol="TEST",
            scaled_balance=1000,
            actual_balance=1000,
            liquidation_threshold=8000,
            ltv=7500,
            is_enabled_as_collateral=False,
            in_emode=False,
        )
        assert pos.effective_liquidation_threshold == 0


class TestUserPositionSummary:
    """Tests for UserPositionSummary."""

    def test_is_at_risk_when_below_threshold(self) -> None:
        """Position is at risk when health factor below threshold."""
        summary = UserPositionSummary(
            user_address=ZERO_ADDRESS,
            market_id=1,
            emode_category_id=None,
            is_isolation_mode=False,
            collateral_positions=(),
            debt_positions=(),
            health_factor=1.05,  # Below default threshold of 1.1
        )
        assert summary.is_at_risk is True

    def test_is_at_risk_when_above_threshold(self) -> None:
        """Position is safe when health factor above threshold."""
        summary = UserPositionSummary(
            user_address=ZERO_ADDRESS,
            market_id=1,
            emode_category_id=None,
            is_isolation_mode=False,
            collateral_positions=(),
            debt_positions=(),
            health_factor=1.5,
        )
        assert summary.is_at_risk is False

    def test_is_liquidatable_when_below_one(self) -> None:
        """Position is liquidatable when health factor below 1."""
        summary = UserPositionSummary(
            user_address=ZERO_ADDRESS,
            market_id=1,
            emode_category_id=None,
            is_isolation_mode=False,
            collateral_positions=(),
            debt_positions=(),
            health_factor=0.95,
        )
        assert summary.is_liquidatable is True

    def test_is_liquidatable_when_above_one(self) -> None:
        """Position is not liquidatable when health factor above 1."""
        summary = UserPositionSummary(
            user_address=ZERO_ADDRESS,
            market_id=1,
            emode_category_id=None,
            is_isolation_mode=False,
            collateral_positions=(),
            debt_positions=(),
            health_factor=1.05,
        )
        assert summary.is_liquidatable is False

    def test_health_factor_none_means_safe(self) -> None:
        """Position with no debt (health_factor=None) is not at risk."""
        summary = UserPositionSummary(
            user_address=ZERO_ADDRESS,
            market_id=1,
            emode_category_id=None,
            is_isolation_mode=False,
            collateral_positions=(),
            debt_positions=(),
            health_factor=None,
        )
        assert summary.is_at_risk is False
        assert summary.is_liquidatable is False


class TestCalculateHealthFactor:
    """Tests for calculate_health_factor function."""

    def test_no_debt_returns_none(self) -> None:
        """Returns None when there's no debt."""
        collateral = CollateralPositionData(
            asset_address=ZERO_ADDRESS,
            asset_symbol="TEST",
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

    def test_health_factor_calculation(self) -> None:
        """
        Health factor = (collateral * LT) / (debt * BASIS_POINTS).

        Example:
        - Collateral: 1000, LT: 80% -> weighted = 1000 * 8000 = 8,000,000
        - Debt: 500
        - HF = 8,000,000 / (500 * 10,000) = 1.6
        """
        collateral = CollateralPositionData(
            asset_address=ZERO_ADDRESS,
            asset_symbol="TEST",
            scaled_balance=1000,
            actual_balance=1000,
            liquidation_threshold=8000,  # 80%
            ltv=7500,
            is_enabled_as_collateral=True,
            in_emode=False,
        )
        debt = DebtPositionData(
            asset_address=ZERO_ADDRESS,
            asset_symbol="DEBT",
            scaled_balance=500,
            actual_balance=500,
        )
        result = calculate_health_factor(
            collateral_positions=(collateral,),
            debt_positions=(debt,),
        )
        assert result is not None
        assert result == pytest.approx(1.6, rel=1e-6)

    def test_disabled_collateral_not_counted(self) -> None:
        """Disabled collateral doesn't contribute to health factor."""
        collateral = CollateralPositionData(
            asset_address=ZERO_ADDRESS,
            asset_symbol="TEST",
            scaled_balance=1000,
            actual_balance=1000,
            liquidation_threshold=8000,
            ltv=7500,
            is_enabled_as_collateral=False,  # Disabled
            in_emode=False,
        )
        debt = DebtPositionData(
            asset_address=ZERO_ADDRESS,
            asset_symbol="DEBT",
            scaled_balance=500,
            actual_balance=500,
        )
        result = calculate_health_factor(
            collateral_positions=(collateral,),
            debt_positions=(debt,),
        )
        # HF = 0 / (500 * 10000) = 0
        assert result == pytest.approx(0.0, rel=1e-6)

    def test_liquidatable_health_factor(self) -> None:
        """Position with HF < 1 is liquidatable."""
        collateral = CollateralPositionData(
            asset_address=ZERO_ADDRESS,
            asset_symbol="TEST",
            scaled_balance=100,
            actual_balance=100,
            liquidation_threshold=8000,  # 80%
            ltv=7500,
            is_enabled_as_collateral=True,
            in_emode=False,
        )
        debt = DebtPositionData(
            asset_address=ZERO_ADDRESS,
            asset_symbol="DEBT",
            scaled_balance=100,
            actual_balance=100,
        )
        result = calculate_health_factor(
            collateral_positions=(collateral,),
            debt_positions=(debt,),
        )
        # HF = (100 * 8000) / (100 * 10000) = 0.8
        assert result == pytest.approx(0.8, rel=1e-6)


class TestPositionAnalysisResult:
    """Tests for PositionAnalysisResult."""

    def test_total_users(self) -> None:
        """Total users is sum of all categories."""
        result = PositionAnalysisResult()
        result.safe_users = [object()]  # type: ignore[list-item]
        result.at_risk_users = [object(), object()]  # type: ignore[list-item]
        result.liquidatable_users = [object()]  # type: ignore[list-item]
        assert result.total_users == 4

    def test_empty_result(self) -> None:
        """Empty result has zero counts."""
        result = PositionAnalysisResult()
        assert result.total_users == 0
        assert result.at_risk_count == 0
        assert result.liquidatable_count == 0


class TestConstants:
    """Tests for module constants."""

    def test_basis_points(self) -> None:
        """Basis points is 10000 (100%)."""
        assert BASIS_POINTS == 10000

    def test_health_factor_thresholds(self) -> None:
        """Health factor thresholds are properly defined."""
        assert pytest.approx(1.1) == HEALTH_FACTOR_AT_RISK_THRESHOLD
        assert pytest.approx(1.0) == HEALTH_FACTOR_LIQUIDATABLE_THRESHOLD
        assert HEALTH_FACTOR_AT_RISK_THRESHOLD > HEALTH_FACTOR_LIQUIDATABLE_THRESHOLD
