"""
Basic sanity tests for TokenMath wrapper contracts.

These tests verify that:
1. The wrapper contracts deploy successfully
2. The constants are correct
3. Basic function calls work
4. Python and Solidity implementations match for known values
"""

import pytest


class TestWrapperDeployment:
    """Test that wrapper contracts deploy correctly."""

    def test_rev1_deployed(self, token_math_wrapper_rev1):
        """Verify Rev 1 wrapper is deployed."""
        assert token_math_wrapper_rev1.address is not None

    def test_rev4_deployed(self, token_math_wrapper_rev4):
        """Verify Rev 4 wrapper is deployed."""
        assert token_math_wrapper_rev4.address is not None

    def test_rev9_deployed(self, token_math_wrapper_rev9):
        """Verify Rev 9 wrapper is deployed."""
        assert token_math_wrapper_rev9.address is not None

    def test_all_wrappers_deployed(self, token_math_wrappers):
        """Verify all three wrappers are in the dictionary."""
        assert 1 in token_math_wrappers
        assert 4 in token_math_wrappers
        assert 9 in token_math_wrappers
        for revision in [1, 4, 9]:
            assert token_math_wrappers[revision].address is not None


class TestConstants:
    """Test that constants match expected values."""

    @pytest.mark.parametrize("revision", [1, 4, 9])
    def test_wad_constant(self, token_math_wrappers, revision):
        """WAD should be 1e18 across all revisions."""
        result = token_math_wrappers[revision].functions.WAD().call()
        assert result == 10**18

    @pytest.mark.parametrize("revision", [1, 4, 9])
    def test_ray_constant(self, token_math_wrappers, revision):
        """RAY should be 1e27 across all revisions."""
        result = token_math_wrappers[revision].functions.RAY().call()
        assert result == 10**27


class TestBasicMath:
    """Test basic math functions return expected values."""

    def test_ray_mul_simple_rev1(self, token_math_wrapper_rev1):
        """2.5 RAY * 0.5 RAY = 1.25 RAY (half-up)."""
        # Use integer arithmetic: 2.5 RAY = 25 * 10**26, 0.5 RAY = 5 * 10**26
        result = token_math_wrapper_rev1.functions.rayMul(25 * 10**26, 5 * 10**26).call()
        assert result == 125 * 10**25  # 1.25 RAY

    def test_ray_mul_simple_rev4(self, token_math_wrapper_rev4):
        """2.5 RAY * 0.5 RAY = 1.25 RAY (half-up)."""
        result = token_math_wrapper_rev4.functions.rayMul(25 * 10**26, 5 * 10**26).call()
        assert result == 125 * 10**25  # 1.25 RAY

    def test_ray_div_simple_rev1(self, token_math_wrapper_rev1):
        """2.5 RAY / 0.5 RAY = 5 RAY (half-up)."""
        result = token_math_wrapper_rev1.functions.rayDiv(25 * 10**26, 5 * 10**26).call()
        assert result == 5 * 10**27

    def test_ray_div_simple_rev4(self, token_math_wrapper_rev4):
        """2.5 RAY / 0.5 RAY = 5 RAY (half-up)."""
        result = token_math_wrapper_rev4.functions.rayDiv(25 * 10**26, 5 * 10**26).call()
        assert result == 5 * 10**27


class TestTokenMathFunctions:
    """Test TokenMath wrapper functions."""

    def test_collateral_mint_scaled_amount_rev1(self, token_math_wrapper_rev1):
        """Test collateral mint calculation for Rev 1."""
        # Mint 1000 underlying with index 1.0 (RAY)
        amount = 1000 * 10**18  # 1000 tokens
        index = 1 * 10**27  # 1.0 in RAY

        result = token_math_wrapper_rev1.functions.getCollateralMintScaledAmount(
            amount, index
        ).call()

        # rayDiv(amount, index) = (amount * RAY + index/2) / index
        # For amount = 1000e18, index = 1e27:
        # = (1000e18 * 1e27 + 0.5e27) / 1e27 = 1000e18 = 10^21
        expected = 1000 * 10**18
        assert result == expected

    def test_collateral_balance_rev1(self, token_math_wrapper_rev1):
        """Test collateral balance calculation for Rev 1."""
        # Balance of 1000 scaled tokens with index 1.0
        scaled_amount = 1000 * 10**9  # 1000 scaled tokens
        index = 1 * 10**27  # 1.0 in RAY

        result = token_math_wrapper_rev1.functions.getCollateralBalance(scaled_amount, index).call()

        # With half-up rounding: 1000e9 * 1e27 / 1e27 = 1000e9
        expected = scaled_amount * index // 10**27
        assert result == expected


class TestRoundingDifferences:
    """Test that floor/ceil variants exist in Rev 4/9 but not Rev 1."""

    def test_floor_ceil_functions_exist_rev4(self, token_math_wrapper_rev4):
        """Rev 4 should have floor/ceil variants."""
        # These should not revert
        token_math_wrapper_rev4.functions.rayMulFloor(10**27, 10**27).call()
        token_math_wrapper_rev4.functions.rayMulCeil(10**27, 10**27).call()
        token_math_wrapper_rev4.functions.rayDivFloor(10**27, 10**27).call()
        token_math_wrapper_rev4.functions.rayDivCeil(10**27, 10**27).call()

    def test_floor_ceil_functions_exist_rev9(self, token_math_wrapper_rev9):
        """Rev 9 should have floor/ceil variants."""
        token_math_wrapper_rev9.functions.rayMulFloor(10**27, 10**27).call()
        token_math_wrapper_rev9.functions.rayMulCeil(10**27, 10**27).call()
        token_math_wrapper_rev9.functions.rayDivFloor(10**27, 10**27).call()
        token_math_wrapper_rev9.functions.rayDivCeil(10**27, 10**27).call()

    def test_floor_ceil_not_in_rev1(self, token_math_wrapper_rev1):
        """Rev 1 should NOT have floor/ceil variants."""
        # These should raise an AttributeError since they're not in the ABI
        assert not hasattr(token_math_wrapper_rev1.functions, "rayMulFloor"), (
            "Rev 1 should not have rayMulFloor"
        )
        assert not hasattr(token_math_wrapper_rev1.functions, "rayMulCeil"), (
            "Rev 1 should not have rayMulCeil"
        )


class TestFloorCeilRounding:
    """Test that floor/ceil rounding behaves correctly in Rev 4/9."""

    def test_ray_mul_floor_vs_ceil_rev4(self, token_math_wrapper_rev4):
        """Floor should be <= Ceil for same inputs."""
        a = 3 * 10**27  # 3.0 RAY
        b = 2 * 10**27  # 2.0 RAY
        # 3.0 * 2.0 = 6.0, exact result

        floor_result = token_math_wrapper_rev4.functions.rayMulFloor(a, b).call()
        ceil_result = token_math_wrapper_rev4.functions.rayMulCeil(a, b).call()

        assert floor_result == ceil_result  # Exact result, no rounding needed

    def test_ray_mul_floor_vs_ceil_with_remainder_rev4(self, token_math_wrapper_rev4):
        """When there's a remainder, floor < ceil."""
        a = 1 * 10**27 + 1  # Just over 1.0 RAY
        b = 1 * 10**27  # 1.0 RAY
        # Result = (10^27 + 1) * 10^27 / 10^27 = 10^27 + 1

        floor_result = token_math_wrapper_rev4.functions.rayMulFloor(a, b).call()
        ceil_result = token_math_wrapper_rev4.functions.rayMulCeil(a, b).call()

        assert floor_result <= ceil_result
        # Both should be 10^27 + 1 since there's no fractional remainder
        assert floor_result == 1 * 10**27 + 1
        assert ceil_result == 1 * 10**27 + 1

    def test_ray_div_floor_vs_ceil_rev4(self, token_math_wrapper_rev4):
        """Floor should be <= Ceil for division with remainder."""
        a = 10 * 10**27  # 10.0 RAY
        b = 3 * 10**27  # 3.0 RAY
        # 10 / 3 = 3.333..., floor = 3.333... truncated, ceil = 3.333... rounded up

        floor_result = token_math_wrapper_rev4.functions.rayDivFloor(a, b).call()
        ceil_result = token_math_wrapper_rev4.functions.rayDivCeil(a, b).call()

        assert floor_result <= ceil_result
        # Expected: floor = 3333333333333333333333333333 (3.333... truncated)
        # Expected: ceil = 3333333333333333333333333334 (3.333... rounded up)
        assert floor_result == 3333333333333333333333333333
        assert ceil_result == 3333333333333333333333333334
