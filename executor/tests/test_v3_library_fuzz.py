"""
Cross-implementation fuzz tests: Vyper port vs original Solidity.

Deploys both the Vyper test_harness and the Solidity SolReference on the
same Anvil instance, then uses Hypothesis to compare outputs across a wide
range of inputs. Any disagreement is a bug in the Vyper port.
"""

import json
from pathlib import Path

import pytest
from hypothesis import HealthCheck, given, settings
from hypothesis import strategies as st

# Committed solc 0.7.6 artifact (see scripts/build-sol-reference-artifact.sh):
# the original Solidity V3-math libraries, deployed next to the Vyper
# test_harness for cross-implementation fuzzing.
SOL_REF_ARTIFACT = (
    Path(__file__).resolve().parent.parent
    / "contracts"
    / "sol_reference"
    / "artifacts"
    / "SolReference.json"
)


# ── Strategies ──

uint256 = st.integers(min_value=0, max_value=2**256 - 1)
uint160 = st.integers(min_value=1, max_value=2**160 - 1)
uint128 = st.integers(min_value=1, max_value=2**128 - 1)
int256 = st.integers(min_value=-(2**255), max_value=2**255 - 1)
fee_pips = st.integers(min_value=0, max_value=999999)
valid_tick = st.integers(min_value=-887272, max_value=887272)
MIN_SQRT_RATIO = 4295128739
MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342
valid_sqrt_price = st.integers(min_value=MIN_SQRT_RATIO, max_value=MAX_SQRT_RATIO - 1)


# ── Fixtures ──


@pytest.fixture(scope="module")
def harness(project, accounts):
    """Deploy the Vyper test harness."""
    return project.test_harness.deploy(sender=accounts[0])


@pytest.fixture(scope="module")
def sol_ref(accounts):
    """Deploy the Solidity reference contract from pre-compiled artifact."""
    from ethpm_types import ContractType
    from ape.contracts.base import ContractContainer

    if not SOL_REF_ARTIFACT.is_file():
        pytest.fail(
            f"missing committed artifact {SOL_REF_ARTIFACT} — build and commit it "
            "with executor/scripts/build-sol-reference-artifact.sh"
        )
    with open(SOL_REF_ARTIFACT) as f:
        artifact = json.load(f)

    ct = ContractType(
        abi=artifact["abi"],
        deploymentBytecode={"bytecode": artifact["bytecode"]["object"]},
        runtimeBytecode={"bytecode": artifact["deployedBytecode"]["object"]},
    )
    container = ContractContainer(ct)
    return container.deploy(sender=accounts[0])


def _compare_vy_sol(vy_result, sol_result, context):
    """Compare Vyper and Solidity results, with helpful error message."""
    assert vy_result == sol_result, f"{context}:\n  Vyper = {vy_result}\n  Sol   = {sol_result}"


# ═══════════════════════════════════════════════════════════════════════════
# FullMath
# ═══════════════════════════════════════════════════════════════════════════


class TestFullMathFuzz:
    """Fuzz FullMath.mul_div and mul_div_rounding_up against Solidity."""

    @given(a=uint256, b=uint256, d=st.integers(min_value=1, max_value=2**256 - 1))
    @settings(max_examples=200, deadline=None, suppress_health_check=[HealthCheck.too_slow])
    def test_mul_div(self, harness, sol_ref, a, b, d):
        try:
            vy = harness.test_mul_div(a, b, d)
        except Exception:
            with pytest.raises(Exception):
                sol_ref.test_mul_div(a, b, d)
            return

        try:
            sol = sol_ref.test_mul_div(a, b, d)
        except Exception:
            pytest.fail(f"Solidity reverted but Vyper returned {vy} for mul_div({a}, {b}, {d})")

        assert vy == sol, f"mul_div({a}, {b}, {d}): Vyper={vy}, Sol={sol}"

    @given(a=uint256, b=uint256, d=st.integers(min_value=1, max_value=2**256 - 1))
    @settings(max_examples=200, deadline=None, suppress_health_check=[HealthCheck.too_slow])
    def test_mul_div_rounding_up(self, harness, sol_ref, a, b, d):
        try:
            vy = harness.test_mul_div_rounding_up(a, b, d)
        except Exception:
            with pytest.raises(Exception):
                sol_ref.test_mul_div_rounding_up(a, b, d)
            return

        try:
            sol = sol_ref.test_mul_div_rounding_up(a, b, d)
        except Exception:
            pytest.fail(
                f"Solidity reverted but Vyper returned {vy} for mul_div_rounding_up({a}, {b}, {d})"
            )

        assert vy == sol, f"mul_div_rounding_up({a}, {b}, {d}): Vyper={vy}, Sol={sol}"


# ═══════════════════════════════════════════════════════════════════════════
# SqrtPriceMath
# ═══════════════════════════════════════════════════════════════════════════


class TestSqrtPriceMathFuzz:
    """Fuzz SqrtPriceMath against Solidity."""

    @given(
        sqrt_p=valid_sqrt_price,
        liquidity=uint128,
        amount=st.integers(min_value=0, max_value=2**128 - 1),
        zero_for_one=st.booleans(),
    )
    @settings(max_examples=200, deadline=None, suppress_health_check=[HealthCheck.too_slow])
    def test_get_next_sqrt_price_from_input(
        self, harness, sol_ref, sqrt_p, liquidity, amount, zero_for_one
    ):
        sqrt_p_u160 = min(sqrt_p, 2**160 - 1)
        liquidity_u128 = min(liquidity, 2**128 - 1)

        try:
            vy = harness.test_get_next_sqrt_price_from_input(
                sqrt_p_u160, liquidity_u128, amount, zero_for_one
            )
        except Exception:
            with pytest.raises(Exception):
                sol_ref.test_get_next_sqrt_price_from_input(
                    sqrt_p_u160, liquidity_u128, amount, zero_for_one
                )
            return

        try:
            sol = sol_ref.test_get_next_sqrt_price_from_input(
                sqrt_p_u160, liquidity_u128, amount, zero_for_one
            )
        except Exception:
            pytest.fail(f"Solidity reverted but Vyper returned {vy}")

        assert vy == sol, (
            f"from_input({sqrt_p_u160}, {liquidity_u128}, {amount}, {zero_for_one}): Vyper={vy}, Sol={sol}"
        )

    @given(
        sqrt_p=valid_sqrt_price,
        liquidity=uint128,
        amount=st.integers(min_value=1, max_value=2**128 - 1),
        zero_for_one=st.booleans(),
    )
    @settings(max_examples=200, deadline=None, suppress_health_check=[HealthCheck.too_slow])
    def test_get_next_sqrt_price_from_output(
        self, harness, sol_ref, sqrt_p, liquidity, amount, zero_for_one
    ):
        sqrt_p_u160 = min(sqrt_p, 2**160 - 1)
        liquidity_u128 = min(liquidity, 2**128 - 1)

        try:
            vy = harness.test_get_next_sqrt_price_from_output(
                sqrt_p_u160, liquidity_u128, amount, zero_for_one
            )
        except Exception:
            with pytest.raises(Exception):
                sol_ref.test_get_next_sqrt_price_from_output(
                    sqrt_p_u160, liquidity_u128, amount, zero_for_one
                )
            return

        try:
            sol = sol_ref.test_get_next_sqrt_price_from_output(
                sqrt_p_u160, liquidity_u128, amount, zero_for_one
            )
        except Exception:
            pytest.fail(f"Solidity reverted but Vyper returned {vy}")

        assert vy == sol, (
            f"from_output({sqrt_p_u160}, {liquidity_u128}, {amount}, {zero_for_one}): Vyper={vy}, Sol={sol}"
        )

    @given(
        sqrt_a=valid_sqrt_price,
        sqrt_b=valid_sqrt_price,
        liquidity=uint128,
        round_up=st.booleans(),
    )
    @settings(max_examples=200, deadline=None, suppress_health_check=[HealthCheck.too_slow])
    def test_get_amount0_delta(self, harness, sol_ref, sqrt_a, sqrt_b, liquidity, round_up):
        sqrt_a_u160 = min(sqrt_a, 2**160 - 1)
        sqrt_b_u160 = min(sqrt_b, 2**160 - 1)
        liquidity_u128 = min(liquidity, 2**128 - 1)

        try:
            vy = harness.test_get_amount0_delta(sqrt_a_u160, sqrt_b_u160, liquidity_u128, round_up)
        except Exception:
            with pytest.raises(Exception):
                sol_ref.test_get_amount0_delta(sqrt_a_u160, sqrt_b_u160, liquidity_u128, round_up)
            return

        try:
            sol = sol_ref.test_get_amount0_delta(sqrt_a_u160, sqrt_b_u160, liquidity_u128, round_up)
        except Exception:
            pytest.fail(f"Solidity reverted but Vyper returned {vy}")

        assert vy == sol, (
            f"amount0({sqrt_a_u160}, {sqrt_b_u160}, {liquidity_u128}, {round_up}): Vyper={vy}, Sol={sol}"
        )

    @given(
        sqrt_a=valid_sqrt_price,
        sqrt_b=valid_sqrt_price,
        liquidity=uint128,
        round_up=st.booleans(),
    )
    @settings(max_examples=200, deadline=None, suppress_health_check=[HealthCheck.too_slow])
    def test_get_amount1_delta(self, harness, sol_ref, sqrt_a, sqrt_b, liquidity, round_up):
        sqrt_a_u160 = min(sqrt_a, 2**160 - 1)
        sqrt_b_u160 = min(sqrt_b, 2**160 - 1)
        liquidity_u128 = min(liquidity, 2**128 - 1)

        try:
            vy = harness.test_get_amount1_delta(sqrt_a_u160, sqrt_b_u160, liquidity_u128, round_up)
        except Exception:
            with pytest.raises(Exception):
                sol_ref.test_get_amount1_delta(sqrt_a_u160, sqrt_b_u160, liquidity_u128, round_up)
            return

        try:
            sol = sol_ref.test_get_amount1_delta(sqrt_a_u160, sqrt_b_u160, liquidity_u128, round_up)
        except Exception:
            pytest.fail(f"Solidity reverted but Vyper returned {vy}")

        assert vy == sol, (
            f"amount1({sqrt_a_u160}, {sqrt_b_u160}, {liquidity_u128}, {round_up}): Vyper={vy}, Sol={sol}"
        )


# ═══════════════════════════════════════════════════════════════════════════
# TickMath
# ═══════════════════════════════════════════════════════════════════════════


class TestTickMathFuzz:
    """Fuzz TickMath against Solidity."""

    @given(tick=valid_tick)
    @settings(max_examples=500, deadline=None, suppress_health_check=[HealthCheck.too_slow])
    def test_get_sqrt_ratio_at_tick(self, harness, sol_ref, tick):
        vy = harness.test_get_sqrt_ratio_at_tick(tick)
        sol = sol_ref.test_get_sqrt_ratio_at_tick(tick)
        assert vy == sol, f"get_sqrt_ratio_at_tick({tick}): Vyper={vy}, Sol={sol}"

    @given(sqrt_price=valid_sqrt_price)
    @settings(max_examples=500, deadline=None, suppress_health_check=[HealthCheck.too_slow])
    def test_get_tick_at_sqrt_ratio(self, harness, sol_ref, sqrt_price):
        sqrt_u160 = min(sqrt_price, 2**160 - 1)

        try:
            vy = harness.test_get_tick_at_sqrt_ratio(sqrt_u160)
        except Exception:
            with pytest.raises(Exception):
                sol_ref.test_get_tick_at_sqrt_ratio(sqrt_u160)
            return

        try:
            sol = sol_ref.test_get_tick_at_sqrt_ratio(sqrt_u160)
        except Exception:
            pytest.fail(f"Solidity reverted but Vyper returned {vy}")

        assert vy == sol, f"get_tick_at_sqrt_ratio({sqrt_u160}): Vyper={vy}, Sol={sol}"

    @given(tick=st.integers(min_value=-887271, max_value=887271))
    @settings(max_examples=200, deadline=None, suppress_health_check=[HealthCheck.too_slow])
    def test_round_trip(self, harness, tick):
        """tick → price → tick should be identity (excluding boundary ±887272)."""
        price = harness.test_get_sqrt_ratio_at_tick(tick)
        recovered = harness.test_get_tick_at_sqrt_ratio(price)
        assert recovered == tick, f"tick {tick} → price {price} → tick {recovered}"


# ═══════════════════════════════════════════════════════════════════════════
# SwapMath (computeSwapStep)
# ═══════════════════════════════════════════════════════════════════════════


class TestSwapMathFuzz:
    """Fuzz SwapMath.computeSwapStep against Solidity."""

    @given(
        sqrt_current=valid_sqrt_price,
        sqrt_target=valid_sqrt_price,
        liquidity=uint128,
        amount_remaining=int256,
        fee_pips=fee_pips,
    )
    @settings(max_examples=500, deadline=None, suppress_health_check=[HealthCheck.too_slow])
    def test_compute_swap_step(
        self, harness, sol_ref, sqrt_current, sqrt_target, liquidity, amount_remaining, fee_pips
    ):
        sqrt_current_u160 = min(sqrt_current, 2**160 - 1)
        sqrt_target_u160 = min(sqrt_target, 2**160 - 1)
        liquidity_u128 = min(liquidity, 2**128 - 1)

        try:
            vy = harness.test_compute_swap_step(
                sqrt_current_u160, sqrt_target_u160, liquidity_u128, amount_remaining, fee_pips
            )
        except Exception:
            with pytest.raises(Exception):
                sol_ref.test_compute_swap_step(
                    sqrt_current_u160, sqrt_target_u160, liquidity_u128, amount_remaining, fee_pips
                )
            return

        try:
            sol = sol_ref.test_compute_swap_step(
                sqrt_current_u160, sqrt_target_u160, liquidity_u128, amount_remaining, fee_pips
            )
        except Exception:
            pytest.fail(
                f"Solidity reverted but Vyper returned {vy} for compute_swap_step({sqrt_current_u160}, {sqrt_target_u160}, {liquidity_u128}, {amount_remaining}, {fee_pips})"
            )

        assert vy == sol, (
            f"compute_swap_step({sqrt_current_u160}, {sqrt_target_u160}, "
            f"{liquidity_u128}, {amount_remaining}, {fee_pips}):\n"
            f"Vyper=({vy[0]}, {vy[1]}, {vy[2]}, {vy[3]})\n"
            f"Sol=({sol[0]}, {sol[1]}, {sol[2]}, {sol[3]})"
        )
