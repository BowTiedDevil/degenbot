"""Balancer V2 math routing — delegation-detection gate.

Ergo 6TLIJ5/FSD3CR: the weighted + stable companion ``swap_fn`` paths
route the core swap math through the ``degenbot-balancer-math`` Rust leaf
(``degenbot._ffi.balancer_*``) instead of the Python ``balancer/libraries/``
ports. Scaling orchestration + the stable ``_add_swap_fee_amount`` divUp
fee path stay Python (ADR-005: the math leaf is pure arithmetic; the I/O /
scaling / bytecode-detection stays Python-side).

The leaf is byte-for-byte cross-checked vs the Python oracle by the frozen
``degenbot-balancer-math/tests/oracle_crosscheck.rs`` snapshot at the unit
level; per §4.5 this module is the orchestration-level gate that spies on
the Rust seam to prove the routed path hits it with the right arguments
(\'the parity tests already cover the math\'). The §4.3 parity recompute
against the Python ``balancer/libraries/*`` ports retired with this
harness — no second live implementation remains.
"""

from __future__ import annotations

from fractions import Fraction

import pytest

import degenbot.balancer.pools as pools_mod
import degenbot.balancer.stable_pools as sp_mod
from degenbot.balancer.libraries.constants import ONE
from degenbot.balancer.pools import BalancerV2Pool
from degenbot.balancer.stable_pools import INVARIANT_V2
from degenbot._ffi import Bot
from tests.helpers.balancer_pool_factory import (
    make_balancer_stable_pool,
    make_balancer_weighted_pool,
)
from tests.helpers.erc20_factory import make_erc20


@pytest.fixture
def weighted_pool() -> BalancerV2Pool:
    bot = Bot()
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
    bot = Bot()
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
        weighted_pool.calculate_tokens_out_from_tokens_in(t0, t1, amount_in)

        # §4.5 delegation-detection: the Rust seam was hit exactly once for
        # the weighted subtract_fee + calc_out_given_in pair, with the
        # bytecode-detected `pow_version` (V2) threaded across as the `u8`
        # discriminant (arg index 5 of `_rs_calc_out_given_in`).
        assert len(weighted_spies["sub_fee"].calls) == 1
        assert len(weighted_spies["out_given_in"].calls) == 1
        assert weighted_spies["out_given_in"].calls[0][0][5] == 2  # version u8 (V2 + fast-paths)

    def test_calculate_tokens_in_routes_through_rust(self, weighted_pool, weighted_spies) -> None:
        t0, t1 = weighted_pool._tokens
        amount_out = 5_000 * ONE
        weighted_pool.calculate_tokens_in_from_tokens_out(t0, t1, amount_out)

        # §4.5 delegation-detection: the Rust seam was hit exactly once for
        # the weighted calc_in_given_out + add_swap_fee pair.
        assert len(weighted_spies["in_given_out"].calls) == 1
        assert len(weighted_spies["add_fee"].calls) == 1


class TestStableRouting:
    def test_calculate_tokens_out_routes_through_rust(self, stable_pool, stable_spies) -> None:
        t0, t1 = stable_pool._tokens
        amount_in = 10_000 * ONE
        stable_pool.calculate_tokens_out_from_tokens_in(t0, t1, amount_in)

        # §4.5 delegation-detection: the stable calculate_invariant_deployed
        # + calc_out_given_in + subtract_swap_fee Rust seams were all hit.
        assert len(stable_spies["inv_dep"].calls) == 1
        assert len(stable_spies["out_given_in"].calls) == 1
        assert len(stable_spies["sub_fee"].calls) == 1
