"""
Property-based tests for TokenMath Python/Solidity parity.

Uses Hypothesis to generate random inputs and verify Python implementations
exactly match Solidity contract behavior across all revisions.
"""

from typing import TYPE_CHECKING

import hypothesis
import hypothesis.strategies as st
import pytest
from web3.exceptions import ContractLogicError

from degenbot.aave.libraries import wad_ray_math
from degenbot.aave.libraries.token_math import (
    ExplicitRoundingMath,
    HalfUpRoundingMath,
)
from degenbot.constants import MAX_UINT256, MIN_UINT256
from degenbot.exceptions.evm import EVMRevertError

if TYPE_CHECKING:
    from web3.contract import Contract

# Strategies for generating test inputs
uint256_strategy = st.integers(min_value=MIN_UINT256, max_value=MAX_UINT256)
non_zero_uint256_strategy = st.integers(min_value=1, max_value=MAX_UINT256)


class TestRawMathParity:
    """
    Test raw ray/wad math functions match Solidity exactly.
    """

    @pytest.mark.parametrize("revision", [1, 4, 9])
    @hypothesis.settings(deadline=None)
    @hypothesis.given(a=uint256_strategy, b=uint256_strategy)
    def test_ray_mul_parity(
        self, request: pytest.FixtureRequest, a: int, b: int, revision: int
    ) -> None:
        """
        Python ray_mul matches Solidity rayMul for all revisions.
        """

        wrapper: Contract = request.getfixturevalue(f"token_math_wrapper_rev{revision}")

        try:
            solidity_result = wrapper.functions.rayMul(a, b).call()
        except ContractLogicError:
            solidity_error = True
        else:
            solidity_error = False

        try:
            python_result = wad_ray_math.ray_mul(a, b)
            python_error = False
        except EVMRevertError:
            python_error = True

        if solidity_error and python_error:
            return  # Both error - parity verified
        if solidity_error != python_error:
            pytest.fail(
                f"Error mismatch: Solidity errored={solidity_error}, Python errored={python_error}"
            )

        assert python_result == solidity_result, (
            f"ray_mul({a}, {b}) mismatch: Python={python_result}, Solidity={solidity_result}"
        )

    @pytest.mark.parametrize("revision", [1, 4, 9])
    @hypothesis.settings(deadline=None)
    @hypothesis.given(a=uint256_strategy, b=non_zero_uint256_strategy)
    def test_ray_div_parity(
        self,
        request: pytest.FixtureRequest,
        a: int,
        b: int,
        revision: int,
    ) -> None:
        """
        Python ray_div matches Solidity rayDiv for all revisions.
        """

        wrapper = request.getfixturevalue(f"token_math_wrapper_rev{revision}")

        try:
            solidity_result = wrapper.functions.rayDiv(a, b).call()
        except ContractLogicError:
            solidity_error = True
        else:
            solidity_error = False

        try:
            python_result = wad_ray_math.ray_div(a, b)
            python_error = False
        except EVMRevertError:
            python_error = True

        if solidity_error and python_error:
            return
        if solidity_error != python_error:
            pytest.fail(
                f"Error mismatch: Solidity errored={solidity_error}, Python errored={python_error}"
            )

        assert python_result == solidity_result, (
            f"ray_div({a}, {b}) mismatch: Python={python_result}, Solidity={solidity_result}"
        )

    @pytest.mark.parametrize("revision", [4, 9])
    @hypothesis.settings(deadline=None)
    @hypothesis.given(a=uint256_strategy, b=uint256_strategy)
    def test_ray_mul_floor_parity(
        self, request: pytest.FixtureRequest, a: int, b: int, revision: int
    ) -> None:
        """
        Python ray_mul_floor matches Solidity rayMulFloor (Rev 4/9 only).
        """

        wrapper = request.getfixturevalue(f"token_math_wrapper_rev{revision}")

        try:
            solidity_result = wrapper.functions.rayMulFloor(a, b).call()
        except ContractLogicError:
            solidity_error = True
        else:
            solidity_error = False

        try:
            python_result = wad_ray_math.ray_mul_floor(a, b)
            python_error = False
        except EVMRevertError:
            python_error = True

        if solidity_error and python_error:
            return
        if solidity_error != python_error:
            pytest.fail(
                f"Error mismatch: Solidity errored={solidity_error}, Python errored={python_error}"
            )

        assert python_result == solidity_result, (
            f"ray_mul_floor({a}, {b}) mismatch: Python={python_result}, Solidity={solidity_result}"
        )

    @pytest.mark.parametrize("revision", [4, 9])
    @hypothesis.settings(deadline=None)
    @hypothesis.given(a=uint256_strategy, b=uint256_strategy)
    def test_ray_mul_ceil_parity(
        self, request: pytest.FixtureRequest, a: int, b: int, revision: int
    ) -> None:
        """
        Python ray_mul_ceil matches Solidity rayMulCeil (Rev 4/9 only).
        """

        wrapper = request.getfixturevalue(f"token_math_wrapper_rev{revision}")

        try:
            solidity_result = wrapper.functions.rayMulCeil(a, b).call()
        except ContractLogicError:
            solidity_error = True
        else:
            solidity_error = False

        try:
            python_result = wad_ray_math.ray_mul_ceil(a, b)
            python_error = False
        except EVMRevertError:
            python_error = True

        if solidity_error and python_error:
            return
        if solidity_error != python_error:
            pytest.fail(
                f"Error mismatch: Solidity errored={solidity_error}, Python errored={python_error}"
            )

        assert python_result == solidity_result, (
            f"ray_mul_ceil({a}, {b}) mismatch: Python={python_result}, Solidity={solidity_result}"
        )

    @pytest.mark.parametrize("revision", [4, 9])
    @hypothesis.settings(deadline=None)
    @hypothesis.given(a=uint256_strategy, b=non_zero_uint256_strategy)
    def test_ray_div_floor_parity(
        self, request: pytest.FixtureRequest, a: int, b: int, revision: int
    ) -> None:
        """
        Python ray_div_floor matches Solidity rayDivFloor (Rev 4/9 only).
        """

        wrapper = request.getfixturevalue(f"token_math_wrapper_rev{revision}")

        try:
            solidity_result = wrapper.functions.rayDivFloor(a, b).call()
        except ContractLogicError:
            solidity_error = True
        else:
            solidity_error = False

        try:
            python_result = wad_ray_math.ray_div_floor(a, b)
            python_error = False
        except EVMRevertError:
            python_error = True

        if solidity_error and python_error:
            return
        if solidity_error != python_error:
            pytest.fail(
                f"Error mismatch: Solidity errored={solidity_error}, Python errored={python_error}"
            )

        assert python_result == solidity_result, (
            f"ray_div_floor({a}, {b}) mismatch: Python={python_result}, Solidity={solidity_result}"
        )

    @pytest.mark.parametrize("revision", [4, 9])
    @hypothesis.settings(deadline=None)
    @hypothesis.given(a=uint256_strategy, b=non_zero_uint256_strategy)
    def test_ray_div_ceil_parity(
        self, request: pytest.FixtureRequest, a: int, b: int, revision: int
    ) -> None:
        """
        Python ray_div_ceil matches Solidity rayDivCeil (Rev 4/9 only).
        """

        wrapper = request.getfixturevalue(f"token_math_wrapper_rev{revision}")

        try:
            solidity_result = wrapper.functions.rayDivCeil(a, b).call()
        except ContractLogicError:
            solidity_error = True
        else:
            solidity_error = False

        try:
            python_result = wad_ray_math.ray_div_ceil(a, b)
            python_error = False
        except EVMRevertError:
            python_error = True

        if solidity_error and python_error:
            return
        if solidity_error != python_error:
            pytest.fail(
                f"Error mismatch: Solidity errored={solidity_error}, Python errored={python_error}"
            )

        assert python_result == solidity_result, (
            f"ray_div_ceil({a}, {b}) mismatch: Python={python_result}, Solidity={solidity_result}"
        )

    @pytest.mark.parametrize("revision", [1, 4, 9])
    @hypothesis.settings(deadline=None)
    @hypothesis.given(a=uint256_strategy, b=uint256_strategy)
    def test_wad_mul_parity(
        self, request: pytest.FixtureRequest, a: int, b: int, revision: int
    ) -> None:
        """
        Python wad_mul matches Solidity wadMul for all revisions.
        """

        wrapper = request.getfixturevalue(f"token_math_wrapper_rev{revision}")

        try:
            solidity_result = wrapper.functions.wadMul(a, b).call()
        except ContractLogicError:
            solidity_error = True
        else:
            solidity_error = False

        try:
            python_result = wad_ray_math.wad_mul(a, b)
            python_error = False
        except EVMRevertError:
            python_error = True

        if solidity_error and python_error:
            return
        if solidity_error != python_error:
            pytest.fail(
                f"Error mismatch: Solidity errored={solidity_error}, Python errored={python_error}"
            )

        assert python_result == solidity_result, (
            f"wad_mul({a}, {b}) mismatch: Python={python_result}, Solidity={solidity_result}"
        )

    @pytest.mark.parametrize("revision", [1, 4, 9])
    @hypothesis.settings(deadline=None)
    @hypothesis.given(a=uint256_strategy, b=non_zero_uint256_strategy)
    def test_wad_div_parity(
        self, request: pytest.FixtureRequest, a: int, b: int, revision: int
    ) -> None:
        """
        Python wad_div matches Solidity wadDiv for all revisions.
        """

        wrapper = request.getfixturevalue(f"token_math_wrapper_rev{revision}")

        try:
            solidity_result = wrapper.functions.wadDiv(a, b).call()
        except ContractLogicError:
            solidity_error = True
        else:
            solidity_error = False

        try:
            python_result = wad_ray_math.wad_div(a, b)
            python_error = False
        except EVMRevertError:
            python_error = True

        if solidity_error and python_error:
            return
        if solidity_error != python_error:
            pytest.fail(
                f"Error mismatch: Solidity errored={solidity_error}, Python errored={python_error}"
            )

        assert python_result == solidity_result, (
            f"wad_div({a}, {b}) mismatch: Python={python_result}, Solidity={solidity_result}"
        )


class TestTokenMathParity:
    """
    Test TokenMath methods match Solidity contracts.
    """

    @pytest.mark.parametrize("revision", [1, 4, 9])
    @hypothesis.settings(deadline=None)
    @hypothesis.given(amount=uint256_strategy, index=non_zero_uint256_strategy)
    def test_collateral_mint_scaled_amount_parity(
        self, request: pytest.FixtureRequest, amount: int, index: int, revision: int
    ) -> None:
        """
        Python get_collateral_mint_scaled_amount matches Solidity.
        """

        wrapper = request.getfixturevalue(f"token_math_wrapper_rev{revision}")

        try:
            solidity_result = wrapper.functions.getCollateralMintScaledAmount(amount, index).call()
        except ContractLogicError:
            solidity_error = True
        else:
            solidity_error = False

        token_math = HalfUpRoundingMath if revision == 1 else ExplicitRoundingMath
        try:
            python_result = token_math.get_collateral_mint_scaled_amount(amount, index)
            python_error = False
        except EVMRevertError:
            python_error = True

        if solidity_error and python_error:
            return
        if solidity_error != python_error:
            pytest.fail(
                f"Error mismatch: Solidity errored={solidity_error}, Python errored={python_error}"
            )

        assert python_result == solidity_result, (
            f"get_collateral_mint_scaled_amount({amount}, {index}) mismatch: "
            f"Python={python_result}, Solidity={solidity_result}"
        )

    @pytest.mark.parametrize("revision", [1, 4, 9])
    @hypothesis.settings(deadline=None)
    @hypothesis.given(amount=uint256_strategy, index=non_zero_uint256_strategy)
    def test_collateral_burn_scaled_amount_parity(
        self, request: pytest.FixtureRequest, amount: int, index: int, revision: int
    ) -> None:
        """
        Python get_collateral_burn_scaled_amount matches Solidity.
        """

        wrapper = request.getfixturevalue(f"token_math_wrapper_rev{revision}")

        try:
            solidity_result = wrapper.functions.getCollateralBurnScaledAmount(amount, index).call()
        except ContractLogicError:
            solidity_error = True
        else:
            solidity_error = False

        token_math = HalfUpRoundingMath if revision == 1 else ExplicitRoundingMath
        try:
            python_result = token_math.get_collateral_burn_scaled_amount(amount, index)
            python_error = False
        except EVMRevertError:
            python_error = True

        if solidity_error and python_error:
            return
        if solidity_error != python_error:
            pytest.fail(
                f"Error mismatch: Solidity errored={solidity_error}, Python errored={python_error}"
            )

        assert python_result == solidity_result, (
            f"get_collateral_burn_scaled_amount({amount}, {index}) mismatch: "
            f"Python={python_result}, Solidity={solidity_result}"
        )

    @pytest.mark.parametrize("revision", [1, 4, 9])
    @hypothesis.settings(deadline=None)
    @hypothesis.given(amount=uint256_strategy, index=non_zero_uint256_strategy)
    def test_collateral_transfer_scaled_amount_parity(
        self, request: pytest.FixtureRequest, amount: int, index: int, revision: int
    ) -> None:
        """
        Python get_collateral_transfer_scaled_amount matches Solidity.
        """

        wrapper = request.getfixturevalue(f"token_math_wrapper_rev{revision}")

        try:
            solidity_result = wrapper.functions.getCollateralTransferScaledAmount(
                amount, index
            ).call()
        except ContractLogicError:
            solidity_error = True
        else:
            solidity_error = False

        token_math = HalfUpRoundingMath if revision == 1 else ExplicitRoundingMath
        try:
            python_result = token_math.get_collateral_transfer_scaled_amount(amount, index)
            python_error = False
        except EVMRevertError:
            python_error = True

        if solidity_error and python_error:
            return
        if solidity_error != python_error:
            pytest.fail(
                f"Error mismatch: Solidity errored={solidity_error}, Python errored={python_error}"
            )

        assert python_result == solidity_result, (
            f"get_collateral_transfer_scaled_amount({amount}, {index}) mismatch: "
            f"Python={python_result}, Solidity={solidity_result}"
        )

    @pytest.mark.parametrize("revision", [1, 4, 9])
    @hypothesis.settings(deadline=None)
    @hypothesis.given(scaled_amount=uint256_strategy, index=uint256_strategy)
    def test_collateral_balance_parity(
        self, request: pytest.FixtureRequest, scaled_amount: int, index: int, revision: int
    ) -> None:
        """
        Python get_collateral_balance matches Solidity getCollateralBalance.
        """

        wrapper = request.getfixturevalue(f"token_math_wrapper_rev{revision}")

        try:
            solidity_result = wrapper.functions.getCollateralBalance(scaled_amount, index).call()
        except ContractLogicError:
            solidity_error = True
        else:
            solidity_error = False

        token_math = HalfUpRoundingMath if revision == 1 else ExplicitRoundingMath
        try:
            python_result = token_math.get_collateral_balance(scaled_amount, index)
            python_error = False
        except EVMRevertError:
            python_error = True

        if solidity_error and python_error:
            return
        if solidity_error != python_error:
            pytest.fail(
                f"Error mismatch: Solidity errored={solidity_error}, Python errored={python_error}"
            )

        assert python_result == solidity_result, (
            f"get_collateral_balance({scaled_amount}, {index}) mismatch: "
            f"Python={python_result}, Solidity={solidity_result}"
        )

    @pytest.mark.parametrize("revision", [1, 4, 9])
    @hypothesis.settings(deadline=None)
    @hypothesis.given(amount=uint256_strategy, index=non_zero_uint256_strategy)
    def test_debt_mint_scaled_amount_parity(
        self, request: pytest.FixtureRequest, amount: int, index: int, revision: int
    ) -> None:
        """
        Python get_debt_mint_scaled_amount matches Solidity getDebtMintScaledAmount.
        """

        wrapper = request.getfixturevalue(f"token_math_wrapper_rev{revision}")

        try:
            solidity_result = wrapper.functions.getDebtMintScaledAmount(amount, index).call()
        except ContractLogicError:
            solidity_error = True
        else:
            solidity_error = False

        token_math = HalfUpRoundingMath if revision == 1 else ExplicitRoundingMath
        try:
            python_result = token_math.get_debt_mint_scaled_amount(amount, index)
            python_error = False
        except EVMRevertError:
            python_error = True

        if solidity_error and python_error:
            return
        if solidity_error != python_error:
            pytest.fail(
                f"Error mismatch: Solidity errored={solidity_error}, Python errored={python_error}"
            )

        assert python_result == solidity_result, (
            f"get_debt_mint_scaled_amount({amount}, {index}) mismatch: "
            f"Python={python_result}, Solidity={solidity_result}"
        )

    @pytest.mark.parametrize("revision", [1, 4, 9])
    @hypothesis.settings(deadline=None)
    @hypothesis.given(amount=uint256_strategy, index=non_zero_uint256_strategy)
    def test_debt_burn_scaled_amount_parity(
        self, request: pytest.FixtureRequest, amount: int, index: int, revision: int
    ) -> None:
        """
        Python get_debt_burn_scaled_amount matches Solidity getDebtBurnScaledAmount.
        """

        wrapper = request.getfixturevalue(f"token_math_wrapper_rev{revision}")

        try:
            solidity_result = wrapper.functions.getDebtBurnScaledAmount(amount, index).call()
        except ContractLogicError:
            solidity_error = True
        else:
            solidity_error = False

        token_math = HalfUpRoundingMath if revision == 1 else ExplicitRoundingMath
        try:
            python_result = token_math.get_debt_burn_scaled_amount(amount, index)
            python_error = False
        except EVMRevertError:
            python_error = True

        if solidity_error and python_error:
            return
        if solidity_error != python_error:
            pytest.fail(
                f"Error mismatch: Solidity errored={solidity_error}, Python errored={python_error}"
            )

        assert python_result == solidity_result, (
            f"get_debt_burn_scaled_amount({amount}, {index}) mismatch: "
            f"Python={python_result}, Solidity={solidity_result}"
        )

    @pytest.mark.parametrize("revision", [1, 4, 9])
    @hypothesis.settings(deadline=None)
    @hypothesis.given(scaled_amount=uint256_strategy, index=uint256_strategy)
    def test_debt_balance_parity(
        self, request: pytest.FixtureRequest, scaled_amount: int, index: int, revision: int
    ) -> None:
        """
        Python get_debt_balance matches Solidity getDebtBalance.
        """

        wrapper = request.getfixturevalue(f"token_math_wrapper_rev{revision}")

        try:
            solidity_result = wrapper.functions.getDebtBalance(scaled_amount, index).call()
        except ContractLogicError:
            solidity_error = True
        else:
            solidity_error = False

        token_math = HalfUpRoundingMath if revision == 1 else ExplicitRoundingMath
        try:
            python_result = token_math.get_debt_balance(scaled_amount, index)
            python_error = False
        except EVMRevertError:
            python_error = True

        if solidity_error and python_error:
            return
        if solidity_error != python_error:
            pytest.fail(
                f"Error mismatch: Solidity errored={solidity_error}, Python errored={python_error}"
            )

        assert python_result == solidity_result, (
            f"get_debt_balance({scaled_amount}, {index}) mismatch: "
            f"Python={python_result}, Solidity={solidity_result}"
        )


class TestRoundingHierarchy:
    """Verify floor <= half-up <= ceil for Rev 4/9."""

    @pytest.mark.parametrize("revision", [4, 9])
    @hypothesis.settings(deadline=None)
    @hypothesis.given(a=uint256_strategy, b=non_zero_uint256_strategy)
    def test_ray_div_rounding_hierarchy(
        self, request: pytest.FixtureRequest, a: int, b: int, revision: int
    ) -> None:
        """
        floor <= half-up <= ceil for ray_div variants (when not exact division).
        """

        wrapper = request.getfixturevalue(f"token_math_wrapper_rev{revision}")

        try:
            floor_result = wrapper.functions.rayDivFloor(a, b).call()
            half_up_result = wrapper.functions.rayDiv(a, b).call()
            ceil_result = wrapper.functions.rayDivCeil(a, b).call()
        except ContractLogicError:
            hypothesis.assume(condition=False)  # Skip if any operation overflows

        assert floor_result <= half_up_result <= ceil_result, (
            f"ray_div rounding hierarchy violated: floor={floor_result}, "
            f"half-up={half_up_result}, ceil={ceil_result}"
        )

    @pytest.mark.parametrize("revision", [4, 9])
    @hypothesis.settings(deadline=None)
    @hypothesis.given(a=uint256_strategy, b=uint256_strategy)
    def test_ray_mul_rounding_hierarchy(
        self, request: pytest.FixtureRequest, a: int, b: int, revision: int
    ) -> None:
        """
        floor <= half-up <= ceil for ray_mul variants (when not exact multiplication).
        """

        wrapper = request.getfixturevalue(f"token_math_wrapper_rev{revision}")

        try:
            floor_result = wrapper.functions.rayMulFloor(a, b).call()
            half_up_result = wrapper.functions.rayMul(a, b).call()
            ceil_result = wrapper.functions.rayMulCeil(a, b).call()
        except ContractLogicError:
            hypothesis.assume(condition=False)  # Skip if any operation overflows

        assert floor_result <= half_up_result <= ceil_result, (
            f"ray_mul rounding hierarchy violated: floor={floor_result}, "
            f"half-up={half_up_result}, ceil={ceil_result}"
        )


class TestKnownValues:
    """Regression tests from issues 0034 and 0036."""

    @pytest.mark.parametrize("revision", [4, 9])
    def test_issue_0034_rev9_rounding(self, request: pytest.FixtureRequest, revision: int) -> None:
        """
        Issue 0034: Pool Rev 9 MINT_TO_TREASURY ceil rounding verification.

        From debug/aave/0034:
            - MintedToTreasury.amount: 76,116,689,027,312,564,277
            - Index: 1,051,094,981,887,882,471,312,148,250
            - Expected scaled: 72,416,565,904,061,875,431 (using ray_div_ceil)
        """

        wrapper = request.getfixturevalue(f"token_math_wrapper_rev{revision}")

        amount = 76116689027312564277
        index = 1051094981887882471312148250
        expected = 72416565904061875431

        result = wrapper.functions.rayDivCeil(amount, index).call()
        assert result == expected, f"Issue 0034 regression: expected {expected}, got {result}"

        # Also verify Python matches
        python_result = wad_ray_math.ray_div_ceil(amount, index)
        assert python_result == expected, (
            f"Issue 0034 Python mismatch: expected {expected}, got {python_result}"
        )

    def test_issue_0036_rev8_half_up_rounding(self, token_math_wrapper_rev1) -> None:
        """Issue 0036: Pool Rev 8 MINT_TO_TREASURY half-up rounding verification.

        From debug/aave/0036:
        - MintedToTreasury.amount: 312,922,037,040,136,887
        - Index: 1,001,340,845,020,106,656,953,816,530
        - Expected scaled: 312,503,018,923,445,089 (using ray_div, half-up)

        Note: Rev 1 wrapper uses half-up rounding like Rev 8.
        """
        amount = 312922037040136887
        index = 1001340845020106656953816530
        expected = 312503018923445089

        result = token_math_wrapper_rev1.functions.rayDiv(amount, index).call()
        assert result == expected, f"Issue 0036 regression: expected {expected}, got {result}"

        # Also verify Python matches
        python_result = wad_ray_math.ray_div(amount, index)
        assert python_result == expected, (
            f"Issue 0036 Python mismatch: expected {expected}, got {python_result}"
        )
