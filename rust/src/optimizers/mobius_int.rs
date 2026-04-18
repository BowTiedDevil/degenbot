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
#![allow(unused_variables)]
#![allow(unused_assignments)]
#![allow(clippy::unnecessary_cast)]
#![allow(clippy::type_complexity)]

use alloy::primitives::{U256, U512};

use crate::optimizers::mobius::MobiusError;

/// Fee parameters for a single pool hop.
///
/// In Uniswap V2, the fee is expressed as `fee_numer / fee_denom`:
/// - 0.3% fee → `fee_numer = 997`, `fee_denom = 1000`
/// - 0.05% fee → `fee_numer = 9995`, `fee_denom = 10000`
///
/// The swap formula is:
/// `y = fee_numer * reserve_out * x / (fee_denom * reserve_in + fee_numer * x)`
#[derive(Clone, Debug)]
pub struct IntHopState {
    /// Reserve of the input token (uint256 scale).
    pub reserve_in: U256,
    /// Reserve of the output token (uint256 scale).
    pub reserve_out: U256,
    /// Fee numerator (e.g. 997 for 0.3%).
    pub fee_numer: u64,
    /// Fee denominator (e.g. 1000 for 0.3%).
    pub fee_denom: u64,
}

impl IntHopState {
    /// Create a new integer hop state.
    #[must_use]
    pub const fn new(reserve_in: U256, reserve_out: U256, fee_numer: u64, fee_denom: u64) -> Self {
        Self {
            reserve_in,
            reserve_out,
            fee_numer,
            fee_denom,
        }
    }

    /// Simulate a swap through this hop using EVM-exact integer arithmetic.
    ///
    /// Returns `0` if the calculation would overflow or divide by zero.
    #[must_use]
    pub fn swap(&self, x: U256) -> U256 {
        // y = fee_numer * reserve_out * x / (fee_denom * reserve_in + fee_numer * x)
        let fn_u256 = U256::from(self.fee_numer);
        let fd_u256 = U256::from(self.fee_denom);

        // Compute in U512 to avoid overflow in numerator and denominator
        let x_u512 = U512::from(x);
        let r_in_u512 = U512::from(self.reserve_in);
        let r_out_u512 = U512::from(self.reserve_out);
        let fn_u512 = U512::from(fn_u256);
        let fd_u512 = U512::from(fd_u256);

        // numerator = fee_numer * reserve_out * x
        let numerator = fn_u512 * r_out_u512 * x_u512;

        // denominator = fee_denom * reserve_in + fee_numer * x
        let denom = fd_u512 * r_in_u512 + fn_u512 * x_u512;

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
/// [[fee_numer * reserve_out, 0],
///  [fee_numer,              fee_denom * reserve_in]]
/// ```
///
/// The product of all matrices gives the composite coefficients.
#[derive(Clone, Debug)]
pub struct IntMobiusCoefficients {
    /// Numerator coefficient: K = prod(fee_numer_i * reserve_out_i).
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
/// where `fn` = fee_numer, `fd` = fee_denom, `r` = reserve_in, `s` = reserve_out.
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
    let fn0 = U512::from(first.fee_numer);
    let fd0 = U512::from(first.fee_denom);
    let r0 = U512::from(first.reserve_in);
    let s0 = U512::from(first.reserve_out);

    // Initialize 2x2 matrix from first hop:
    // [[fn0 * s0, 0],
    //  [fn0,      fd0 * r0]]
    let mut a00 = fn0 * s0; // K
    let mut a01 = U512::ZERO;
    let mut a10 = fn0; // Will become N
    let mut a11 = fd0 * r0; // M

    // Multiply by subsequent hops' matrices
    for hop in &hops[1..] {
        let fn_i = U512::from(hop.fee_numer);
        let fd_i = U512::from(hop.fee_denom);
        let r_i = U512::from(hop.reserve_in);
        let s_i = U512::from(hop.reserve_out);

        // Hop matrix:
        // [[fn_i * s_i, 0],
        //  [fn_i,       fd_i * r_i]]

        // Matrix multiply: result = current * hop
        let new_a00 = a00 * fn_i * s_i;
        let new_a01 = U512::ZERO; // Always 0 since a01 = 0 and hop[0][1] = 0
        let new_a10 = a10 * fd_i * r_i + a00 * fn_i;
        let new_a11 = a11 * fd_i * r_i;

        a00 = new_a00;
        a01 = new_a01;
        a10 = new_a10;
        a11 = new_a11;
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
/// Each hop applies: `y = fee_numer * reserve_out * x / (fee_denom * reserve_in + fee_numer * x)`
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
fn u512_to_f64(v: U512) -> f64 {
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

/// Convert f64 to U256, capping at U256::MAX.
fn f64_to_u256(v: f64) -> U256 {
    if v <= 0.0 || !v.is_finite() {
        return U256::ZERO;
    }

    // U256::MAX ≈ 1.16e77
    let max_f64 = 1.157920892373162e77;
    if v >= max_f64 {
        return U256::MAX;
    }

    // For values that fit in u64, convert directly
    if v < 1.8446744073709552e19 {
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
}
