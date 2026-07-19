//! `QuantAMM` closed-form N-token Balancer weighted basket arbitrage solver.
//!
//! Faithful Rust port of `src/degenbot/arbitrage/solvers/balancer_weighted.py`
//! implementing Equation 9 from Willetts & Harrington, "Closed-form solutions
//! for generic N-token AMM arbitrage" (QuantAMM.fi, Feb 2024).
//!
//! # Why f64
//!
//! The closed-form solution involves `R^w̃` where `w̃` is a rational
//! (renormalized) weight — `(R^p)^(1/q)` has no exact integer form in
//! general. The paper's derivation is **inherently floating-point**, so the
//! core computation runs entirely in `f64`. Integer trades are recovered at
//! the end via [`refine_to_integer`] (descale + ±3 brute-force), matching the
//! Python reference exactly.
//!
//! # Standalone entry point
//!
//! This is NOT a `solve_path` arm — it does not compose into the cyclic
//! hop-list dispatch. It is a separate entry point for N-token basket
//! deposit/withdraw optimization (multiple tokens in/out simultaneously),
//! giving the engine feature parity with Python's `BalancerMultiTokenSolver`.

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// State for an N-token Balancer weighted pool basket arbitrage.
///
/// Faithful Rust port of `BalancerMultiTokenState`
/// (`src/degenbot/arbitrage/solvers/balancer_weighted.py`).
///
/// # Fields
///
/// - `reserves`: token reserves in wei.
/// - `weights`: normalized weights as 18-decimal fixed point (sum = 1e18).
/// - `fee_numer` / `fee_denom`: swap fee as the exact fraction `fee_numer / fee_denom`.
/// - `decimals`: decimal places per token; empty = no scaling (all treated
///   as 18-decimal internally — Balancer Vault convention).
#[derive(Clone, Debug)]
pub struct BalancerMultiTokenState {
    /// Token reserves in wei.
    pub reserves: Vec<u128>,
    /// Normalized weights as 18-decimal fixed point (sum = 1e18).
    pub weights: Vec<u64>,
    /// Fee numerator. `fee = fee_numer / fee_denom`.
    pub fee_numer: u64,
    /// Fee denominator.
    pub fee_denom: u64,
    /// Decimal places per token; empty = no scaling.
    pub decimals: Vec<u8>,
}

#[allow(dead_code)] // used by T2 (full impl) — stub returns failure in RED phase
impl BalancerMultiTokenState {
    /// Number of tokens.
    #[must_use]
    pub fn n_tokens(&self) -> usize {
        self.reserves.len()
    }

    /// Compute scaling factors to upscale reserves to 18-decimal.
    /// Empty `decimals` → all ones (no upscaling needed).
    fn scaling_factors(&self) -> Vec<u128> {
        if self.decimals.is_empty() {
            vec![1; self.n_tokens()]
        } else {
            self.decimals
                .iter()
                .map(|&d| 10u128.pow(u32::from(18u8.saturating_sub(d))))
                .collect()
        }
    }

    /// Reserves upscaled to 18-decimal (Balancer Vault convention).
    #[allow(clippy::cast_precision_loss)] // f64 is the paper's derivation space
    fn upscaled_reserves(&self) -> Vec<f64> {
        let factors = self.scaling_factors();
        self.reserves
            .iter()
            .zip(factors.iter())
            .map(|(r, f)| (*r as f64) * (*f as f64))
            .collect()
    }

    /// Descale a trade from 18-decimal units back to native token units.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    fn descale_trade(&self, trade: f64, token_index: usize) -> i128 {
        if self.decimals.is_empty() {
            return trade.round() as i128;
        }
        let factor = 10u128.pow(u32::from(18u8.saturating_sub(self.decimals[token_index])));
        (trade / factor as f64).round() as i128
    }
}

/// Result of multi-token basket arbitrage optimization.
///
/// Rust port of `MultiTokenArbitrageResult`.
#[derive(Clone, Debug, PartialEq)]
pub struct MultiTokenArbitrageResult {
    /// Optimal trade amounts (positive = deposit, negative = withdraw),
    /// in native token units.
    pub trades: Vec<i128>,
    /// Expected profit in numéraire units (f64).
    pub profit: f64,
    /// Whether a profitable trade was found.
    pub success: bool,
    /// Trade signature that produced this result: -1=withdraw, 0=no trade, 1=deposit.
    pub signature: Vec<i8>,
    /// Number of signatures evaluated.
    pub iterations: usize,
}

// ---------------------------------------------------------------------------
// Solver
// ---------------------------------------------------------------------------

/// Find optimal multi-token arbitrage trade for a Balancer weighted pool.
///
/// Uses Equation 9 from Willetts & Harrington (2024). Iterates over all
/// valid trade signatures, computing the closed-form optimum for each,
/// validating, and selecting the highest-profit integer-refined result.
///
/// Stub implementation — returns a failure result. TDD RED phase.
#[must_use]
pub fn solve_balancer_weighted(
    pool: &BalancerMultiTokenState,
    _market_prices: &[f64],
    _max_input: Option<f64>,
) -> MultiTokenArbitrageResult {
    let n = pool.n_tokens();
    MultiTokenArbitrageResult {
        trades: vec![0; n],
        profit: 0.0,
        success: false,
        signature: vec![0; n],
        iterations: 0,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Oracle values from Python `solve_balancer_weighted` with `decimals=()`:
    /// reserves=(100e18, 2e12, 1e12), weights=(5e17, 2.5e17, 2.5e17),
    /// `fee=3/1000`, `market_prices=(2000.0, 1.0, 1.0)`.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    #[test]
    fn tdd_red_three_token_basket_matches_python_oracle() {
        let pool = BalancerMultiTokenState {
            reserves: vec![
                100_000_000_000_000_000_000u128,
                2_000_000_000_000u128,
                1_000_000_000_000u128,
            ],
            weights: vec![
                500_000_000_000_000_000u64,
                250_000_000_000_000_000u64,
                250_000_000_000_000_000u64,
            ],
            fee_numer: 3,
            fee_denom: 1000,
            decimals: vec![],
        };
        let market_prices = [2000.0_f64, 1.0, 1.0];

        let result = solve_balancer_weighted(&pool, &market_prices, None);

        // RED: stub returns success=false, this will fail.
        assert!(result.success, "should find a profitable basket trade");
        assert_eq!(
            result.signature,
            vec![-1_i8, 1, 1],
            "signature should be withdraw-WETH, deposit-USDC, deposit-DAI"
        );
        assert_eq!(
            result.iterations, 12,
            "should evaluate 12 signatures for N=3"
        );

        // Profit ≈ 2.0e23 — relative tolerance for f64 noise.
        let expected_profit = 1.999_984_935_003e23_f64;
        let rel_err = (result.profit - expected_profit).abs() / expected_profit;
        assert!(
            rel_err < 1e-6,
            "profit {} should match oracle {expected_profit} within 1e-6 (rel_err={rel_err})",
            result.profit
        );

        // Trades — relative tolerance (f64 → int noise).
        let expected_trades = [
            -99_999_623_374_327_840_768_i128,
            376_623_666_139_452_608,
            376_624_669_148_479_680,
        ];
        for (i, (&got, &exp)) in result.trades.iter().zip(expected_trades.iter()).enumerate() {
            let rel_err = if exp != 0 {
                ((got - exp) as f64).abs() / (exp as f64).abs()
            } else {
                (got as f64).abs()
            };
            assert!(
                rel_err < 1e-6,
                "trade[{i}] = {got} should match oracle {exp} within 1e-6 (rel_err={rel_err})"
            );
        }
    }
}
