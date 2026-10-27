"""Consumer probe (iii) - the Python driver: complete one solve task through
the ``degenbot`` package's Rust-core solve surface (ADR-084 D1/C5).

The FFI seam here is ``solve_balancer_weighted_basket``
(``degenbot._ffi``; bridged through ``degenbot.arbitrage``), the QuantAMM
closed-form N-token Balancer weighted-basket solver - the Python-reachable
``degenbot_pools::solve`` entry. The catalog's C5 consumer (iii) completes the
documented task "solve a basket through the Python driver" with this probe.

Fixture: the same 3-token WETH/USDC/DAI 50/25/25 mispriced pool the basket
parity seam (``tests/balancer/test_quantamm_basket_parity.py``) pins, so the
probe asserts the door answers rather than re-deriving the math.
"""

from __future__ import annotations

from fractions import Fraction

from tests.helpers.seam import solve_balancer_weighted_basket

# Reserves in wei; weights at 50/25/25 (18-decimal fixed point); 0.3% fee;
# decimals empty = 18-decimal scaling; prices flag the mispricing
# (byte-equivalent to the basket parity seam's fixture).
_RESERVES = (100_000_000_000_000_000_000, 2_000_000_000_000, 1_000_000_000_000)
_WEIGHTS = (500_000_000_000_000_000, 250_000_000_000_000_000, 250_000_000_000_000_000)
_FEE = Fraction(3, 1000)
_PRICES = (2000.0, 1.0, 1.0)


def test_solve_surface_probe() -> None:
    """The Python driver completes a solve task through the FFI door."""
    trades, profit, success, signature, iterations = solve_balancer_weighted_basket(
        reserves=list(_RESERVES),
        weights=list(_WEIGHTS),
        fee_numer=_FEE.numerator,
        fee_denom=_FEE.denominator,
        decimals=[],
        market_prices=list(_PRICES),
        max_input=None,
    )
    assert success, "the Rust-core solver must answer through the FFI seam"
    assert profit > 0, f"expected a profitable basket solve, got profit={profit}"
    assert len(trades) == len(_RESERVES), "one trade per token"
    assert -1 in signature, "should withdraw the overpriced token"
    assert iterations >= 0
