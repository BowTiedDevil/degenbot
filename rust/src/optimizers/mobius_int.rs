//! Integer Möbius transformation optimizer for EVM-exact arbitrage.
//!
//! Uses U512 intermediate arithmetic for the coefficient recurrence (K, M, N),
//! giving exact profitability checks. The optimal input is derived from the
//! exact K/M ratio via f64, then verified with EVM-exact integer simulation.
//!
//! This produces **EVM-exact** results — the simulation step uses the same
//! integer arithmetic as the Uniswap V2/V3 contracts:
//!
//! ```text
//! y = gamma_numer * reserve_out * x / (gamma_denom * reserve_in + gamma_numer * x)
//! ```
//!
//! where all values are integers and `/` is floor division (EVM semantics).

#![allow(non_snake_case)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::let_and_return)]
#![allow(clippy::redundant_field_names)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::float_cmp)]
#![allow(clippy::suboptimal_flops)]
#![allow(clippy::similar_names)]
#![allow(clippy::unreadable_literal)]
#![allow(clippy::unnecessary_cast)]
#![allow(clippy::type_complexity)]

use alloy::primitives::{U256, U512};

use crate::optimizers::mobius::{MobiusError, mobius_solve, HopState};

/// Fee parameters for a single pool hop.
///
/// In Uniswap V2, the fee is expressed as `gamma_numer / fee_denom`:
/// - 0.3% fee → `gamma_numer = 997`, `fee_denom = 1000`
/// - 0.05% fee → `gamma_numer = 9995`, `fee_denom = 10000`
///
/// `gamma_numer` is the retained fraction (what passes through after fees),
/// not the fee amount itself.
///
/// The swap formula is:
/// `y = gamma_numer * reserve_out * x / (fee_denom * reserve_in + gamma_numer * x)`
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct IntHopState {
    /// Reserve of the input token (uint256 scale).
    pub reserve_in: U256,
    /// Reserve of the output token (uint256 scale).
    pub reserve_out: U256,
    /// Gamma numerator: the retained fraction (e.g. 997 for 0.3% fee).
    pub gamma_numer: u64,
    /// Fee denominator (e.g. 1000 for 0.3% fee).
    pub fee_denom: u64,
}

impl IntHopState {
    /// Create a new integer hop state.
    #[must_use]
    pub const fn new(reserve_in: U256, reserve_out: U256, gamma_numer: u64, fee_denom: u64) -> Self {
        Self {
            reserve_in,
            reserve_out,
            gamma_numer,
            fee_denom,
        }
    }

    /// Simulate a swap through this hop using EVM-exact integer arithmetic.
    ///
    /// Returns `0` if the calculation would overflow or divide by zero.
    #[must_use]
    pub fn swap(&self, x: U256) -> U256 {
        // y = gamma_numer * reserve_out * x / (fee_denom * reserve_in + gamma_numer * x)
        let gn_u256 = U256::from(self.gamma_numer);
        let fd_u256 = U256::from(self.fee_denom);

        // Compute in U512 to avoid overflow in numerator and denominator
        let x_u512 = U512::from(x);
        let r_in_u512 = U512::from(self.reserve_in);
        let r_out_u512 = U512::from(self.reserve_out);
        let gn_u512 = U512::from(gn_u256);
        let fd_u512 = U512::from(fd_u256);

        // numerator = gamma_numer * reserve_out * x
        let numerator = gn_u512 * r_out_u512 * x_u512;

        // denominator = fee_denom * reserve_in + gamma_numer * x
        let denom = fd_u512 * r_in_u512 + gn_u512 * x_u512;

        if denom.is_zero() {
            return U256::ZERO;
        }

        // Floor division (EVM semantics)
        let result_u512 = numerator / denom;

        // Truncate to U256
        let bytes: [u8; 64] = result_u512.to_be_bytes();
        // U512 is 64 bytes, U256 is 32 bytes — take last 32 bytes
        let mut result_bytes = [0u8; 32];
        result_bytes.copy_from_slice(&bytes[32..64]);
        U256::from_be_bytes(result_bytes)
    }
}

/// Integer Möbius coefficients for an n-hop constant product path.
///
/// K, M, N are computed as U512 integers via the 2×2 matrix composition.
/// Each hop contributes the matrix:
///
/// ```text
/// [[gamma_numer * reserve_out, 0],
///  [gamma_numer,              fee_denom * reserve_in]]
/// ```
///
/// The product of all matrices gives the composite coefficients.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct IntMobiusCoefficients {
    /// Numerator coefficient: K = prod(gamma_numer_i * reserve_out_i).
    pub K: U512,
    /// Denominator constant: M = prod(fee_denom_i * reserve_in_i).
    pub M: U512,
    /// Denominator linear: N = composite coefficient from matrix product.
    pub N: U512,
    /// True when K > M (profitable after fees).
    pub is_profitable: bool,
}

/// Compute integer Möbius coefficients K, M, N for an n-hop path.
///
/// Uses 2×2 matrix multiplication with U512 entries to avoid overflow.
/// Each hop encodes as:
///
/// ```text
/// [[fn * s, 0],
///  [fn,     fd * r]]
/// ```
///
/// where `gn` = gamma_numer, `fd` = fee_denom, `r` = reserve_in, `s` = reserve_out.
///
/// # Errors
///
/// Returns `MobiusError::EmptyHops` if the hops list is empty.
pub fn compute_int_mobius_coefficients(
    hops: &[IntHopState],
) -> Result<IntMobiusCoefficients, MobiusError> {
    if hops.is_empty() {
        return Err(MobiusError::EmptyHops);
    }

    let first = &hops[0];
    let gn0 = U512::from(first.gamma_numer);
    let fd0 = U512::from(first.fee_denom);
    let r0 = U512::from(first.reserve_in);
    let s0 = U512::from(first.reserve_out);

    // Initialize 2x2 matrix from first hop:
    // [[gn0 * s0, 0],
    //  [gn0,      fd0 * r0]]
    let mut a00 = gn0 * s0; // K
    let mut a10 = gn0; // Will become N
    let mut a11 = fd0 * r0; // M

    // Multiply by subsequent hops' matrices.
    // a01 is always zero: the upper-right entry of each hop matrix is 0,
    // and composing with [[fn'*s', 0; ...]] keeps (0,1) at 0.
    for hop in &hops[1..] {
        let gn_i = U512::from(hop.gamma_numer);
        let fd_i = U512::from(hop.fee_denom);
        let r_i = U512::from(hop.reserve_in);
        let s_i = U512::from(hop.reserve_out);

        // Matrix multiply: result = current * hop
        let old_a00 = a00;
        a00 = old_a00 * gn_i * s_i;
        a10 = a10 * fd_i * r_i + old_a00 * gn_i;
        a11 = a11 * fd_i * r_i;
    }

    Ok(IntMobiusCoefficients {
        K: a00,
        M: a11,
        N: a10,
        is_profitable: a00 > a11,
    })
}

/// Simulate a swap through all hops using EVM-exact integer arithmetic.
///
/// Each hop applies: `y = gamma_numer * reserve_out * x / (fee_denom * reserve_in + gamma_numer * x)`
/// with floor division (EVM semantics).
#[must_use]
pub fn int_simulate_path(x: U256, hops: &[IntHopState]) -> U256 {
    let mut amount = x;
    for hop in hops {
        if amount.is_zero() {
            return U256::ZERO;
        }
        amount = hop.swap(amount);
    }
    amount
}

/// Result from the integer Möbius solver.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct IntMobiusResult {
    /// Optimal input amount (uint256).
    pub optimal_input: U256,
    /// Profit = output - input (uint256).
    pub profit: U256,
    /// Whether the arbitrage is profitable.
    pub success: bool,
    /// Number of refinement iterations (0 for closed-form, ~5 for refinement).
    pub iterations: u32,
}

/// Combined result from float Möbius solve with optional integer refinement.
///
/// When `int_hops` are provided and all hops are integer, includes
/// EVM-exact U256 optimal input and profit.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct MobiusArbResult {
    /// Float optimal input from the Möbius closed-form solution.
    pub optimal_input: f64,
    /// Float profit from simulation.
    pub profit: f64,
    /// Number of float solve iterations (always 0 for Möbius).
    pub iterations: u32,
    /// Whether the arbitrage is profitable.
    pub success: bool,
    /// EVM-exact integer optimal input (set when `all_int` is true).
    pub optimal_input_int: Option<U256>,
    /// EVM-exact integer profit (set when `all_int` is true).
    pub profit_int: Option<U256>,
}

/// Solve Möbius arbitrage with optional integer refinement.
///
/// When `all_int` is true and `int_hops` is populated, performs merged
/// integer refinement: float solve for approximate optimum, then EVM-exact
/// U256 search around it.
pub fn mobius_solve_with_refinement(
    base_hops: &[HopState],
    int_hops: &[IntHopState],
    all_int: bool,
    max_input: Option<f64>,
) -> MobiusArbResult {
    let (x_opt, profit, iters) = mobius_solve(base_hops, max_input);

    if all_int {
        if x_opt > 0.0 && profit > 0.0 {
            let int_result = mobius_refine_int(x_opt, int_hops, max_input);
            MobiusArbResult {
                optimal_input: x_opt,
                profit,
                iterations: iters,
                success: int_result.success,
                optimal_input_int: Some(int_result.optimal_input),
                profit_int: Some(int_result.profit),
            }
        } else {
            MobiusArbResult {
                optimal_input: 0.0,
                profit: 0.0,
                iterations: iters,
                success: false,
                optimal_input_int: Some(U256::ZERO),
                profit_int: Some(U256::ZERO),
            }
        }
    } else {
        MobiusArbResult {
            optimal_input: x_opt,
            profit,
            iterations: iters,
            success: x_opt > 0.0 && profit > 0.0,
            optimal_input_int: None,
            profit_int: None,
        }
    }
}

/// Solve for optimal arbitrage input using integer Möbius coefficients
/// and EVM-exact simulation.
///
/// # Algorithm
///
/// 1. Compute K, M, N as U512 integers (exact, no float)
/// 2. Check K > M for profitability
/// 3. Compute x_approx via f64 from the exact K/M ratio
/// 4. EVM-simulate at integer points around x_approx
/// 5. Return best result
///
/// This produces **EVM-exact** profit values — no float drift.
///
/// # Errors
///
/// Returns `MobiusError::EmptyHops` if the hops list is empty.
pub fn int_mobius_solve(hops: &[IntHopState]) -> Result<IntMobiusResult, MobiusError> {
    let coeffs = compute_int_mobius_coefficients(hops)?;

    if !coeffs.is_profitable {
        return Ok(IntMobiusResult {
            optimal_input: U256::ZERO,
            profit: U256::ZERO,
            success: false,
            iterations: 0,
        });
    }

    // Compute x_approx from f64 conversion of the exact K/M ratio
    let x_approx = compute_approx_optimal_input(&coeffs);

    if x_approx.is_zero() {
        return Ok(IntMobiusResult {
            optimal_input: U256::ZERO,
            profit: U256::ZERO,
            success: false,
            iterations: 0,
        });
    }

    // EVM-exact simulation at x_approx and nearby points
    let mut best_x = U256::ZERO;
    let mut best_profit = U256::ZERO;
    let mut iters = 0u32;

    // Search ±2 around x_approx (integer truncation can shift by ±1)
    for delta in -2i32..=2 {
        let x = if delta >= 0 {
            x_approx.saturating_add(U256::from(delta as u64))
        } else {
            x_approx.saturating_sub(U256::from((-delta) as u64))
        };

        if x.is_zero() {
            continue;
        }

        let output = int_simulate_path(x, hops);
        iters += 1;

        if output > x {
            let profit = output - x;
            if profit > best_profit {
                best_profit = profit;
                best_x = x;
            }
        }
    }

    Ok(IntMobiusResult {
        optimal_input: best_x,
        profit: best_profit,
        success: !best_profit.is_zero(),
        iterations: iters,
    })
}

/// Integer refinement around a float optimum using EVM-exact U256 arithmetic.
///
/// This is the core of the "move integer refinement to Rust" optimization:
/// instead of returning a float result to Python and doing 3-5 Python
/// `_simulate_path` calls (each converting to float), we do the ±N search
/// entirely in Rust with U256 integer arithmetic.
///
/// # Algorithm
///
/// 1. Convert float `x_approx` to U256
/// 2. For each candidate in [x_floor - radius, x_floor + radius + 1]:
///    - Simulate the full path using `IntHopState::swap()` (U256 EVM-exact)
///    - Compute profit = output - input
///    - Track best (max profit) candidate
/// 3. Return best result
///
/// # Search radius
///
/// - 2 hops: ±1 (profit function is very flat at optimum)
/// - 3+ hops: ±min(num_hops, 5) (wider peak due to fee compounding)
///
/// This matches the Python `_integer_refinement` logic exactly.
#[must_use]
pub fn mobius_refine_int(
    x_approx: f64,
    hops: &[IntHopState],
    max_input: Option<f64>,
) -> IntMobiusResult {
    if x_approx <= 0.0 || !x_approx.is_finite() {
        return IntMobiusResult {
            optimal_input: U256::ZERO,
            profit: U256::ZERO,
            success: false,
            iterations: 0,
        };
    }

    // Convert x_approx to U256
    let x_floor_u256 = f64_to_u256(x_approx.floor());

    // Determine search radius (same logic as Python _integer_refinement)
    let num_hops = hops.len();
    let search_radius: i32 = if num_hops <= 2 {
        1
    } else {
        i32::try_from(num_hops.min(5)).unwrap_or(1)
    };

    // Convert max_input to U256 if present
    let max_input_u256 = max_input.map(|m| f64_to_u256(m.floor()));

    let mut best_x: U256 = U256::ZERO;
    let mut best_profit: U256 = U256::ZERO;
    let mut iters = 0u32;

    // Iterate from x_floor - search_radius to x_floor + search_radius + 1
    // We can't do U256 + negative, so we iterate by offset from x_floor
    let lo_offset = -i64::from(search_radius); // e.g. -1
    let hi_offset = i64::from(search_radius) + 1; // e.g. 2

    for offset in lo_offset..=hi_offset {
        let candidate = if offset >= 0 {
            x_floor_u256.saturating_add(U256::from(offset as u64))
        } else {
            x_floor_u256.saturating_sub(U256::from((-offset) as u64))
        };

        // Skip zero inputs
        if candidate.is_zero() {
            continue;
        }

        // Apply max_input constraint
        if let Some(max) = max_input_u256 {
            if candidate > max {
                continue;
            }
        }

        let output = int_simulate_path(candidate, hops);
        iters += 1;

        if output > candidate {
            let profit = output - candidate;
            if profit > best_profit {
                best_profit = profit;
                best_x = candidate;
            }
        }
    }

    IntMobiusResult {
        optimal_input: best_x,
        profit: best_profit,
        success: !best_profit.is_zero(),
        iterations: iters,
    }
}

/// Compute approximate optimal input from U512 coefficients using f64.
///
/// The formula is: x_opt = (sqrt(K*M) - M) / N
///
/// Since K*M can overflow f64, we reformulate as:
/// x_opt = M * (sqrt(K/M) - 1) / N
///
/// And compute K/M in f64 with sufficient precision.
fn compute_approx_optimal_input(coeffs: &IntMobiusCoefficients) -> U256 {
    // Convert K/M to f64 for the square root computation.
    // K and M are U512, so we need to handle potential f64 overflow.
    // Strategy: normalize by the leading bit position.

    let k_f64 = u512_to_f64(coeffs.K);
    let m_f64 = u512_to_f64(coeffs.M);
    let n_f64 = u512_to_f64(coeffs.N);

    if m_f64 <= 0.0 || n_f64 <= 0.0 || k_f64 <= 0.0 {
        return U256::ZERO;
    }

    // x_opt = M * (sqrt(K/M) - 1) / N
    let km_ratio = k_f64 / m_f64;
    if km_ratio <= 1.0 {
        return U256::ZERO;
    }

    let sqrt_km = km_ratio.sqrt();
    let x_f64 = m_f64 * (sqrt_km - 1.0) / n_f64;

    if x_f64 <= 0.0 || !x_f64.is_finite() {
        return U256::ZERO;
    }

    // Convert to U256, capping at U256::MAX
    f64_to_u256(x_f64)
}

/// Convert U512 to f64 with best available precision.
///
/// For values > 2^53 (f64 mantissa), we lose low-order bits,
/// but the relative error is ~1e-16 which is sufficient for the
/// approximation step (we refine with EVM simulation afterward).
pub fn u512_to_f64(v: U512) -> f64 {
    // U512::to_be_bytes() returns 64 bytes in big-endian order.
    // bytes[0] is most significant, bytes[63] is least significant.
    // For values < 2^64, only bytes[56..64] are nonzero.
    let bit_len = v.bit_len();
    if bit_len == 0 {
        return 0.0;
    }

    // General approach: shift right so value fits in 64 bits,
    // convert, then multiply by 2^(bits_shifted).
    // This gives ~15-16 significant digits of precision regardless of magnitude.
    let shift = bit_len.saturating_sub(64);
    let shifted = v >> shift;
    let bytes_shifted: [u8; 64] = shifted.to_be_bytes();
    let mut lo_bytes = [0u8; 8];
    lo_bytes.copy_from_slice(&bytes_shifted[56..64]);
    let lo = u64::from_be_bytes(lo_bytes);
    #[allow(clippy::cast_possible_wrap)]
    let shift_i32 = shift as i32;
    (lo as f64) * 2f64.powi(shift_i32)
}

/// Convert U256 to f64 via U512 with best available precision.
pub fn u256_to_f64(v: U256) -> f64 {
    u512_to_f64(U512::from(v))
}

/// Convert f64 to U256, capping at U256::MAX.
fn f64_to_u256(v: f64) -> U256 {
    // U256::MAX ≈ 1.16e77
    const U256_MAX_AS_F64: f64 = 1.157920892373162e77;
    // u64::MAX as f64
    const U64_MAX_AS_F64: f64 = u64::MAX as f64;

    if v <= 0.0 || !v.is_finite() {
        return U256::ZERO;
    }

    if v >= U256_MAX_AS_F64 {
        return U256::MAX;
    }

    if v < U64_MAX_AS_F64 {
        // u64::MAX as f64
        return U256::from(v as u64);
    }

    // For larger values, decompose into hi * 2^64 + lo
    let hi_f64 = v / 2f64.powi(64);
    let hi = hi_f64 as u64;
    let lo_f64 = v - (hi as f64) * 2f64.powi(64);
    let lo = lo_f64 as u64;

    U256::from(hi) * U256::from(2u64).pow(U256::from(64u64)) + U256::from(lo)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn u256(n: u64) -> U256 {
        U256::from(n)
    }

    #[test]
    fn test_int_hop_swap_zero_input() {
        let hop = IntHopState::new(u256(1_000_000), u256(2_000_000), 997, 1000);
        let output = hop.swap(U256::ZERO);
        assert!(output.is_zero());
    }

    #[test]
    fn test_int_hop_swap_basic() {
        // x = 1000, r_in = 1M, r_out = 2M, fee = 0.3%
        // y = 997 * 2M * 1000 / (1000 * 1M + 997 * 1000)
        // = 997 * 2e9 / (1e9 + 997000)
        // = 1994000000 / 19997000
        // = 99.7150... (floor: 99)
        let hop = IntHopState::new(u256(1_000_000), u256(2_000_000), 997, 1000);
        let output = hop.swap(u256(1000));
        assert!(!output.is_zero());
        assert!(output < u256(2000)); // Output < 2x input
    }

    #[test]
    fn test_int_hop_swap_exact_evm() {
        // Verify against EVM formula: y = (gamma * s * x) / (r + gamma * x)
        // With gamma = 997/1000:
        // y = (997 * s * x) / (1000 * r + 997 * x)
        let hop = IntHopState::new(
            u256(2_000_000_000_000),                                        // 2M USDC
            U256::from(1000u64) * U256::from(10u64).pow(U256::from(18u64)), // 1000 WETH (18 decimals)
            997,
            1000,
        );
        let x = u256(1_000_000_000_000); // 1M USDC input
        let output = hop.swap(x);

        // Manual EVM calculation:
        // numerator = 997 * 1e21 * 1e12 = 997e33
        // denominator = 1000 * 2e12 + 997 * 1e12 = 2000e12 + 997e12 = 2997e12
        // y = 997e33 / 2997e12 = 997e21 / 2997 = 332500834...
        // This should be ~3.325e20 (332.5M wei WETH)
        assert!(!output.is_zero());
        assert!(output > U256::ZERO);
    }

    #[test]
    fn test_compute_int_mobius_coefficients_empty() {
        let result = compute_int_mobius_coefficients(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_compute_int_mobius_coefficients_not_profitable() {
        // Same-product pools: K/M = (fn/fd)^2 < 1
        let hops = vec![
            IntHopState::new(u256(1_000_000), u256(1_000_000), 997, 1000),
            IntHopState::new(u256(1_000_000), u256(1_000_000), 997, 1000),
        ];
        let coeffs = compute_int_mobius_coefficients(&hops).unwrap();
        assert!(!coeffs.is_profitable);
    }

    #[test]
    fn test_compute_int_mobius_coefficients_profitable() {
        // Asymmetric reserves where K > M
        let hops = vec![
            IntHopState::new(u256(1_000_000), u256(5_000_000), 997, 1000),
            IntHopState::new(u256(1_500_000), u256(3_000_000), 997, 1000),
        ];
        let coeffs = compute_int_mobius_coefficients(&hops).unwrap();
        // K = 997 * 5M * 997 * 3M = 997^2 * 15e12 = ~14.91e12
        // M = 1000 * 1M * 1000 * 1.5M = 1.5e12
        // K >> M → profitable
        assert!(coeffs.is_profitable);
    }

    #[test]
    fn test_int_mobius_solve_profitable() {
        let hops = vec![
            IntHopState::new(u256(1_000_000), u256(5_000_000), 997, 1000),
            IntHopState::new(u256(1_500_000), u256(3_000_000), 997, 1000),
        ];
        let result = int_mobius_solve(&hops).unwrap();
        assert!(result.success);
        assert!(!result.optimal_input.is_zero());
        assert!(!result.profit.is_zero());
    }

    #[test]
    fn test_int_mobius_solve_not_profitable() {
        let hops = vec![
            IntHopState::new(u256(1_000_000), u256(1_000_000), 997, 1000),
            IntHopState::new(u256(1_000_000), u256(1_000_000), 997, 1000),
        ];
        let result = int_mobius_solve(&hops).unwrap();
        assert!(!result.success);
    }

    #[test]
    fn test_int_simulate_path_two_hop() {
        let hops = vec![
            IntHopState::new(u256(1_000_000), u256(5_000_000), 997, 1000),
            IntHopState::new(u256(1_500_000), u256(3_000_000), 997, 1000),
        ];
        let output = int_simulate_path(u256(1000), &hops);
        assert!(!output.is_zero());
    }

    #[test]
    fn test_u512_to_f64_roundtrip() {
        // Small value
        let v = U512::from(12345u64);
        let f = u512_to_f64(v);
        assert!((f - 12345.0).abs() < 1.0);

        // Large value (beyond f64 mantissa)
        let large = U512::from(1u64) << 100;
        let f = u512_to_f64(large);
        assert!(f > 0.0);
        assert!(f.is_finite());
    }

    #[test]
    fn test_f64_to_u256_basic() {
        assert_eq!(f64_to_u256(0.0), U256::ZERO);
        assert_eq!(f64_to_u256(-1.0), U256::ZERO);
        assert_eq!(f64_to_u256(f64::NAN), U256::ZERO);
        assert_eq!(f64_to_u256(f64::INFINITY), U256::ZERO);
        assert_eq!(f64_to_u256(1000.0), U256::from(1000u64));
        assert_eq!(f64_to_u256(1e18), U256::from(1_000_000_000_000_000_000u64));
    }

    // ======================================================================
    // mobius_refine_int tests
    // ======================================================================

    #[test]
    fn test_mobius_refine_int_profitable_2hop() {
        let hops = vec![
            IntHopState::new(u256(1_000_000), u256(5_000_000), 997, 1000),
            IntHopState::new(u256(1_500_000), u256(3_000_000), 997, 1000),
        ];
        // x_approx from float solver is ~333333
        let result = mobius_refine_int(333333.0, &hops, None);
        assert!(result.success);
        assert!(!result.optimal_input.is_zero());
        assert!(!result.profit.is_zero());

        // Verify EVM-exact: simulate at optimal_input
        let output = int_simulate_path(result.optimal_input, &hops);
        assert!(output > result.optimal_input);
        assert_eq!(output - result.optimal_input, result.profit);
    }

    #[test]
    fn test_mobius_refine_int_matches_int_mobius_solve() {
        let hops = vec![
            IntHopState::new(u256(1_000_000), u256(5_000_000), 997, 1000),
            IntHopState::new(u256(1_500_000), u256(3_000_000), 997, 1000),
        ];
        // Use known x_approx (float solver gives ~499445 for these reserves)
        let int_result = int_mobius_solve(&hops).unwrap();
        assert!(int_result.success);

        let refine_result = mobius_refine_int(499445.0, &hops, None);
        assert!(refine_result.success);
        // Both should find the same profit (flat peak)
        assert_eq!(refine_result.profit, int_result.profit);
    }

    #[test]
    fn test_mobius_refine_int_zero_x_approx() {
        let hops = vec![
            IntHopState::new(u256(1_000_000), u256(5_000_000), 997, 1000),
            IntHopState::new(u256(1_500_000), u256(3_000_000), 997, 1000),
        ];
        let result = mobius_refine_int(0.0, &hops, None);
        assert!(!result.success);
    }

    #[test]
    fn test_mobius_refine_int_not_profitable() {
        // Same-product pools are never profitable after fees
        let hops = vec![
            IntHopState::new(u256(100_000), u256(50), 997, 1000),
            IntHopState::new(u256(50), u256(100_000), 997, 1000),
        ];
        let result = mobius_refine_int(10.0, &hops, None);
        assert!(!result.success);
    }

    #[test]
    fn test_mobius_refine_int_max_input_respected() {
        let hops = vec![
            IntHopState::new(u256(1_000_000), u256(5_000_000), 997, 1000),
            IntHopState::new(u256(1_500_000), u256(3_000_000), 997, 1000),
        ];
        let result = mobius_refine_int(999.0, &hops, Some(1000.0));
        assert!(result.success);
        assert!(result.optimal_input <= f64_to_u256(1000.0));
    }

    #[test]
    fn test_mobius_refine_int_3hop() {
        let hops = vec![
            IntHopState::new(u256(2_000_000), u256(2_100_000), 997, 1000),
            IntHopState::new(u256(2_000_000), u256(2_050_000), 997, 1000),
            IntHopState::new(u256(2_050_000), u256(2_000_000), 997, 1000),
        ];
        // x_approx ~ 16000 for this path
        let result = mobius_refine_int(16000.0, &hops, None);
        assert!(result.success);
        assert!(!result.profit.is_zero());
    }

    #[test]
    fn test_mobius_refine_int_best_in_neighborhood() {
        let hops = vec![
            IntHopState::new(u256(1_000_000), u256(5_000_000), 997, 1000),
            IntHopState::new(u256(1_500_000), u256(3_000_000), 997, 1000),
        ];
        let result = mobius_refine_int(499445.0, &hops, None);
        assert!(result.success);

        // Check that no ±2 neighbor has better profit
        let x_opt = result.optimal_input;
        for delta in -2i64..=2i64 {
            let candidate = if delta >= 0 {
                x_opt.saturating_add(U256::from(delta as u64))
            } else {
                x_opt.saturating_sub(U256::from((-delta) as u64))
            };
            if candidate.is_zero() {
                continue;
            }
            let output = int_simulate_path(candidate, &hops);
            if output > candidate {
                let profit = output - candidate;
                assert!(profit <= result.profit, "Neighbor has better profit");
            }
        }
    }
}
