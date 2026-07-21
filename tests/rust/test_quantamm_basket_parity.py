"""Tier-2 FFI-seam gate for the QuantAMM basket solver pyfunction (ADR-005).

Slimmed from the legacy shell-wiring parity test: the Python
`BalancerMultiTokenSolver` delegating shell + its `Solver` ABC plumbing +
the f64 `hop_types` taxonomy were retired (audit `6C32UV` →
`docs/migration-guides/parity-oracle-retirement-cutover.md`). What survives is
the ADR-005 Tier-2 dual-driver assertion on the `solve_balancer_weighted_basket`
pyfunction itself — driving the Rust core's closed-form N-token Balancer
weighted-basket solver (QuantAMM Equation 9, Willetts & Harrington 2024)
directly through the PyO3 boundary on its doctest fixture.

Why this file stays (and the shell half went): the pyfunction is a
Python-reachable Rust-core surface, not a private helper of the deleted shell.
Retiring it would strand the basket solver behind a Python wall and delete the
only FFI seam. The Rust `#[cfg(test)]` corpus in
`rust/crates/degenbot-solvers/src/basket.rs` is the math regression set; this
module is the arg-marshalling + result-shaping seam.

Fixture: 3-token WETH/USDC/DAI 50/25/25 weighted pool (the QuantAMM doctest).
"""

from __future__ import annotations

from fractions import Fraction

from degenbot._ffi import solve_balancer_weighted_basket

# ---------------------------------------------------------------------------
# Doctest fixture — kept byte-equivalent to the retired BalancerMultiTokenHop
# field values (reserves/weights/fee/decimals/market_prices).
# ---------------------------------------------------------------------------

# WETH/USDC/DAI reserves in wei; weights at 50%/25%/25%; 0.3% swap fee.
# `decimals` left empty to match the retired `BalancerMultiTokenHop` default
# (`decimals: tuple[int, ...] = ()`); the Rust core defaults to 18-decimal
# scaling, which yields the ~2e23 profit magnitude the doctest records.
_FIXTURE_RESERVES = (100_000_000_000_000_000_000, 2_000_000_000_000, 1_000_000_000_000)
_FIXTURE_WEIGHTS = (500_000_000_000_000_000, 250_000_000_000_000_000, 250_000_000_000_000_000)
_FIXTURE_FEE = Fraction(3, 1000)
_FIXTURE_DECIMALS: tuple[int, ...] = ()
_FIXTURE_PRICES = (2000.0, 1.0, 1.0)


def _solve() -> tuple[int, float, bool, list[int], int]:
    """Drive the Rust core through the pyfunction and return
    (total_deposit, profit, success, signature, iterations)."""
    trades, profit, success, signature, iterations = solve_balancer_weighted_basket(
        reserves=list(_FIXTURE_RESERVES),
        weights=list(_FIXTURE_WEIGHTS),
        fee_numer=_FIXTURE_FEE.numerator,
        fee_denom=_FIXTURE_FEE.denominator,
        decimals=list(_FIXTURE_DECIMALS),
        market_prices=list(_FIXTURE_PRICES),
        max_input=None,
    )
    total_deposit = sum(max(0, t) * _FIXTURE_PRICES[i] for i, t in enumerate(trades))
    return int(total_deposit), profit, success, signature, iterations


class TestQuantAMMBasketPyfunctionSeam:
    """The `solve_balancer_weighted_basket` pyfunction's arg-marshalling +
    result-shaping contract (ADR-005 Tier-2 FFI seam)."""

    def test_finds_profitable_trade(self) -> None:
        """The Rust solver finds a profitable basket trade on the mispriced
        fixture (success=True, positive profit, mixed signature)."""
        _trades, profit, success, signature, _ = _solve()
        assert success, "Rust solver should find a profitable trade"
        assert profit > 0, "profit should be positive"
        # Signature: withdraw the overpriced token (WETH), deposit the
        # underpriced tokens (USDC, DAI).
        assert -1 in signature, "should have a withdrawal"
        assert 1 in signature, "should have a deposit"

    def test_profit_magnitude_is_reasonable(self) -> None:
        """The profit (in token units) should be approximately 2e23 — sanity
        check the closed-form produces a sensible magnitude."""
        _deposit, profit, _success, _sig, _iters = _solve()
        assert 1e22 < profit < 1e24, f"profit {profit} out of expected range"

    def test_total_deposit_is_reasonable(self) -> None:
        """The optimal total deposit (in USD-priced terms) should land near
        7.5e17 on the mispriced fixture (closed-form sanity check)."""
        deposit, _profit, success, _sig, _iters = _solve()
        assert success, "solver should converge on the fixture"
        assert deposit > 0, f"total_deposit {deposit} should be positive"
        assert 1e16 < deposit < 1e18, f"total_deposit {deposit} out of expected range"

    def test_evaluates_all_n3_signatures(self) -> None:
        """Both paths evaluate all N=3 signatures (12)."""
        _deposit, _profit, _success, _sig, iters = _solve()
        assert iters == 12, f"expected 12 iterations for N=3, got {iters}"

    def test_empty_market_prices_finds_no_trade(self) -> None:
        """With no market prices, the Rust core cannot price a basket trade —
        success is False (no profitable trade found). This pins the
        pyfunction's behavior on the missing-prices contract that the deleted
        shell used to guard in Python."""
        _trades, _profit, success, _sig, _iters = solve_balancer_weighted_basket(
            reserves=list(_FIXTURE_RESERVES),
            weights=list(_FIXTURE_WEIGHTS),
            fee_numer=_FIXTURE_FEE.numerator,
            fee_denom=_FIXTURE_FEE.denominator,
            decimals=list(_FIXTURE_DECIMALS),
            market_prices=[],
            max_input=None,
        )
        assert success is False, "no market prices → no profitable trade"
