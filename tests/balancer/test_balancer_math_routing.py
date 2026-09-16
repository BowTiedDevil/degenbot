"""Balancer V2 math routing — delegation-detection gate.

The weighted + stable companion swap paths are thin Python driver shells
over the Rust core's pair-swap surface (``LiquidityPool.calculate_tokens_*_for_pair``).
Token + scaling-factor resolution stays Python-side; the swap math in its
entirety is Rust-owned.

This module is the orchestration-level gate that spies on the shell→FFI
seam to prove the routed path hits the Rust core exactly once with the
driver-resolved arguments ('the parity tests already cover the math').
"""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING

import pytest

from degenbot._ffi import Bot
from degenbot.balancer.libraries.constants import ONE
from degenbot.balancer.stable_pools import INVARIANT_V2
from tests.helpers.balancer_pool_factory import (
    make_balancer_stable_pool,
    make_balancer_weighted_pool,
)
from tests.helpers.erc20_factory import make_erc20

if TYPE_CHECKING:
    from degenbot.balancer.pools import BalancerV2Pool


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


class _FfiProxy:
    """Wrap the Rust FFI pool handle, recording the shell→core pair calls.

    The pyo3 class surface is immutable, so the companion's ``_py_pool``
    attribute is swapped for this proxy: unspyed members delegate, the
    pair-swap surface records + delegates.
    """

    def __init__(self, inner) -> None:
        self._inner = inner
        self.pair_out_calls: list[dict] = []
        self.pair_in_calls: list[dict] = []

    def __getattr__(self, name: str):
        return getattr(self._inner, name)

    def calculate_tokens_out_for_pair(self, **kwargs):
        self.pair_out_calls.append(kwargs)
        return self._inner.calculate_tokens_out_for_pair(**kwargs)

    def calculate_tokens_in_for_pair(self, **kwargs):
        self.pair_in_calls.append(kwargs)
        return self._inner.calculate_tokens_in_for_pair(**kwargs)


@pytest.fixture
def weighted_spies(weighted_pool: BalancerV2Pool) -> _FfiProxy:
    proxy = _FfiProxy(weighted_pool._py_pool)
    weighted_pool._py_pool = proxy
    return proxy


@pytest.fixture
def stable_spies(stable_pool: BalancerV2Pool) -> _FfiProxy:
    proxy = _FfiProxy(stable_pool._py_pool)
    stable_pool._py_pool = proxy
    return proxy


class TestWeightedRouting:
    def test_calculate_tokens_out_routes_through_rust(self, weighted_pool, weighted_spies) -> None:
        t0, t1 = weighted_pool._tokens
        amount_in = 10_000 * ONE
        weighted_pool.calculate_tokens_out_from_tokens_in(t0, t1, amount_in)

        # Delegation-detection: the shell hit the Rust core pair surface
        # exactly once, with the driver-resolved token indices.
        assert len(weighted_spies.pair_out_calls) == 1
        call = weighted_spies.pair_out_calls[0]
        assert call["index_in"] == 0
        assert call["index_out"] == 1
        assert call["amount_in"] == amount_in
        assert call["override_balances"] is None

    def test_calculate_tokens_in_routes_through_rust(self, weighted_pool, weighted_spies) -> None:
        t0, t1 = weighted_pool._tokens
        amount_out = 5_000 * ONE
        weighted_pool.calculate_tokens_in_from_tokens_out(t0, t1, amount_out)

        assert len(weighted_spies.pair_in_calls) == 1
        assert len(weighted_spies.pair_out_calls) == 0


class TestStableRouting:
    def test_calculate_tokens_out_routes_through_rust(self, stable_pool, stable_spies) -> None:
        t0, t1 = stable_pool._tokens
        amount_in = 10_000 * ONE
        stable_pool.calculate_tokens_out_from_tokens_in(t0, t1, amount_in)

        assert len(stable_spies.pair_out_calls) == 1
        call = stable_spies.pair_out_calls[0]
        assert call["index_in"] == 0
        assert call["index_out"] == 1
        assert call["amount_in"] == amount_in
        # Static rates → no explicit scaling-factor override
        assert call["override_scaling_factors"] is None
