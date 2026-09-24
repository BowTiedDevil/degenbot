//! `QuantAMM` solver bindings (feature = "bot").
//!
//! Domain `#[pyfunction]` surface over `degenbot-solvers`; registration stays
//! centralized in `c_api::register`.

use pyo3::prelude::*;

/// `QuantAMM` closed-form N-token Balancer weighted basket arbitrage solver.
///
/// Thin `PyO3` wrapper over [`degenbot_solvers::basket::solve_balancer_weighted`].
/// Port of `BalancerMultiTokenSolver` / `solve_balancer_weighted` (Willetts &
/// Harrington, `QuantAMM` Equation 9).
///
/// # Arguments
///
/// - `reserves`: list of token reserves in wei.
/// - `weights`: list of normalized weights as 18-decimal fixed point (sum = 1e18).
/// - `fee_numer`, `fee_denom`: swap fee as the fraction `fee_numer / fee_denom`.
/// - `decimals`: list of decimal places per token; empty list = no scaling.
/// - `market_prices`: list of market prices per token (in numeraire).
/// - `max_input`: optional max total deposit value in numeraire units.
///
/// # Returns
///
/// `(trades, profit, success, signature, iterations)` — `trades` is a list of
/// native-token-integer amounts (positive = deposit, negative = withdraw).
///
/// # Errors
///
/// Returns `ValueError` if reserves and `market_prices` lengths don't match,
/// or if reserves/weights can't be converted to u128/u64.
#[expect(clippy::needless_pass_by_value, clippy::type_complexity)]
#[pyfunction]
#[pyo3(signature = (
    reserves, weights, fee_numer, fee_denom, decimals, market_prices, max_input=None
))]
pub fn solve_balancer_weighted_basket(
    reserves: Vec<u128>,
    weights: Vec<u64>,
    fee_numer: u64,
    fee_denom: u64,
    decimals: Vec<u8>,
    market_prices: Vec<f64>,
    max_input: Option<f64>,
) -> PyResult<(Vec<i128>, f64, bool, Vec<i8>, usize)> {
    let pool = degenbot_solvers::basket::BalancerMultiTokenState {
        reserves,
        weights,
        fee_numer,
        fee_denom,
        decimals,
    };
    let result =
        degenbot_solvers::basket::solve_balancer_weighted(&pool, &market_prices, max_input);
    Ok((
        result.trades,
        result.profit,
        result.success,
        result.signature,
        result.iterations,
    ))
}
