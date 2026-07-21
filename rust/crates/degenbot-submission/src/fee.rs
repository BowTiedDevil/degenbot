//! Fee finalization — the `maxFeePerGas` / `maxPriorityFeePerGas` arithmetic
//! that the Python oracle performs inline at submission time.
//!
//! # Parity oracle (ADR-005 §4.2)
//!
//! `examples/eth_backrun_v2_v3_v4_rust.py` L2623–L2624:
//!
//! ```python
//! tx_params["maxPriorityFeePerGas"] = priority_fee
//! tx_params["maxFeePerGas"] = int(1.5 * base_fee_next) + priority_fee
//! ```
//!
//! [`finalize_fees`] reproduces this exactly, including the **float→int
//! truncate boundary**: `int(1.5 * base_fee_next)` uses Python `int()` on a
//! `float`, which truncates toward zero. The Rust path uses
//! `(1.5 * base_fee_next as f64) as u128`, which is the identical IEEE-754
//! double-precision multiply + toward-zero truncation.
//!
//! ## The float boundary (documented)
//!
//! `base_fee_next` is a `u128` wei value. For realistic mainnet base fees
//! (~1e10–1e11 wei, i.e. 10–100 gwei), `1.5 * base_fee_next` is well under
//! `2^53` (~9e15), so the `f64` multiply is **exact** and the truncate is a
//! no-op (the value is already whole). For pathological `base_fee_next`
//! values above `~6e37`, `1.5 * x` overflows `f64`'s exact-integer range,
//! but such values are never produced on mainnet (the 1.5× headroom would be
//! roughly `10^38` wei = `10^29` gwei — impossible). The float path is pinned
//! here for **byte-exact Python parity**, not for arithmetic range — the
//! same convention as [`_compute_priority_fee`] in the Simulation leaf (the
//! `priority_fee` consumed here is *produced* there using the same
//! float→int truncate convention).
//!
//! [`_compute_priority_fee`]: the Simulation leaf (task `YL2MTH`) — cross-epic
//! reference; submission consumes the fee off `tx_params`, no edge.

use crate::error::{SubmissionError, SubmissionResult};
use crate::params::TxParams;

/// The 1.5× headroom multiplier the Python oracle applies to `base_fee_next`
/// when computing `maxFeePerGas`. Exposed as a `const` so the §4.2 parity
/// test can pin the convention without re-deriving the float literal.
pub const MAX_FEE_HEADROOM: f64 = 1.5;

/// Finalize `maxFeePerGas` / `maxPriorityFeePerGas` onto `tx_params`, mirroring
/// the Python oracle's inline fee computation.
///
/// Sets:
/// - `tx_params.max_priority_fee_per_gas = priority_fee`
/// - `tx_params.max_fee_per_gas = int(1.5 * base_fee_next) + priority_fee`
///
/// `base_fee_next` is the *next-block* base fee (`JTLWA3`'s `next_base_fee`,
/// already computed — this crate consumes it, no edge). `priority_fee` is
/// the Simulation leaf's `_compute_priority_fee` output (consumed off
/// `tx_params` — no hard edge; for §4.2 parity it is a fixture value).
///
/// # Errors
///
/// Returns [`SubmissionError::FeeOverflow`] only if the integer addition
/// overflows `u128` (impossible for realistic mainnet fees — see the float
/// boundary note on this module).
///
/// # Examples
///
/// ```
/// # use degenbot_submission::{fee::finalize_fees, params::TxParams};
/// # use alloy::primitives::{Address, Bytes};
/// let mut params = TxParams::new(Address::ZERO, Bytes::new(), 250_000, 7);
/// finalize_fees(&mut params, 10_000_000_000, 2_000_000_000).unwrap();
/// // int(1.5 * 10 gwei) + 2 gwei = 15 gwei + 2 gwei = 17 gwei
/// assert_eq!(params.max_fee_per_gas, 17_000_000_000);
/// assert_eq!(params.max_priority_fee_per_gas, 2_000_000_000);
/// ```
pub fn finalize_fees(
    tx_params: &mut TxParams,
    base_fee_next: u128,
    priority_fee: u128,
) -> SubmissionResult<()> {
    tx_params.max_priority_fee_per_gas = priority_fee;
    tx_params.max_fee_per_gas = compute_max_fee(base_fee_next, priority_fee)?;
    Ok(())
}

/// Compute `maxFeePerGas = int(1.5 * base_fee_next) + priority_fee`.
///
/// Exposed separately from [`finalize_fees`] so the §4.2 parity proptest can
/// drive the arithmetic directly without constructing a full `TxParams`.
/// See the module docs for the float→int truncate boundary.
///
/// # Errors
///
/// Returns [`SubmissionError::FeeOverflow`] only if the integer addition
/// overflows `u128` (impossible for realistic mainnet fees — see the float
/// boundary note on this module).
pub fn compute_max_fee(base_fee_next: u128, priority_fee: u128) -> SubmissionResult<u128> {
    // Python: int(1.5 * base_fee_next) — float multiply then toward-zero
    // truncate via Python `int()`. The Rust `as u128` cast on an `f64` is
    // the same toward-zero truncation. See module docs for the float boundary.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let headroom = (MAX_FEE_HEADROOM * base_fee_next as f64) as u128;
    headroom
        .checked_add(priority_fee)
        .ok_or(SubmissionError::FeeOverflow {
            base_fee_next,
            priority_fee,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_math_matches_python_int_truncate() {
        // int(1.5 * 10 gwei) + 2 gwei = 15 gwei + 2 gwei = 17 gwei
        assert_eq!(
            compute_max_fee(10_000_000_000, 2_000_000_000).unwrap(),
            17_000_000_000
        );
        // int(1.5 * 7) + 1 = 10 + 1 = 11  (7*1.5=10.5 → int truncates to 10)
        assert_eq!(compute_max_fee(7, 1).unwrap(), 11);
        // int(1.5 * 1) + 0 = 1  (1*1.5=1.5 → int truncates to 1)
        assert_eq!(compute_max_fee(1, 0).unwrap(), 1);
        // int(1.5 * 0) + 5 = 5
        assert_eq!(compute_max_fee(0, 5).unwrap(), 5);
    }

    #[test]
    fn finalize_fees_sets_both_fields() {
        let mut params = crate::params::TxParams::new(
            alloy::primitives::Address::ZERO,
            alloy::primitives::Bytes::new(),
            250_000,
            7,
        );
        finalize_fees(&mut params, 10_000_000_000, 2_000_000_000).unwrap();
        assert_eq!(params.max_fee_per_gas, 17_000_000_000);
        assert_eq!(params.max_priority_fee_per_gas, 2_000_000_000);
    }

    proptest::proptest! {
        #[test]
        fn fee_math_never_overflows_for_realistic_base_fees(base_fee in 0u128..1_000_000_000_000_000u128, priority in 0u128..100_000_000_000_000u128) {
            // For any realistic base fee (<1e15 wei = 1e6 gwei) + priority (<1e14),
            // 1.5x never overflows u128 — the headroom path is infallible here.
            let computed = compute_max_fee(base_fee, priority).unwrap();
            // Property: computed >= priority (1.5x headroom is non-negative)
            proptest::prop_assert!(computed >= priority);
            // Property: computed >= int(1.5 * base_fee) (priority added)
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            let headroom = (1.5_f64 * base_fee as f64) as u128;
            proptest::prop_assert_eq!(computed, headroom.saturating_add(priority));
        }
    }
}
