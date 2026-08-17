"""Parity: the ``_ffi.v2_math`` V2 constant-product seam (RH3L24).

The Rust `degenbot-v2-math` primitives (`v2_swap_exact_in` /
`v2_swap_exact_out`, exposed as `calc_exact_in_v2` / `calc_exact_out_v2`) must
round **byte-identically** to the Python reference formulas in
`degenbot.uniswap.v2_functions` across random reserves/amounts/fees — that
identity is what keeps the Python V2-family calcs and the Rust solver's
arithmetic in lockstep. The companion re-exports
(`degenbot.uniswap.math` / `degenbot.aerodrome.math`) are pinned as identity
aliases of the FFI functions.
"""

from __future__ import annotations

import random
from fractions import Fraction

import pytest

from degenbot._ffi.v2_math import calc_exact_in_v2, calc_exact_out_v2
from degenbot.uniswap.v2_functions import (
    constant_product_calc_exact_in,
    constant_product_calc_exact_out,
)

_FEE_RATES: list[tuple[int, int]] = [
    (3, 1000),
    (5, 10000),
    (25, 10000),
    (30, 10000),
    (0, 1000),
]


@pytest.mark.parametrize("seed", range(25))
def test_calc_exact_in_v2_matches_python_formula(seed: int) -> None:
    """Randomized exact-in: Rust seam == Python formula (byte-identical)."""
    rng = random.Random(seed)
    r_in = rng.randrange(10**6, 2 * 10**16)
    r_out = rng.randrange(10**6, 2 * 10**16)
    amount_in = rng.randrange(1, 10**14)
    fee_numer, fee_denom = rng.choice(_FEE_RATES)

    py_result = constant_product_calc_exact_in(
        amount_in, r_in, r_out, Fraction(fee_numer, fee_denom)
    )
    rs_result = calc_exact_in_v2(r_in, r_out, amount_in, fee_numer, fee_denom)
    assert rs_result == py_result


@pytest.mark.parametrize("seed", range(25))
def test_calc_exact_out_v2_matches_python_formula(seed: int) -> None:
    """Randomized exact-out: Rust seam == Python formula (byte-identical)."""
    rng = random.Random(1000 + seed)
    r_in = rng.randrange(10**6, 2 * 10**16)
    r_out = rng.randrange(10**6, 2 * 10**16)
    amount_out = rng.randrange(1, max(2, r_out // 2))
    fee_numer, fee_denom = rng.choice(_FEE_RATES)

    py_result = constant_product_calc_exact_out(
        amount_out, r_in, r_out, Fraction(fee_numer, fee_denom)
    )
    rs_result = calc_exact_out_v2(r_in, r_out, amount_out, fee_numer, fee_denom)
    assert rs_result == py_result


def test_calc_exact_out_v2_rejects_overdraw() -> None:
    """`amount_out >= reserves_out` is undefined (the pool can't hold it)."""
    with pytest.raises(ValueError):
        calc_exact_out_v2(10**9, 10**9, 10**9, 3, 1000)
    with pytest.raises(ValueError):
        calc_exact_out_v2(10**9, 10**9, 10**9 + 1, 3, 1000)


def test_invalid_fee_raises() -> None:
    """`fee_numer > fee_denom` (>100% fee) is invalid — ValueError."""
    with pytest.raises(ValueError):
        calc_exact_in_v2(10**9, 10**9, 100, 5, 3)
    with pytest.raises(ValueError):
        calc_exact_out_v2(10**9, 10**9, 100, 5, 3)


def test_exact_in_out_inverse_relationship() -> None:
    """exact-out( exact-in(x) ) <= x and re-applying it recovers >= the output
    (the `+1` floor-division compensation)."""
    r_in, r_out, amt_in = 10**9, 2 * 10**9, 7_123_456
    out = calc_exact_in_v2(r_in, r_out, amt_in, 3, 1000)
    assert out > 0
    back_in = calc_exact_out_v2(r_in, r_out, out, 3, 1000)
    assert back_in <= amt_in
    assert calc_exact_in_v2(r_in, r_out, back_in, 3, 1000) >= out


def test_companion_reexports_are_identity_aliases() -> None:
    """The companion homes re-export the exact FFI functions (no wrappers)."""
    from degenbot.aerodrome.math import calc_exact_out_v2 as aero_out
    from degenbot.uniswap.math import (
        calc_exact_in_v2 as uni_in,
    )
    from degenbot.uniswap.math import (
        calc_exact_out_v2 as uni_out,
    )

    assert aero_out is calc_exact_out_v2
    assert uni_in is calc_exact_in_v2
    assert uni_out is calc_exact_out_v2


def test_full_fee_exact_in_degenerate() -> None:
    """A 100% fee (gamma = 0) exact-in yields 0 output (matches the Python
    formula); not an error."""
    assert calc_exact_in_v2(10**9, 10**9, 100, 1000, 1000) == 0
