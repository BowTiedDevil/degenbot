"""Curve standard DyCalculator routing — delegation-detection + parity.

Ergo 6TLIJ5/7QMFWM: the ``StandardDyCalculator.calculate`` solve step routes
``stableswap_get_y`` through the ``degenbot-curve-math`` Rust leaf
(``degenbot_rs.curve_stableswap_get_y``) instead of the Python
``calculations/stableswap.py`` port. The variant-orchestration (rate/fee
conversion, balance sourcing) + the crypto/live_admin/metapool variants stay
Python (I/O-bound via ``CurveDataProvider`` + base-pool recursion — out of
scope per the leaf's pure-math boundary).

The leaf is byte-for-byte cross-checked vs the Python oracle by the frozen
``oracle_crosscheck.rs`` snapshot at the unit level; this module is the
orchestration-level gate — it spies on the Rust seam to prove the routed
solve hits it with the right arguments, and recomputes the solve via the
Python oracle (the still-live ``calculations/stableswap.stableswap_get_y``)
to prove byte-equivalence of the routed y.
"""

from __future__ import annotations

import pytest

from degenbot.calculations.stableswap import stableswap_get_y
from degenbot.curve.calculators import standard as standard_mod
from degenbot.curve.calculators.standard import StandardDyCalculator
from degenbot.curve.types import DVariant, DyCalculationInputs, SwapStyle, YVariant
from degenbot.degenbot_rs import curve_stableswap_get_y

PRECISION = 10**18
FEE_DENOMINATOR = 10**10


def _make_inputs() -> DyCalculationInputs:
    """A 2-token 18-decimal standard Curve pool with identity rates."""
    return DyCalculationInputs(
        PRECISION=PRECISION,
        FEE_DENOMINATOR=FEE_DENOMINATOR,
        fee=4 * 10**6,  # 0.04%
        n_coins=2,
        balances=(1 * 10**24, 2 * 10**24),
        rate_multipliers=(PRECISION, PRECISION),  # 18-dec → identity
        precision_multipliers=(1, 1),
        offpeg_fee_multiplier=FEE_DENOMINATOR,
        fee_gamma=10**10,
        mid_fee=4 * 10**6,
        out_fee=40 * 10**6,
        address="0x" + "11" * 20,
        resolved_rates=(PRECISION, PRECISION),  # identity rates
        xp=(1 * 10**24, 2 * 10**24),
        block_number=0,
        block_timestamp=0,
        amp=2000,
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
def standard_calc() -> StandardDyCalculator:
    return StandardDyCalculator(
        swap_style=SwapStyle.STANDARD,
    )  # RATE_ADJUSTED_XP / RESOLVED_RATES / FEE_THEN_RATE


@pytest.fixture
def get_y_spy(monkeypatch) -> _Spy:
    spy = _Spy(standard_mod.curve_stableswap_get_y)
    monkeypatch.setattr(standard_mod, "curve_stableswap_get_y", spy)
    return spy


class TestStandardDyRouting:
    def test_solve_routes_through_rust(self, standard_calc, get_y_spy) -> None:
        inputs = _make_inputs()
        dx = 100 * PRECISION  # swap 100 of token 0 in
        dy = standard_calc.calculate(0, 1, dx, inputs=inputs)

        # Delegation-detection: the Rust seam was hit exactly once.
        assert len(get_y_spy.calls) == 1
        args, _ = get_y_spy.calls[0]
        # Recorded args: (i, j, x, xp, amp, n_coins, a_precision, y_variant, d_variant)
        i, j, x, xp, amp, n_coins, a_precision, y_variant, d_variant = args
        assert (i, j, amp, n_coins, a_precision) == (
            0,
            1,
            inputs.amp,
            inputs.n_coins,
            inputs.a_precision,
        )
        assert y_variant == YVariant.STANDARD.value
        assert d_variant == DVariant.STANDARD.value

        # Parity: the routed y (recovered from the returned dy) matches the
        # Python oracle over the SAME solve inputs. Reverse the FEE_THEN_RATE
        # conversion to recover y: dy = (raw_dy - fee) * PRECISION // rates[j],
        # with raw_dy = xp[j] - y - 1, fee = fee * raw_dy // FEE_DENOMINATOR,
        # rates[j] = PRECISION (identity) → dy = raw_dy - fee.
        y_oracle = stableswap_get_y(
            i,
            j,
            x=x,
            xp=xp,
            amp=amp,
            n_coins=n_coins,
            a_precision=a_precision,
            y_variant=YVariant.STANDARD,
            d_variant=DVariant.STANDARD,
        )
        raw_dy = xp[j] - y_oracle - 1
        fee = inputs.fee * raw_dy // FEE_DENOMINATOR
        expected_dy = (raw_dy - fee) * PRECISION // PRECISION
        assert dy == expected_dy

        # And the routed y (the Rust fn over the recorded args) == the Python oracle y.
        assert curve_stableswap_get_y(*args) == y_oracle
