"""Solidly / Aerodrome / Camelot stable-math routing — delegation-detection gate.

Ergo 6TLIJ5/QF2PPN: the Solidly-stable swap-calc paths route the core math
through the ``degenbot-solidly-math`` Rust leaf
(``degenbot._ffi.solidly_calc_exact_in_stable_solidly`` /
``solidly_calc_exact_in_volatile``) instead of the Python
``calculations/solidly_stable.py`` + ``calculations/camelot.py`` ports. The
companion-level swap-strategy methods (in ``aerodrome/v2_pool_calc.py``) +
the ``SolidlyStableHop``-bound closure (in ``aerodrome/pools.py``) wrap the
Rust seam; the Python oracle's ``calc_d`` / ``calc_k`` / ``calc_f`` /
``get_y_solidly`` / ``f_camelot`` / ``k_camelot`` / ``get_y_camelot`` math is
retained as the §4.3 parity-oracle corpus for the ``test_calculations.py``
unit-math identity tests (per the rubric's
"stays-until-the-routing-lands" criterion — they remain the parity oracle
for any future Solidly-derived DEX variant with a new k/y pair, and the
on-chain-match work in ``test_aerodrome_v2_onchain_parity.py``).

The leaf is byte-for-byte cross-checked vs the Python oracle by the frozen
``rust/crates/degenbot-solidly-math/tests/oracle_crosscheck.rs`` snapshot at
the unit level; per §4.5 this module is the orchestration-level gate that
spies on the Rust seam to prove the routed path hits it with the right
arguments ('the parity tests already cover the math'). No §4.3
value-equivalence recompute was added — exactly one live implementation per
swap-math concern remains.
"""

from __future__ import annotations

from fractions import Fraction

import pytest

import degenbot.aerodrome.functions as functions_mod
import degenbot.aerodrome.v2_pool_calc as calc_mod
from degenbot.aerodrome.functions import calc_exact_in_stable
from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot._ffi import PyBot
from tests.helpers.aerodrome_pool_factory import make_aerodrome_v2_pool
from tests.helpers.erc20_factory import make_erc20


class _Spy:
    """Record calls to a Rust seam function, then delegate to the real impl."""

    def __init__(self, real) -> None:
        self.real = real
        self.calls: list[tuple] = []

    def __call__(self, *args, **kwargs):
        self.calls.append((args, kwargs))
        return self.real(*args, **kwargs)


@pytest.fixture
def stable_pool():
    bot = PyBot()
    t0 = make_erc20(
        bot,
        address="0x" + "a1" * 20,
        name="USD-C",
        symbol="USDC",
        decimals=6,
    )
    t1 = make_erc20(
        bot,
        address="0x" + "b2" * 20,
        name="DOLA",
        symbol="DOLA",
        decimals=18,
    )
    return make_aerodrome_v2_pool(
        address="0x" + "c3" * 20,
        token0=t0,
        token1=t1,
        factory="0x" + "ff" * 20,
        fee=Fraction(3, 1000),  # 0.3% Solidly fee → retained fraction 997/1000
        stable=True,
        reserves_token0=1_000_000_000_000,  # 1M USDC
        reserves_token1=1_000_000_000_000_000_000,  # 1.0 DOLA (1e18)
        py_bot=bot,
    )


@pytest.fixture
def volatile_pool():
    bot = PyBot()
    t0 = make_erc20(
        bot,
        address="0x" + "d4" * 20,
        name="WETH",
        symbol="WETH",
        decimals=18,
    )
    t1 = make_erc20(
        bot,
        address="0x" + "e5" * 20,
        name="USDC",
        symbol="USDC",
        decimals=6,
    )
    return make_aerodrome_v2_pool(
        address="0x" + "f6" * 20,
        token0=t0,
        token1=t1,
        factory="0x" + "ff" * 20,
        fee=Fraction(3, 1000),
        stable=False,
        reserves_token0=1_000_000_000_000_000_000,  # 1.0 WETH
        reserves_token1=3_000_000_000_000,  # 3000 USDC
        py_bot=bot,
    )


@pytest.fixture
def functions_stable_spy(monkeypatch) -> _Spy:
    spy = _Spy(functions_mod.solidly_calc_exact_in_stable_solidly)
    monkeypatch.setattr(functions_mod, "solidly_calc_exact_in_stable_solidly", spy)
    return spy


@pytest.fixture
def calc_strategy_spies(monkeypatch) -> dict[str, _Spy]:
    spies = {
        "stable": _Spy(calc_mod._rs_calc_exact_in_stable_solidly),
        "volatile": _Spy(calc_mod._rs_calc_exact_in_volatile),
    }
    monkeypatch.setattr(calc_mod, "_rs_calc_exact_in_stable_solidly", spies["stable"])
    monkeypatch.setattr(calc_mod, "_rs_calc_exact_in_volatile", spies["volatile"])
    return spies


class TestFunctionsRouting:
    def test_calc_exact_in_stable_routes_through_rust(self, functions_stable_spy) -> None:
        # Pure-wrapper path: aerodrome.functions.calc_exact_in_stable splits
        # the Python Fraction(997, 1000) into numer=997 + denom=1000 at the
        # seam because the pure-Rust math leaf takes two U256s (no
        # num-rational dep in the alloy-only core).
        fee = Fraction(997, 1000)
        result = calc_exact_in_stable(
            amount_in=1_000_000,
            token_in=0,
            reserves0=1_000_000_000_000_000_000,
            reserves1=1_000_000_000_000_000_000,
            decimals0=10**18,
            decimals1=10**18,
            fee=fee,
        )

        # §4.5 delegation-detection: the Rust seam was hit exactly once with
        # the 8 expected positional args (the Solidity Vyper `token_in` is a
        # uint8 sentinel; fee splits into numer/denom at the binding).
        assert len(functions_stable_spy.calls) == 1
        args, _kwargs = functions_stable_spy.calls[0]
        assert len(args) == 8
        assert args[0] == 1_000_000  # amount_in
        assert args[1] == 0  # token_in
        assert args[6] == 997  # fee_numer (retained post-fee num)
        assert args[7] == 1000  # fee_denom
        assert result > 0


class TestPoolCalcRouting:
    def test_stable_swap_routes_through_rust(self, stable_pool, calc_strategy_spies) -> None:
        t0, t1 = stable_pool._token0, stable_pool._token1
        amount_in = 100_000_000  # 100 USDC
        stable_pool.calculate_tokens_out_from_tokens_in(t0, amount_in)

        # §4.5 delegation-detection: the strategy's stable swap method routed
        # through the Rust seam (not the Python oracle's
        # `calc_exact_in_stable`); the volatile seam was NOT touched.
        assert len(calc_strategy_spies["stable"].calls) == 1
        assert len(calc_strategy_spies["volatile"].calls) == 0

    def test_volatile_swap_routes_through_rust(self, volatile_pool, calc_strategy_spies) -> None:
        t0, t1 = volatile_pool._token0, volatile_pool._token1
        amount_in = 100_000_000_000_000_000  # 0.1 WETH
        volatile_pool.calculate_tokens_out_from_tokens_in(t0, amount_in)

        # §4.5 delegation-detection: the strategy's volatile swap method routed
        # through the Rust seam (not the Python oracle's
        # `calc_exact_in_volatile`); the stable seam was NOT touched.
        assert len(calc_strategy_spies["volatile"].calls) == 1
        assert len(calc_strategy_spies["stable"].calls) == 0
