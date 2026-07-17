"""Solidly / Aerodrome / Camelot stable-math routing — delegation-detection gate.

Ergo 6TLIJ5/QF2PPN: the Solidly-stable swap-calc paths route the core math
through the ``degenbot-solidly-math`` Rust leaf
(``degenbot._ffi.solidly_calc_exact_in_stable_solidly`` /
``solidly_calc_exact_in_volatile``) instead of the Python
``calculations/solidly_stable.py`` + ``calculations/camelot.py`` ports. The
companion-level swap-strategy methods (in ``aerodrome/v2_pool_calc.py``)
wrap the Rust seam; the SolidlyStableHop-bound closure (in
``aerodrome/pools.py``) wraps the same Rust leaf in-line
(ergo S5SJXF / NFYOWI retired the redundant
``aerodrome.functions.calc_exact_in_stable`` Fraction-splitting wrapper —
its delegation is now asserted directly against
``aerodrome.math.calc_exact_in_stable_solidly``, with no intermediate
Python callable). The Python oracle's ``calc_d`` / ``calc_k`` /
``calc_f`` / ``get_y_solidly`` / ``f_camelot`` / ``k_camelot`` /
``get_y_camelot`` math is retained as the §4.3 parity-oracle corpus for the
``test_calculations.py`` unit-math identity tests, and the on-chain-match
work in ``test_aerodrome_v2_onchain_parity.py``.

The leaf is byte-for-byte cross-checked vs the Python oracle by the frozen
``rust/crates/degenbot-solidly-math/tests/oracle_crosscheck.rs`` snapshot at
the unit level; per §4.5 this module is the orchestration-level gate that
spies on the Rust seam to prove the routed path hits it with the right
arguments ('the parity tests already cover the math').
"""

from __future__ import annotations

from fractions import Fraction

import pytest

import degenbot.aerodrome.v2_pool_calc as calc_mod
from degenbot.bot import PyBot
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
def calc_strategy_spies(monkeypatch) -> dict[str, _Spy]:
    spies = {
        "stable": _Spy(calc_mod._rs_calc_exact_in_stable_solidly),
        "volatile": _Spy(calc_mod._rs_calc_exact_in_volatile),
        "stable_out": _Spy(calc_mod._rs_calc_exact_out_stable_solidly),
    }
    monkeypatch.setattr(calc_mod, "_rs_calc_exact_in_stable_solidly", spies["stable"])
    monkeypatch.setattr(calc_mod, "_rs_calc_exact_in_volatile", spies["volatile"])
    monkeypatch.setattr(calc_mod, "_rs_calc_exact_out_stable_solidly", spies["stable_out"])
    return spies


class TestPoolCalcRouting:
    def test_stable_swap_routes_through_rust(self, stable_pool, calc_strategy_spies) -> None:
        t0 = stable_pool._token0
        amount_in = 100_000_000  # 100 USDC
        stable_pool.calculate_tokens_out_from_tokens_in(t0, amount_in)

        # §4.5 delegation-detection: the strategy's stable swap method routed
        # through the Rust seam (not the Python oracle's
        # `calc_exact_in_stable`); the volatile seam was NOT touched.
        assert len(calc_strategy_spies["stable"].calls) == 1
        assert len(calc_strategy_spies["volatile"].calls) == 0

    def test_volatile_swap_routes_through_rust(self, volatile_pool, calc_strategy_spies) -> None:
        t0 = volatile_pool._token0
        amount_in = 100_000_000_000_000_000  # 0.1 WETH
        volatile_pool.calculate_tokens_out_from_tokens_in(t0, amount_in)

        # §4.5 delegation-detection: the strategy's volatile swap method routed
        # through the Rust seam (not the Python oracle's
        # `calc_exact_in_volatile`); the stable seam was NOT touched.
        assert len(calc_strategy_spies["volatile"].calls) == 1
        assert len(calc_strategy_spies["stable"].calls) == 0


class TestStableExactOut:
    """Aerodrome stable exact-out roundtrip — the inverse of the stable exact-in.

    The stable exact-out (`calc_exact_out_stable_solidly`) was previously a
    `NotImplementedError` stub (ergo S5SJXF / LTLR2K — a gap-fill, not a port:
    the Python leaf never existed). The §4.2 oracle is the property-based
    roundtrip over the Solidly/Aerodrome stable invariant: `exact_out` is the
    MINIMUM input producing at least the requested output, so

        exact_out(exact_in(amount_in) ) ≤ amount_in
        exact_in(exact_out(out)        ) ≥ out
        exact_in(exact_out(out) - 1     ) < out   (the minimum is tight)
    """

    def test_routes_through_rust(self, stable_pool, calc_strategy_spies) -> None:
        t1 = stable_pool._token1  # the OUT token (exact-out direction)
        stable_pool.calculate_tokens_in_from_tokens_out(1_000_000, t1)

        # §4.5 delegation-detection: the stable exact-out strategy routed
        # through the Rust `calc_exact_out_stable_solidly` seam.
        assert len(calc_strategy_spies["stable_out"].calls) == 1
        assert len(calc_strategy_spies["stable"].calls) == 0
        assert len(calc_strategy_spies["volatile"].calls) == 0

    def test_roundtrip_is_the_minimum_input(self, stable_pool) -> None:
        t0 = stable_pool._token0
        t1 = stable_pool._token1
        amount_in = 1_000_000  # 1 USDC
        out = stable_pool.calculate_tokens_out_from_tokens_in(t0, amount_in)
        recovered = stable_pool.calculate_tokens_in_from_tokens_out(out, t1)

        # (a) The inverse returns the MINIMUM input producing ≥ out, so it is
        #     ≤ the seed amount_in (which over-paid or exactly paid for the
        #     floored output).
        assert 0 < recovered <= amount_in

        # (b) Correctness: the recovered input produces at least the target
        #     output when fed back through exact-in.
        assert stable_pool.calculate_tokens_out_from_tokens_in(t0, recovered) >= out

        # (c) Tightness: one wei less than `recovered` no longer reaches `out`
        #     (so `recovered` is the true minimum — the getAmountIn answer).
        if recovered > 1:
            assert stable_pool.calculate_tokens_out_from_tokens_in(t0, recovered - 1) < out
