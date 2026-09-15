//! Curve `rate_multipliers` / `precision_multipliers` derivation.
//!
//! The single Rust source of truth for the Curve scaling multipliers a pool's
//! `xp` (rate-adjusted balances) and swap outputs are computed from
//! (ADR-005 slice 11c, ergo `JLAPAC`). Both the Rust construction path
//! (`degenbot-bot` `build_curve_pool`) and the `degenbot._ffi.curve_math`
//! Python surface call this, so the derivation lives in exactly one place.
//!
//! This replaces the deleted Python
//! `curve_stableswap_liquidity_pool._compute_rate_and_precision_multipliers`
//! shim, which the pre-cutover factory used to derive the same values for the
//! FFI registration surface.

use alloy::primitives::U256;

/// Curve `PRECISION_DECIMALS` — the fixed-point exponent of
/// `rate_multipliers` / `precision_multipliers` (`10**18`).
pub const PRECISION_DECIMALS: u32 = 18;

/// Derive a Curve pool's `(rate_multipliers, precision_multipliers)`.
///
/// `token_decimals` are the per-coin ERC20 `decimals()` values.
///
/// - When `precision_multipliers` is `Some` (the lending-token override
///   path) it is returned verbatim as the precision multipliers and
///   `rate = pm * 10**precision_decimals`.
/// - Otherwise both derive from `token_decimals`:
///   `rate = 10**(2*precision_decimals - d)`,
///   `precision = 10**(precision_decimals - d)`.
///
/// # Panics
///
/// Panics if a token decimal exceeds `2 * precision_decimals` (a malformed
/// ERC20 — real token decimals are `<= 18` for Curve coins).
#[must_use]
pub fn derive_rate_and_precision_multipliers(
    token_decimals: &[u8],
    precision_multipliers: Option<&[U256]>,
    precision_decimals: u32,
) -> (Vec<U256>, Vec<U256>) {
    let ten = U256::from(10u64);
    if let Some(pms) = precision_multipliers {
        let rate = pms
            .iter()
            .map(|pm| *pm * ten.pow(U256::from(precision_decimals)))
            .collect();
        (rate, pms.to_vec())
    } else {
        let rate = token_decimals
            .iter()
            .map(|d| ten.pow(U256::from(2 * precision_decimals - u32::from(*d))))
            .collect();
        let precision = token_decimals
            .iter()
            .map(|d| ten.pow(U256::from(precision_decimals - u32::from(*d))))
            .collect();
        (rate, precision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(exp: u32) -> U256 {
        U256::from(10u64).pow(U256::from(exp))
    }

    #[test]
    fn derives_from_token_decimals_without_overrides() {
        // 6-dec coin -> rate 1e30 / precision 1e12; 18-dec -> 1e18 / 1e0.
        let (rate, precision) =
            derive_rate_and_precision_multipliers(&[6, 18], None, PRECISION_DECIMALS);
        assert_eq!(rate, vec![e(30), e(18)]);
        assert_eq!(precision, vec![e(12), e(0)]);
    }

    #[test]
    fn precision_overrides_drive_the_rate_product() {
        // A lending coin's precision override (e.g. cToken underlying 6-dec)
        // is returned verbatim; rate = pm * 10**18.
        let pms = vec![e(12), e(0)];
        let (rate, precision) =
            derive_rate_and_precision_multipliers(&[8, 18], Some(&pms), PRECISION_DECIMALS);
        assert_eq!(rate, vec![e(30), e(18)]);
        assert_eq!(precision, pms);
    }

    #[test]
    fn empty_input_yields_empty_vectors() {
        let (rate, precision) =
            derive_rate_and_precision_multipliers(&[], None, PRECISION_DECIMALS);
        assert!(rate.is_empty());
        assert!(precision.is_empty());
    }

    #[test]
    fn three_coin_tripool_shape() {
        // DAI 18 / USDC 6 / USDT 6 (the canonical tripool).
        let (rate, precision) =
            derive_rate_and_precision_multipliers(&[18, 6, 6], None, PRECISION_DECIMALS);
        assert_eq!(rate, vec![e(18), e(30), e(30)]);
        assert_eq!(precision, vec![e(0), e(12), e(12)]);
    }
}
