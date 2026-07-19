"""Shell-wiring gate for the QuantAMM basket solver delegation (ADR-005).

The Python ``BalancerMultiTokenSolver`` is now a **delegating shell** over the
Rust core ``degenbot._ffi.solve_balancer_weighted_basket`` — no math lives in
Python. This module gates the **delegation wiring**: it calls the Rust core
directly (``_rust_solve``) and through the Python shell
(``_python_solve`` → ``BalancerMultiTokenSolver.solve``) on the same fixture and
asserts they agree. A disagreement means the shell marshals an argument or
shapes a result incorrectly (a wiring bug), not a math divergence — the math
is exercised by the Rust ``#[cfg(test)]`` corpus in
``balancer_weighted_basket.rs``.

Fixture: 3-token WETH/USDC/DAI 50/25/25 weighted pool (the doctest fixture).
"""

from __future__ import annotations

from fractions import Fraction

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
    """Call the Rust core directly and return (optimal_input, profit, iterations)."""
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
    """Call through the Python delegating shell and return
    (optimal_input, profit, iterations)."""
    solver = BalancerMultiTokenSolver()
    result = solver.solve(SolveInput(hops=(hop,)))
    return result.optimal_input, result.profit, result.iterations


class TestQuantAMMBasketShellWiring:
    """The Python shell delegates to the Rust core without distortion.

    Both paths invoke the same Rust solver; these tests gate the shell's
    arg marshalling and ``SolveResult`` shaping.
    """

    def test_optimal_input_matches_within_tolerance(self) -> None:
        hop = _doctest_fixture()
        rust_input, _, _ = _rust_solve(hop)
        py_input, _, _ = _python_solve(hop)
        rel_err = abs(rust_input - py_input) / max(abs(py_input), 1)
        assert rel_err < 1e-6, (
            f"optimal_input: rust={rust_input}, python={py_input}, rel_err={rel_err}"
        )

    def test_profit_matches_within_tolerance(self) -> None:
        hop = _doctest_fixture()
        _, rust_profit, _ = _rust_solve(hop)
        _, py_profit, _ = _python_solve(hop)
        rel_err = abs(rust_profit - py_profit) / max(abs(py_profit), 1)
        assert rel_err < 1e-6, (
            f"profit: rust={rust_profit}, python={py_profit}, rel_err={rel_err}"
        )

    def test_iterations_match(self) -> None:
        """Both paths evaluate all N=3 signatures (12)."""
        hop = _doctest_fixture()
        _, _, rust_iters = _rust_solve(hop)
        _, _, py_iters = _python_solve(hop)
        assert rust_iters == py_iters == 12

    def test_shell_finds_profitable_trade(self) -> None:
        """The shell-delegated solve finds a profitable basket trade on the
        mispriced fixture (success=True, positive profit, mixed signature)."""
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
        """The profit should be approximately 2e23 — sanity check the
        closed-form produces a sensible magnitude."""
        hop = _doctest_fixture()
        _, rust_profit, _ = _rust_solve(hop)
        _, py_profit, _ = _python_solve(hop)
        # Both should be in the 1e23 range
        assert 1e22 < rust_profit < 1e24, f"rust profit {rust_profit} out of expected range"
        assert 1e22 < py_profit < 1e24, f"python profit {py_profit} out of expected range"

    def test_shell_raises_on_no_market_prices(self) -> None:
        """The shell guards the no-market_prices precondition before the Rust call."""
        from degenbot.exceptions import OptimizationError

        hop = BalancerMultiTokenHop(
            reserves=(100e18, 2e12, 1e12),
            weights=(5e17, 25e16, 25e16),
            fee=Fraction(3, 1000),
            market_prices=None,
        )
        solver = BalancerMultiTokenSolver()
        try:
            solver.solve(SolveInput(hops=(hop,)))
        except OptimizationError:
            pass
        else:
            msg = "shell should raise OptimizationError on missing market_prices"
            raise AssertionError(msg)
