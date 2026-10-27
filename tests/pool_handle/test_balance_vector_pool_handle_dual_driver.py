"""Dual-driver parity test for the prototype structural Pool handle (balance-vector)."""

from __future__ import annotations

import pytest

from tests.helpers.pool_handle_factory import (
    ONE,
    make_balancer_stable_handle,
    make_balancer_weighted_handle,
    make_curve_handle,
)


@pytest.fixture
def curve_pool():
    return make_curve_handle()


@pytest.fixture
def balancer_weighted_pool():
    return make_balancer_weighted_handle()


def test_curve_structure_and_identity(curve_pool) -> None:
    assert curve_pool.structure() == "balance_vector"
    identity, variant = curve_pool.identity()
    assert identity == "balance_vector"
    assert variant == "curve"


def test_curve_balance_vector_view(curve_pool) -> None:
    bv = curve_pool.balance_vector()

    assert bv.n_tokens == 2
    assert len(bv.tokens) == 2
    assert len(bv.balances) == 2
    assert bv.tokens[0].lower() == "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    assert bv.balances[0] == ONE


def test_curve_calculate_tokens_out_matches_recorded_constant(curve_pool) -> None:
    # Standard stableswap (A=100, a_precision=100, equal 1e18 balances, zero
    # fee), swap 1e18 token0→token1. The recorded constant 934112765606210873
    # is cross-checked against the independent Rust `simulate_curve_stableswap_swap`
    # and an independent pure-Python Vyper-port re-derivation — a non-circular
    # oracle (ADR-005 Tier-2 recorded-constant shape).
    out = curve_pool.calculate_tokens_out(True, ONE)

    assert out is not None
    assert out == 934_112_765_606_210_873
    # Symmetry (equal balances → equal reverse swap)
    out_rev = curve_pool.calculate_tokens_out(False, ONE)

    assert out_rev == out

    # Monotonicity
    out_bigger = curve_pool.calculate_tokens_out(True, 2 * ONE)

    assert out_bigger > out


def test_balancer_weighted_structure_and_identity(balancer_weighted_pool) -> None:
    assert balancer_weighted_pool.structure() == "balance_vector"
    identity, variant = balancer_weighted_pool.identity()
    assert identity == "balance_vector"
    assert variant == "balancer_weighted"


def test_balancer_weighted_swap_matches_closed_form(balancer_weighted_pool) -> None:
    # Equal weights + zero fee → constant-product:
    #   out = B_out * amount_in / (B_in + amount_in)
    #   = 1_000_000 * 1_000 / 1_001_000 = 999 (integer floor)
    out = balancer_weighted_pool.calculate_tokens_out(True, 1_000)

    assert out is not None
    assert out == 999


@pytest.fixture
def balancer_stable_pool():
    return make_balancer_stable_handle()


def test_balancer_stable_structure_and_identity(balancer_stable_pool) -> None:
    assert balancer_stable_pool.structure() == "balance_vector"
    identity, variant = balancer_stable_pool.identity()
    assert identity == "balance_vector"
    assert variant == "balancer_stable"


def test_balancer_stable_swap_matches_companion_oracle(balancer_stable_pool) -> None:
    # No closed form for StableMath; the recorded constant 989 is cross-checked
    # against the independent pure-Python BalancerV2StablePool companion (same
    # pure-math leaf, independent marshalling) for the same fixture. Symmetry
    # (equal pools → equal reverse swaps) + monotonicity are the weaker-oracle
    # sanity checks (ADR-005 Tier 2 non-closed-form shape).
    out = balancer_stable_pool.calculate_tokens_out(True, 1_000)

    assert out is not None
    assert out == 989
    # Symmetry
    out_rev = balancer_stable_pool.calculate_tokens_out(False, 1_000)

    assert out_rev == out

    # Monotonicity
    out_bigger = balancer_stable_pool.calculate_tokens_out(True, 10_000)

    assert out_bigger > out
