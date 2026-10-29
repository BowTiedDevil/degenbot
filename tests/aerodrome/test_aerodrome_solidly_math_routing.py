"""Solidly / Aerodrome / Camelot stable-math routing — delegation-detection gate.

The Solidly-stable swap-calc paths route the core math
through the ``degenbot-solidly-math`` Rust leaf
(``degenbot._ffi.solidly_calc_exact_in_stable_solidly`` /
``solidly_calc_exact_in_volatile``) instead of the Python
``calculations/solidly_stable.py`` + ``calculations/camelot.py`` ports. The
companion-level swap-strategy methods (in ``aerodrome/v2_pool_calc.py``)
wrap the Rust seam; the SolidlyStableHop-bound closure (in
``aerodrome/pools.py``) wraps the same Rust leaf in-line
its delegation is asserted directly against
``aerodrome.math.calc_exact_in_stable_solidly``, with no intermediate
Python callable). The Python oracle's ``calc_d`` / ``calc_k`` /
``calc_f`` / ``get_y_solidly`` / ``f_camelot`` / ``k_camelot`` /
``get_y_camelot`` math is retained as the parity-oracle corpus for the
``test_calculations.py`` unit-math identity tests, and the on-chain-match
work in ``test_aerodrome_v2_onchain_parity.py``.

The leaf is byte-for-byte cross-checked vs the Python oracle by the frozen
``rust/crates/foundation/degenbot-math/tests/oracle_crosscheck_solidly.rs`` snapshot at
the unit level; this module is the orchestration-level gate that
spies on the Rust seam to prove the routed path hits it with the right
arguments ('the parity tests already cover the math').
"""

from __future__ import annotations

from fractions import Fraction

import pytest

import degenbot.aerodrome.v2_pool_quoter as quoter_mod
from tests.helpers.aerodrome_pool_factory import make_aerodrome_v2_pool
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.seam import Bot


@pytest.fixture
def stable_pool():
    bot = Bot()
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
    bot = Bot()
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


class TestPoolCalcRouting:
    """Routing is construction-time WIRING, not a runtime branch: the pool's
    quoter binds exactly one exact-in leaf per stable flag, and
    `calculate_tokens_out_from_tokens_in` dispatches only through the bound
    seam. Asserted on the wiring itself plus an instance-level fake — no
    module-global patching.
    """

    def test_stable_pool_wires_the_stable_calc(self, stable_pool) -> None:
        assert stable_pool.swap_quoter._tokens_out_leaf is quoter_mod._leaf_tokens_out_stable

    def test_volatile_pool_wires_the_volatile_calc(self, volatile_pool) -> None:
        assert volatile_pool.swap_quoter._tokens_out_leaf is quoter_mod._leaf_tokens_out_volatile

    def test_calculate_out_dispatches_only_through_the_wired_calc(self, stable_pool) -> None:
        calls: list[object] = []

        def recording_calc(*, ctx: object) -> int:
            calls.append(ctx)
            return 123

        t0 = stable_pool._token0
        quoter = quoter_mod.AerodromeV2SwapQuoter(
            stable_pool,
            stable_pool._py_pool,
            stable=stable_pool._stable,
            fee=stable_pool._fee,
        )
        quoter._tokens_out_leaf = recording_calc
        out = quoter.exact_in(t0, 100_000_000)

        # The read consults ONLY the bound seam: the fake's return value comes
        # out, and exactly one request arrived.
        assert out == 123
        assert len(calls) == 1

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
