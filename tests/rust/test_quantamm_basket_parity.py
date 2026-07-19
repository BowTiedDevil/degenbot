"""Rust-vs-Python parity test for the QuantAMM basket solver.

Cross-checks `degenbot._ffi.solve_balancer_weighted_basket` (the Rust core
closed-form solver, ported from `balancer_weighted.py`) against the Python
`BalancerMultiTokenSolver` on the doctest fixture (3-token WETH/USDC/DAI
50/25/25 weighted pool).

The closed-form solution (Willetts & Harrington, QuantAMM Equation 9) is
inherently f64 (rational exponents), so both solvers compute in f64 and
convert to integers at the end. Minor f64 noise (±3 wei per trade) is
expected — the parity gate uses a relative tolerance on `optimal_input`
(total deposit value) and `profit`.
"""

from __future__ import annotations

from fractions import Fraction

import pytest

from degenbot._ffi import solve_balancer_weighted_basket
from degenbot.arbitrage.solvers import BalancerMultiTokenSolver, SolveInput
from degenbot.types.hop_types import BalancerMultiTokenHop


def _doctest_fixture() -> BalancerMultiTokenHop:
    """The 3-token WETH/USDC/DAI 50/25/25 weighted pool from the
    `BalancerMultiTokenSolver` doctest."""
    return BalancerMultiTokenHop(
        reserves=(100e18, 2e12, 1e12),  # WETH, USDC, DAI in wei
        weights=(5e17, 25e16, 25e16),  # 50%, 25%, 25%
        fee=Fraction(3, 1000),
        market_prices=(2000.0, 1.0, 1.0),  # In USD
    )


def _rust_solve(hop: BalancerMultiTokenHop) -> tuple[int, int, int]:
    """Call the Rust solver and return (optimal_input, profit, iterations)."""
    trades, profit, _success, _signature, iterations = solve_balancer_weighted_basket(
        reserves=[int(r) for r in hop.reserves],
        weights=[int(w) for w in hop.weights],
        fee_numer=hop.fee.numerator,
        fee_denom=hop.fee.denominator,
        decimals=list(hop.decimals),
        market_prices=list(hop.market_prices or ()),
        max_input=None,
    )
    total_deposit = sum(max(0, t) * hop.market_prices[i] for i, t in enumerate(trades))
    return int(total_deposit), int(profit), iterations


def _python_solve(hop: BalancerMultiTokenHop) -> tuple[int, int, int]:
    """Call the Python solver and return (optimal_input, profit, iterations)."""
    solver = BalancerMultiTokenSolver()
    result = solver.solve(SolveInput(hops=(hop,)))
    return result.optimal_input, result.profit, result.iterations


class TestQuantAMMBasketParity:
    """Rust core matches Python `BalancerMultiTokenSolver`."""

    def test_optimal_input_matches_within_tolerance(self) -> None:
        hop = _doctest_fixture()
        rust_input, _rust_profit, _ = _rust_solve(hop)
        py_input, _py_profit, _ = _python_solve(hop)
        rel_err = abs(rust_input - py_input) / max(abs(py_input), 1)
        assert rel_err < 1e-6, (
            f"optimal_input: rust={rust_input}, python={py_input}, rel_err={rel_err}"
        )

    def test_profit_matches_within_tolerance(self) -> None:
        hop = _doctest_fixture()
        _rust_input, rust_profit, _ = _rust_solve(hop)
        _py_input, py_profit, _ = _python_solve(hop)
        rel_err = abs(rust_profit - py_profit) / max(abs(py_profit), 1)
        assert rel_err < 1e-6, (
            f"profit: rust={rust_profit}, python={py_profit}, rel_err={rel_err}"
        )

    def test_iterations_match(self) -> None:
        """Both solvers evaluate all N=3 signatures (12)."""
        hop = _doctest_fixture()
        _, _, rust_iters = _rust_solve(hop)
        _, _, py_iters = _python_solve(hop)
        assert rust_iters == py_iters == 12

    def test_rust_finds_profitable_trade(self) -> None:
        """The Rust solver should find a profitable basket trade (success=True)
        on the mispriced fixture."""
        hop = _doctest_fixture()
        trades, profit, success, signature, _ = solve_balancer_weighted_basket(
            reserves=[int(r) for r in hop.reserves],
            weights=[int(w) for w in hop.weights],
            fee_numer=hop.fee.numerator,
            fee_denom=hop.fee.denominator,
            decimals=list(hop.decimals),
            market_prices=list(hop.market_prices or ()),
            max_input=None,
        )
        assert success, "Rust solver should find a profitable trade"
        assert profit > 0, "profit should be positive"
        # Signature: withdraw the overpriced token (WETH), deposit the underpriced
        # tokens (USDC, DAI) — matches the Python oracle.
        assert -1 in signature, "should have a withdrawal"
        assert 1 in signature, "should have a deposit"
        assert any(t < 0 for t in trades), "should have a negative trade (withdrawal)"

    def test_profit_magnitude_is_reasonable(self) -> None:
        """The profit should be approximately 2e23 (the Python oracle value)
        — sanity check the closed-form produces a sensible magnitude."""
        hop = _doctest_fixture()
        _, rust_profit, _ = _rust_solve(hop)
        _, py_profit, _ = _python_solve(hop)
        # Both should be in the 1e23 range
        assert 1e22 < rust_profit < 1e24, f"rust profit {rust_profit} out of expected range"
        assert 1e22 < py_profit < 1e24, f"python profit {py_profit} out of expected range"
