"""
Cross-implementation fuzz tests: Vyper port vs original Solidity.

Deploys both the Vyper test_harness and the Solidity SolReference on the
same Anvil instance, then uses Hypothesis to compare outputs across a wide
range of inputs. Any disagreement is a bug in the Vyper port.

Success and revert parity both go through _expect_parity: a case agrees
when both sides return equal values, or both revert with the same exposed
revert identity.
"""

import json
from pathlib import Path

from ape.exceptions import ApeAttributeError
from hypothesis import HealthCheck, given, settings
from hypothesis import strategies as st
import pytest

from .conftest_shared import (
    NO_REVERT_DATA,
    capture_revert,
    raise_custom_error,
    raise_harness_error,
    raise_revert,
    revert_id,
    reverts_match,
)

# Committed solc 0.7.6 artifact (see scripts/build-sol-reference-artifact.sh):
# the original Solidity V3-math libraries, deployed next to the Vyper
# test_harness for cross-implementation fuzzing.
SOL_REF_ARTIFACT = (
    Path(__file__).resolve().parents[2]
    / "contracts"
    / "vyper"
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
            "with contracts/scripts/build-sol-reference-artifact.sh"
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


def _expect_parity(vy_call, sol_call, context):
    """Assert one fuzz case reaches the same verdict on both implementations.

    Both sides must return equal values, or both must revert at the contract
    with matching exposed revert identity (conftest_shared.reverts_match).
    Anything else — one side reverting, exposed reverts that disagree, or any
    non-revert exception such as a missing method or a fixture failure —
    fails the test.
    """
    vy_value, vy_revert = capture_revert(vy_call)
    sol_value, sol_revert = capture_revert(sol_call)

    if vy_revert is None and sol_revert is None:
        assert vy_value == sol_value, (
            f"{context}: results disagree:\n  Vyper = {vy_value}\n  Sol   = {sol_value}"
        )
        return

    vy_verdict = revert_id(vy_revert) if vy_revert is not None else f"returned {vy_value}"
    sol_verdict = revert_id(sol_revert) if sol_revert is not None else f"returned {sol_value}"
    assert vy_revert is not None and sol_revert is not None, (
        f"{context}: verdicts disagree:\n  Vyper = {vy_verdict}\n  Sol   = {sol_verdict}"
    )
    assert reverts_match(vy_revert, sol_revert), (
        f"{context}: reverts disagree:\n  Vyper = {vy_verdict}\n  Sol   = {sol_verdict}"
    )


# ═══════════════════════════════════════════════════════════════════════════
# FullMath
# ═══════════════════════════════════════════════════════════════════════════


class TestFullMathFuzz:
    """Fuzz FullMath.mul_div and mul_div_rounding_up against Solidity."""

    @given(a=uint256, b=uint256, d=st.integers(min_value=1, max_value=2**256 - 1))
    @settings(max_examples=200, deadline=None, suppress_health_check=[HealthCheck.too_slow])
    def test_mul_div(self, harness, sol_ref, a, b, d):
        _expect_parity(
            lambda: harness.test_mul_div(a, b, d),
            lambda: sol_ref.test_mul_div(a, b, d),
            f"mul_div({a}, {b}, {d})",
        )

    @given(a=uint256, b=uint256, d=st.integers(min_value=1, max_value=2**256 - 1))
    @settings(max_examples=200, deadline=None, suppress_health_check=[HealthCheck.too_slow])
    def test_mul_div_rounding_up(self, harness, sol_ref, a, b, d):
        _expect_parity(
            lambda: harness.test_mul_div_rounding_up(a, b, d),
            lambda: sol_ref.test_mul_div_rounding_up(a, b, d),
            f"mul_div_rounding_up({a}, {b}, {d})",
        )


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

        _expect_parity(
            lambda: harness.test_get_next_sqrt_price_from_input(
                sqrt_p_u160, liquidity_u128, amount, zero_for_one
            ),
            lambda: sol_ref.test_get_next_sqrt_price_from_input(
                sqrt_p_u160, liquidity_u128, amount, zero_for_one
            ),
            f"from_input({sqrt_p_u160}, {liquidity_u128}, {amount}, {zero_for_one})",
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

        _expect_parity(
            lambda: harness.test_get_next_sqrt_price_from_output(
                sqrt_p_u160, liquidity_u128, amount, zero_for_one
            ),
            lambda: sol_ref.test_get_next_sqrt_price_from_output(
                sqrt_p_u160, liquidity_u128, amount, zero_for_one
            ),
            f"from_output({sqrt_p_u160}, {liquidity_u128}, {amount}, {zero_for_one})",
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

        _expect_parity(
            lambda: harness.test_get_amount0_delta(
                sqrt_a_u160, sqrt_b_u160, liquidity_u128, round_up
            ),
            lambda: sol_ref.test_get_amount0_delta(
                sqrt_a_u160, sqrt_b_u160, liquidity_u128, round_up
            ),
            f"amount0({sqrt_a_u160}, {sqrt_b_u160}, {liquidity_u128}, {round_up})",
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

        _expect_parity(
            lambda: harness.test_get_amount1_delta(
                sqrt_a_u160, sqrt_b_u160, liquidity_u128, round_up
            ),
            lambda: sol_ref.test_get_amount1_delta(
                sqrt_a_u160, sqrt_b_u160, liquidity_u128, round_up
            ),
            f"amount1({sqrt_a_u160}, {sqrt_b_u160}, {liquidity_u128}, {round_up})",
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

        _expect_parity(
            lambda: harness.test_get_tick_at_sqrt_ratio(sqrt_u160),
            lambda: sol_ref.test_get_tick_at_sqrt_ratio(sqrt_u160),
            f"get_tick_at_sqrt_ratio({sqrt_u160})",
        )

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

        _expect_parity(
            lambda: harness.test_compute_swap_step(
                sqrt_current_u160, sqrt_target_u160, liquidity_u128, amount_remaining, fee_pips
            ),
            lambda: sol_ref.test_compute_swap_step(
                sqrt_current_u160, sqrt_target_u160, liquidity_u128, amount_remaining, fee_pips
            ),
            f"compute_swap_step({sqrt_current_u160}, {sqrt_target_u160}, "
            f"{liquidity_u128}, {amount_remaining}, {fee_pips})",
        )


# ── Meta-tests: the parity helper itself ──


def _raise_missing_method():
    msg = "'test_harness' has no attribute 'no_such_method'"
    raise ApeAttributeError(msg)


class TestRevertParityHelpers:
    """_expect_parity counts only matching contract reverts as parity.

    Chain-free: the callables are stubs, so these pin the failure modes a
    catch-all ``except`` destroys without deploying anything.
    """

    def test_equal_results_are_parity(self):
        _expect_parity(lambda: 7, lambda: 7, "ctx")

    def test_result_mismatch_fails(self):
        with pytest.raises(AssertionError, match="results disagree"):
            _expect_parity(lambda: 7, lambda: 8, "ctx")

    def test_result_vs_revert_fails(self):
        with pytest.raises(AssertionError, match="verdicts disagree"):
            _expect_parity(lambda: 7, raise_revert(NO_REVERT_DATA), "ctx")

    def test_revert_vs_result_fails(self):
        with pytest.raises(AssertionError, match="verdicts disagree"):
            _expect_parity(raise_revert(NO_REVERT_DATA), lambda: 7, "ctx")

    def test_matching_revert_messages_are_parity(self):
        _expect_parity(
            raise_revert("FullMath: overflow"), raise_revert("FullMath: overflow"), "ctx"
        )

    def test_bare_reverts_are_parity(self):
        _expect_parity(raise_revert(NO_REVERT_DATA), raise_revert(NO_REVERT_DATA), "ctx")

    def test_annotated_revert_matches_bare_revert(self):
        """A revert that exposes no identity cannot disagree: the Vyper port
        annotates guards the Solidity original leaves bare (`assert ...,
        "FullMath: overflow"` vs bare `require`)."""
        _expect_parity(raise_revert("FullMath: overflow"), raise_revert(NO_REVERT_DATA), "ctx")

    def test_different_revert_messages_fail(self):
        with pytest.raises(AssertionError, match="reverts disagree"):
            _expect_parity(raise_revert("T"), raise_revert("R"), "ctx")

    def test_matching_custom_errors_are_parity(self):
        _expect_parity(
            raise_custom_error("InvalidCommand", opcode=32),
            raise_custom_error("InvalidCommand", opcode=32),
            "ctx",
        )

    def test_different_custom_error_fields_fail(self):
        with pytest.raises(AssertionError, match="reverts disagree"):
            _expect_parity(
                raise_custom_error("InvalidCommand", opcode=7),
                raise_custom_error("InvalidCommand", opcode=32),
                "ctx",
            )

    def test_different_custom_error_names_fail(self):
        with pytest.raises(AssertionError, match="reverts disagree"):
            _expect_parity(
                raise_custom_error("InvalidCommand", opcode=32),
                raise_custom_error("ManagerLocked", opcode=32),
                "ctx",
            )

    def test_unrelated_exception_on_vyper_side_propagates(self):
        with pytest.raises(RuntimeError, match="harness exploded"):
            _expect_parity(raise_harness_error, lambda: 7, "ctx")

    def test_unrelated_exception_on_solidity_side_propagates(self):
        with pytest.raises(RuntimeError, match="harness exploded"):
            _expect_parity(lambda: 7, raise_harness_error, "ctx")

    def test_missing_method_propagates(self):
        with pytest.raises(ApeAttributeError, match="no_such_method"):
            _expect_parity(_raise_missing_method, lambda: 7, "ctx")
