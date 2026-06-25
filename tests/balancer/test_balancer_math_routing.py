"""Balancer V2 math routing — delegation-detection + parity cross-check.

Ergo 6TLIJ5/FSD3CR: the weighted + stable companion ``swap_fn`` paths route the
core swap math through the ``degenbot-balancer-math`` Rust leaf
(``degenbot_rs.balancer_*``) instead of the Python ``balancer/libraries/``
ports. Scaling orchestration + the stable ``_add_swap_fee_amount`` divUp fee
path stay Python (ADR-005: the math leaf is pure arithmetic; the I/O /
scaling / bytecode-detection stays Python-side).

The leaf is byte-for-byte cross-checked vs the Python oracle by the frozen
``oracle_crosscheck.rs`` snapshot at the unit level; this module is the
orchestration-level gate — it spies on the Rust seam to prove the routed path
hits it with the right arguments, and recomputes the result via the Python
oracle (the ``balancer/libraries/*`` ports, kept live as the parity oracle)
to prove the routing is byte-equivalent end-to-end.
"""

from __future__ import annotations

from fractions import Fraction

import pytest

import degenbot.balancer.pools as pools_mod
import degenbot.balancer.stable_pools as sp_mod
from degenbot.balancer.libraries import fixed_point, scaling_helpers, stable_math, weighted_math
from degenbot.balancer.libraries.constants import PowVersion
from degenbot.balancer.pools import BalancerV2Pool
from degenbot.balancer.stable_pools import INVARIANT_V2
from degenbot.degenbot_rs import PyBot
from tests.helpers.balancer_pool_factory import (
    make_balancer_stable_pool,
    make_balancer_weighted_pool,
)
from tests.helpers.erc20_factory import make_erc20

ONE = fixed_point.ONE


@pytest.fixture
def weighted_pool() -> BalancerV2Pool:
    bot = PyBot()
    t0 = make_erc20(bot, address="0x" + "a1" * 20, name="T0", symbol="T0", decimals=18)
    t1 = make_erc20(bot, address="0x" + "b2" * 20, name="T1", symbol="T1", decimals=18)
    return make_balancer_weighted_pool(
        address="0x" + "c3" * 20,
        pool_id=bytes.fromhex("c3" * 20 + "0002" + "0" * 20),
        vault="0x" + "ba" * 20,
        tokens=[t0, t1],
        balances=[1_000_000 * ONE, 2_000_000 * ONE],
        fee=Fraction(3, 1000),  # 0.3%
        weights=[ONE // 2, ONE // 2],
        pow_version=2,  # V2 fast-paths
    )


@pytest.fixture
def stable_pool() -> BalancerV2Pool:
    bot = PyBot()
    t0 = make_erc20(bot, address="0x" + "d4" * 20, name="S0", symbol="S0", decimals=18)
    t1 = make_erc20(bot, address="0x" + "e5" * 20, name="S1", symbol="S1", decimals=18)
    return make_balancer_stable_pool(
        address="0x" + "f6" * 20,
        pool_id=bytes.fromhex("f6" * 20 + "0002" + "0" * 20),
        vault="0x" + "ba" * 20,
        tokens=[t0, t1],
        balances=[1_000_000 * ONE, 2_000_000 * ONE],
        fee=Fraction(3, 1000),
        amp=100_000,  # amp = 100 * AMP_PRECISION (1000) — the raw amplified coefficient
        scaling_factors=[ONE, ONE],  # 18-decimals → identity scaling
        invariant_version=INVARIANT_V2,
    )


class _Spy:
    """Record calls to a Rust seam function, then delegate to the real impl."""

    def __init__(self, real) -> None:
        self.real = real
        self.calls: list[tuple] = []

    def __call__(self, *args, **kwargs):
        self.calls.append((args, kwargs))
        return self.real(*args, **kwargs)


@pytest.fixture
def weighted_spies(monkeypatch) -> dict[str, _Spy]:
    spies = {
        "sub_fee": _Spy(pools_mod._rs_subtract_swap_fee_amount),
        "out_given_in": _Spy(pools_mod._rs_calc_out_given_in),
        "in_given_out": _Spy(pools_mod._rs_calc_in_given_out),
        "add_fee": _Spy(pools_mod._rs_add_swap_fee_amount),
    }
    monkeypatch.setattr(pools_mod, "_rs_subtract_swap_fee_amount", spies["sub_fee"])
    monkeypatch.setattr(pools_mod, "_rs_calc_out_given_in", spies["out_given_in"])
    monkeypatch.setattr(pools_mod, "_rs_calc_in_given_out", spies["in_given_out"])
    monkeypatch.setattr(pools_mod, "_rs_add_swap_fee_amount", spies["add_fee"])
    return spies


@pytest.fixture
def stable_spies(monkeypatch) -> dict[str, _Spy]:
    spies = {
        "sub_fee": _Spy(sp_mod._rs_subtract_swap_fee_amount),
        "out_given_in": _Spy(sp_mod._rs_calc_out_given_in),
        "in_given_out": _Spy(sp_mod._rs_calc_in_given_out),
        "inv": _Spy(sp_mod._rs_calculate_invariant),
        "inv_dep": _Spy(sp_mod._rs_calculate_invariant_deployed),
    }
    monkeypatch.setattr(sp_mod, "_rs_subtract_swap_fee_amount", spies["sub_fee"])
    monkeypatch.setattr(sp_mod, "_rs_calc_out_given_in", spies["out_given_in"])
    monkeypatch.setattr(sp_mod, "_rs_calc_in_given_out", spies["in_given_out"])
    monkeypatch.setattr(sp_mod, "_rs_calculate_invariant", spies["inv"])
    monkeypatch.setattr(sp_mod, "_rs_calculate_invariant_deployed", spies["inv_dep"])
    return spies


class TestWeightedRouting:
    def test_calculate_tokens_out_routes_through_rust(self, weighted_pool, weighted_spies) -> None:
        t0, t1 = weighted_pool._tokens
        amount_in = 10_000 * ONE
        result = weighted_pool.calculate_tokens_out_from_tokens_in(t0, t1, amount_in)

        # Delegation-detection: the Rust seam was hit for fee-subtract + out_given_in.
        assert len(weighted_spies["sub_fee"].calls) == 1
        assert len(weighted_spies["out_given_in"].calls) == 1
        assert weighted_spies["out_given_in"].calls[0][0][5] == 2  # version u8

        # Parity: recompute via the Python oracle over the same orchestration.
        fee_scaled = int(weighted_pool.fee * BalancerV2Pool.FEE_DENOMINATOR)
        amt_after_fee = weighted_math._subtract_swap_fee_amount(amount_in, fee_scaled)
        balances = list(weighted_pool.balances)
        scaling_helpers._upscale_array(
            amounts=balances,
            scaling_factors=weighted_pool.scaling_factors,
        )
        amt_scaled = scaling_helpers._upscale(
            amt_after_fee,
            scaling_factor=weighted_pool.scaling_factors[0],
        )
        expected_out_scaled = weighted_math._calc_out_given_in(
            balance_in=int(balances[0]),
            weight_in=weighted_pool.weights[0],
            balance_out=int(balances[1]),
            weight_out=weighted_pool.weights[1],
            amount_in=int(amt_scaled),
            version=PowVersion.V2,
        )
        expected = int(
            scaling_helpers._downscale_down(
                amount=expected_out_scaled,
                scaling_factor=weighted_pool.scaling_factors[1],
            ),
        )
        assert result == expected

    def test_calculate_tokens_in_routes_through_rust(self, weighted_pool, weighted_spies) -> None:
        t0, t1 = weighted_pool._tokens
        amount_out = 5_000 * ONE
        result = weighted_pool.calculate_tokens_in_from_tokens_out(t0, t1, amount_out)

        # Delegation-detection: the Rust seam was hit for in_given_out + add_fee.
        assert len(weighted_spies["in_given_out"].calls) == 1
        assert len(weighted_spies["add_fee"].calls) == 1

        # Parity: recompute via the Python oracle over the scaled GIVEN_OUT path.
        fee_scaled = int(weighted_pool.fee * BalancerV2Pool.FEE_DENOMINATOR)
        balances = list(weighted_pool.balances)
        scaling_helpers._upscale_array(
            amounts=balances,
            scaling_factors=weighted_pool.scaling_factors,
        )
        amt_out_scaled = scaling_helpers._upscale(
            amount_out,
            scaling_factor=weighted_pool.scaling_factors[1],
        )
        amount_in_scaled = weighted_math._calc_in_given_out(
            balance_in=int(balances[0]),
            weight_in=weighted_pool.weights[0],
            balance_out=int(balances[1]),
            weight_out=weighted_pool.weights[1],
            amount_out=int(amt_out_scaled),
            version=PowVersion.V2,
        )
        amount_in_token = int(
            scaling_helpers._downscale_up(
                amount=amount_in_scaled,
                scaling_factor=weighted_pool.scaling_factors[0],
            ),
        )
        expected = weighted_math._add_swap_fee_amount(amount_in_token, fee_scaled)
        assert result == expected


class TestStableRouting:
    def test_calculate_tokens_out_routes_through_rust(self, stable_pool, stable_spies) -> None:
        t0, t1 = stable_pool._tokens
        amount_in = 10_000 * ONE
        result = stable_pool.calculate_tokens_out_from_tokens_in(t0, t1, amount_in)

        # Delegation-detection: V2 invariant + out_given_in seam hit.
        assert len(stable_spies["inv_dep"].calls) == 1
        assert len(stable_spies["out_given_in"].calls) == 1
        assert len(stable_spies["sub_fee"].calls) == 1

        # Parity: recompute via the Python oracle (no rate provider → identity scaling).
        fee_scaled = int(stable_pool.fee * BalancerV2Pool.FEE_DENOMINATOR)
        amt_after_fee = weighted_math._subtract_swap_fee_amount(amount_in, fee_scaled)
        balances = list(stable_pool.balances)
        inv = stable_math._calculate_invariant_deployed(
            stable_pool.amp,
            balances,
            round_up=True,
        )
        expected = stable_math._calc_out_given_in(
            stable_pool.amp,
            balances,
            0,
            1,
            amt_after_fee,
            inv,
        )
        # identity scaling (18-dec) → downscale is a no-op
        assert result == expected
