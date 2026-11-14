//! Uniswap V3 tick-range constants + the shared decode-side tick check.
//!
//! Every V3/V4 decoder that carries an `int24` tick field hands the decoded
//! value through [`validate_tick`], which widens it to the `i32` the event
//! structs hold and range-checks it against the Uniswap V3 bounds
//! `[-887_272, 887_272]`. The ABI shape of the word (an `int24` sign-extended
//! to 32 bytes) belongs to Alloy's validating decoder — every decoder reads its
//! event through one — so this module owns only the protocol bound: a decoded
//! tick outside the bounds indicates a corrupt or synthetic log, and the check
//! lives in one place rather than as a repeated `887272` literal.
//!
//! The canonical tick-math bounds live in
//! `degenbot-concentrated-liquidity-math::tick_math` (`MIN_TICK`/`MAX_TICK`),
//! which this alloy-only leaf does not depend on.

use alloy::primitives::aliases::I24;

/// Minimum Uniswap V3 tick (the decode-side validation bound for an `int24`
/// tick field). A decoded tick below this indicates a corrupt or synthetic log.
pub const UNISWAP_V3_MIN_TICK: i32 = -887_272;

/// Maximum Uniswap V3 tick (the decode-side validation bound for an `int24`
/// tick field). A decoded tick above this indicates a corrupt or synthetic log.
pub const UNISWAP_V3_MAX_TICK: i32 = 887_272;

/// Widen an ABI-decoded `int24` tick to the `i32` the decoded event structs
/// carry, rejecting a value outside
/// [`UNISWAP_V3_MIN_TICK`]..=[`UNISWAP_V3_MAX_TICK`].
///
/// The ABI half of the check is Alloy's: a tick word that is not a
/// sign-extended `int24` fails the decoder's validating decode before it
/// reaches this function, so `None` here means the tick is outside the protocol
/// range — never that the word was malformed.
#[must_use]
pub fn validate_tick(tick: I24) -> Option<i32> {
    let tick = i32::try_from(tick).ok()?;
    (UNISWAP_V3_MIN_TICK..=UNISWAP_V3_MAX_TICK)
        .contains(&tick)
        .then_some(tick)
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn i24(tick: i32) -> I24 {
        I24::try_from(tick).unwrap()
    }

    #[test]
    fn accepts_zero_tick() {
        assert_eq!(validate_tick(i24(0)), Some(0));
    }

    #[test]
    fn accepts_positive_tick_at_max_bound() {
        assert_eq!(validate_tick(i24(887_272)), Some(887_272));
    }

    #[test]
    fn accepts_negative_tick_at_min_bound() {
        assert_eq!(validate_tick(i24(-887_272)), Some(-887_272));
    }

    #[test]
    fn rejects_tick_above_max() {
        assert_eq!(validate_tick(i24(887_273)), None);
    }

    #[test]
    fn rejects_tick_below_min() {
        assert_eq!(validate_tick(i24(-887_273)), None);
    }

    #[test]
    fn accepts_negative_common_spacing() {
        assert_eq!(validate_tick(i24(-60)), Some(-60));
    }
}
