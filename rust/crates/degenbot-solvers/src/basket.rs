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
// Constants
// ---------------------------------------------------------------------------

/// Minimum number of active tokens (deposit + withdraw) for a valid trade.
const MIN_ACTIVE_TOKENS: usize = 2;

/// Relative tolerance for the invariant check in [`validate_trade`].
const INVARIANT_TOLERANCE: f64 = 1e-6;

/// Search radius for the integer brute-force in [`refine_to_integer`].
const SEARCH_RADIUS: i64 = 3;

/// Maximum combinations before [`refine_to_integer`] falls back to no-search.
const MAX_COMBINATIONS: usize = 1000;

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
    #[expect(clippy::cast_precision_loss)] // f64 is the paper's derivation space
    fn upscaled_reserves(&self) -> Vec<f64> {
        let factors = self.scaling_factors();
        self.reserves
            .iter()
            .zip(factors.iter())
            .map(|(r, f)| (*r as f64) * (*f as f64))
            .collect()
    }

    /// Descale a trade from 18-decimal units back to native token units.
    #[expect(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
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
// Signature generation
// ---------------------------------------------------------------------------

/// Generate all valid trade signatures for an N-token pool.
///
/// A signature is a `Vec<i8>` of `{-1, 0, 1}` values (per token: -1=withdraw,
/// 0=no trade, 1=deposit). Valid signatures must contain at least one
/// deposit (1) and at least one withdraw (-1).
///
/// N=3 → 12 signatures, N=4 → 50, N=5 → 180.
fn generate_trade_signatures(n: usize) -> Vec<Vec<i8>> {
    let total = 3u64.pow(u32::try_from(n).unwrap_or(u32::MAX));
    let mut result = Vec::new();
    for code in 0..total {
        let mut sig = Vec::with_capacity(n);
        let mut c = code;
        let mut has_deposit = false;
        let mut has_withdraw = false;
        for _ in 0..n {
            let val = match c % 3 {
                0 => 0,
                1 => 1,
                _ => -1,
            };
            if val == 1 {
                has_deposit = true;
            } else if val == -1 {
                has_withdraw = true;
            }
            sig.push(val);
            c /= 3;
        }
        if has_deposit && has_withdraw {
            // Reverse to match Python's itertools.product iteration order
            // (product generates the rightmost element changing fastest;
            // our code generates the leftmost element changing fastest, so
            // reversing restores Python's order).
            sig.reverse();
            result.push(sig);
        }
    }
    result
}

/// Compute `d_i = I_{s_i=1}` per the paper's definition.
///
/// `d_i = 1` if depositing (signature[i] == 1), 0 otherwise.
fn compute_d(signature: &[i8]) -> Vec<i32> {
    signature.iter().map(|&s| i32::from(s == 1)).collect()
}

// ---------------------------------------------------------------------------
// Closed-form solution (Equation 9)
// ---------------------------------------------------------------------------

/// Compute optimal trade amounts for a given signature using Equation 9.
///
/// All reserves are internally upscaled to 18-decimal before applying
/// the formula. The resulting trades are in the upscaled 18-decimal space
/// and must be descaled to native token units for on-chain use.
#[expect(clippy::cast_precision_loss)] // weights are 18-dp fixed-point → f64
fn compute_optimal_trade(
    pool: &BalancerMultiTokenState,
    market_prices: &[f64],
    signature: &[i8],
) -> Vec<f64> {
    let n = pool.n_tokens();
    let gamma = 1.0 - (pool.fee_numer as f64) / (pool.fee_denom as f64);

    let d = compute_d(signature);

    let active_indices: Vec<usize> = (0..n).filter(|&i| signature[i] != 0).collect();

    if active_indices.len() < MIN_ACTIVE_TOKENS {
        return vec![0.0; n];
    }

    let reserves_f = pool.upscaled_reserves();

    // Active-token normalized weights: sum to 1.0
    let active_weight_sum: f64 = active_indices.iter().map(|&i| pool.weights[i] as f64).sum();
    let w_tilde: Vec<f64> = (0..n)
        .map(|i| {
            if signature[i] != 0 {
                pool.weights[i] as f64 / active_weight_sum
            } else {
                0.0
            }
        })
        .collect();

    // k_tilde = prod(R_i^w_tilde_i) for active tokens (upscaled)
    let mut k_tilde = 1.0_f64;
    for &i in &active_indices {
        k_tilde *= reserves_f[i].powf(w_tilde[i]);
    }

    // Compute Phi_i for each active token (in upscaled 18-decimal units)
    let mut trades = vec![0.0_f64; n];
    for &i in &active_indices {
        let gamma_i = gamma.powi(d[i]);
        let term_i = (w_tilde[i] * gamma_i / market_prices[i]).powf(1.0 - w_tilde[i]);

        // product_j = prod_{j!=i, j in A} (m_p,j / (w_tilde_j * gamma^(d_j))) ^ w_tilde_j
        let mut product_j = 1.0_f64;
        for &j in &active_indices {
            if j == i {
                continue;
            }
            let gamma_j = gamma.powi(d[j]);
            let denom_j = w_tilde[j] * gamma_j;
            product_j *= (market_prices[j] / denom_j).powf(w_tilde[j]);
        }

        let bracket = k_tilde * term_i * product_j - reserves_f[i];

        // Phi_i = gamma^(-d_i) * bracket (in upscaled 18-decimal units)
        trades[i] = gamma.powi(-d[i]) * bracket;
    }

    trades
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate that the trade:
/// 1. Respects the signature (direction matches)
/// 2. Doesn't withdraw more than available
/// 3. Maintains the invariant with fees.
///
/// All calculations use upscaled 18-decimal reserves.
#[expect(clippy::cast_precision_loss)]
fn validate_trade(trades: &[f64], signature: &[i8], pool: &BalancerMultiTokenState) -> bool {
    let gamma = 1.0 - (pool.fee_numer as f64) / (pool.fee_denom as f64);
    let reserves_f = pool.upscaled_reserves();

    for (i, (&trade, &sig)) in trades.iter().zip(signature.iter()).enumerate() {
        if sig == 1 && trade < 0.0 {
            return false;
        }
        if sig == -1 && trade > 0.0 {
            return false;
        }
        if trade < 0.0 && trade.abs() >= reserves_f[i] {
            return false;
        }
    }

    // Invariant check using upscaled reserves
    let active_indices: Vec<usize> = (0..trades.len()).filter(|&i| signature[i] != 0).collect();
    let active_weight_sum: f64 = active_indices.iter().map(|&i| pool.weights[i] as f64).sum();

    let mut product = 1.0_f64;
    for &i in &active_indices {
        let w_tilde = pool.weights[i] as f64 / active_weight_sum;
        let effective = if signature[i] == 1 {
            gamma * trades[i]
        } else {
            trades[i]
        };
        let new_reserve = reserves_f[i] + effective;
        if new_reserve <= 0.0 {
            return false;
        }
        product *= new_reserve.powf(w_tilde);
    }

    let mut k_tilde = 1.0_f64;
    for &i in &active_indices {
        let w_tilde = pool.weights[i] as f64 / active_weight_sum;
        k_tilde *= reserves_f[i].powf(w_tilde);
    }

    if k_tilde == 0.0 {
        return false;
    }
    let relative_error = (product - k_tilde).abs() / k_tilde;
    relative_error < INVARIANT_TOLERANCE
}

// ---------------------------------------------------------------------------
// Profit
// ---------------------------------------------------------------------------

/// Compute profit from upscaled 18-decimal trades at market prices.
///
/// Converts upscaled 18-decimal trades to token units before
/// multiplying by market prices, ensuring dimensional consistency.
///
/// `Profit = -sum(market_price_i * Phi_i_in_tokens)`
fn compute_profit_token_units(trades: &[f64], market_prices: &[f64]) -> f64 {
    let mut total = 0.0_f64;
    for i in 0..trades.len() {
        let token_amount = trades[i] / 1e18;
        total += market_prices[i] * token_amount;
    }
    -total
}

// ---------------------------------------------------------------------------
// Integer refinement
// ---------------------------------------------------------------------------

/// Refine float trades to integer amounts in native token units.
///
/// Trades are in upscaled 18-decimal units. We descale to native
/// units (accounting for each token's decimals), round, and search
/// ±[`SEARCH_RADIUS`] candidates around the base for the max-profit
/// integer combination.
fn refine_to_integer(
    trades: &[f64],
    signature: &[i8],
    pool: &BalancerMultiTokenState,
    market_prices: &[f64],
) -> Vec<i128> {
    let n = pool.n_tokens();
    let active_indices: Vec<usize> = (0..n).filter(|&i| signature[i] != 0).collect();

    if active_indices.len() < MIN_ACTIVE_TOKENS {
        return vec![0; n];
    }

    // Descale trades from upscaled 18-decimal to native token units
    let mut int_trades: Vec<i128> = (0..n).map(|i| pool.descale_trade(trades[i], i)).collect();

    // Direction enforcement
    for &i in &active_indices {
        if signature[i] == 1 && int_trades[i] < 0 {
            int_trades[i] = 0;
        }
        if signature[i] == -1 && int_trades[i] > 0 {
            int_trades[i] = 0;
        }
    }

    let mut best_trades = int_trades.clone();
    let mut best_profit = f64::NEG_INFINITY;

    // Build search ranges for active tokens
    let search_ranges: Vec<Vec<i128>> = active_indices
        .iter()
        .map(|&i| {
            let base = int_trades[i];
            ((base - i128::from(SEARCH_RADIUS))..=(base + i128::from(SEARCH_RADIUS))).collect()
        })
        .collect();

    // Count combinations
    let combination_count: usize = search_ranges.iter().map(Vec::len).product();
    if combination_count <= MAX_COMBINATIONS {
        // Brute-force product search
        let mut indices = vec![0usize; active_indices.len()];
        loop {
            let mut candidate = int_trades.clone();
            for (k, &i) in active_indices.iter().enumerate() {
                candidate[i] = search_ranges[k][indices[k]];
            }

            let valid = active_indices.iter().all(|&i| {
                if signature[i] == 1 && candidate[i] < 0 {
                    return false;
                }
                if signature[i] == -1 && candidate[i] > 0 {
                    return false;
                }
                if candidate[i] < 0 && candidate[i].unsigned_abs() >= pool.reserves[i] {
                    return false;
                }
                true
            });

            if valid {
                let profit = compute_int_profit(pool, market_prices, &candidate, &active_indices);
                if profit > best_profit {
                    best_profit = profit;
                    best_trades = candidate;
                }
            }

            // Increment indices (odometer)
            let mut carry = true;
            for k in 0..active_indices.len() {
                if carry {
                    indices[k] += 1;
                    if indices[k] < search_ranges[k].len() {
                        carry = false;
                    } else {
                        indices[k] = 0;
                        carry = true;
                    }
                }
            }
            if carry {
                break; // all combinations exhausted
            }
        }
    } else {
        best_trades = int_trades;
    }

    best_trades
}

/// Compute profit for native integer trades:
/// `-sum(market_prices[i] * candidate[i] / 10^decimals[i])` when decimals are
/// present, or `-sum(market_prices[i] * candidate[i])` when empty.
#[expect(clippy::cast_precision_loss)]
fn compute_int_profit(
    pool: &BalancerMultiTokenState,
    market_prices: &[f64],
    candidate: &[i128],
    active_indices: &[usize],
) -> f64 {
    let mut total = 0.0_f64;
    for &i in active_indices {
        let token_amount = if pool.decimals.is_empty() {
            candidate[i] as f64
        } else {
            candidate[i] as f64 / 10f64.powi(i32::from(pool.decimals[i]))
        };
        total += market_prices[i] * token_amount;
    }
    -total
}

// ---------------------------------------------------------------------------
// Main solver
// ---------------------------------------------------------------------------

/// Find optimal multi-token arbitrage trade for a Balancer weighted pool.
///
/// Uses Equation 9 from Willetts & Harrington (2024) with correct
/// `d_i = I_{s_i=1}` indicator (1 for deposit, 0 for withdraw).
///
/// Iterates over all valid trade signatures, computing the closed-form
/// optimum for each, validating, and selecting the highest-profit
/// integer-refined result.
#[must_use]
pub fn solve_balancer_weighted(
    pool: &BalancerMultiTokenState,
    market_prices: &[f64],
    max_input: Option<f64>,
) -> MultiTokenArbitrageResult {
    let n = pool.n_tokens();

    if n != market_prices.len() {
        return MultiTokenArbitrageResult {
            trades: vec![0; n],
            profit: 0.0,
            success: false,
            signature: vec![0; n],
            iterations: 0,
        };
    }

    let signatures = generate_trade_signatures(n);

    let mut best_result: Option<MultiTokenArbitrageResult> = None;
    let mut best_profit = 0.0_f64;

    for signature in &signatures {
        let trades = compute_optimal_trade(pool, market_prices, signature);

        if !validate_trade(&trades, signature, pool) {
            continue;
        }

        let profit = compute_profit_token_units(&trades, market_prices);

        if let Some(max_in) = max_input {
            let total_input: f64 = (0..n)
                .map(|i| market_prices[i] * trades[i].max(0.0) / 1e18)
                .sum();
            if total_input > max_in {
                continue;
            }
        }

        if profit > best_profit {
            best_profit = profit;
            let int_trades = refine_to_integer(&trades, signature, pool, market_prices);
            let active_indices: Vec<usize> = (0..n).filter(|&i| signature[i] != 0).collect();
            let int_profit = compute_int_profit(pool, market_prices, &int_trades, &active_indices);

            if int_profit > 0.0 {
                let sig_vec = signature.clone();
                best_result = Some(MultiTokenArbitrageResult {
                    trades: int_trades,
                    profit: int_profit,
                    success: true,
                    signature: sig_vec,
                    iterations: signatures.len(),
                });
            }
        }
    }

    best_result.unwrap_or(MultiTokenArbitrageResult {
        trades: vec![0; n],
        profit: 0.0,
        success: false,
        signature: vec![0; n],
        iterations: signatures.len(),
    })
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
    #[expect(clippy::cast_precision_loss)]
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

    #[test]
    fn generate_signatures_n3_has_12_valid() {
        let sigs = generate_trade_signatures(3);
        assert_eq!(sigs.len(), 12, "N=3 should have 12 valid signatures");
        // Each must have at least one 1 and one -1
        for s in &sigs {
            assert!(s.contains(&1), "must have a deposit");
            assert!(s.contains(&-1), "must have a withdraw");
        }
    }

    #[test]
    fn generate_signatures_n4_has_50_valid() {
        let sigs = generate_trade_signatures(4);
        assert_eq!(sigs.len(), 50, "N=4 should have 50 valid signatures");
    }
}
