//! Uniswap V3 tick-range constants + the shared int24 extraction helper.
//!
//! The decoders that slice V3/V4 event logs must (a) read a sign-extended
//! `int24` from the tail of a 32-byte ABI word and (b) range-check it against
//! the Uniswap V3 tick bounds `[-887_272, 887_272]`. Both concerns were
//! duplicated across `v3_swap_decoder`, `v3_mint_burn_decoder`,
//! `v4_swap_decoder`, `v4_modify_liquidity_decoder`, and
//! `pool_created_decoder` — the `887272` literal alone appeared 25+ times.
//!
//! This module is the single source of truth for the range + the extraction,
//! so a future tick-range change (or a new int24-decoding event) edits one
//! place. The constants are **decode-side validation** (an already-decoded
//! int24 that lies outside the Uniswap V3 bounds indicates a corrupt or
//! synthetic log); the canonical tick-math bounds live in
//! `degenbot-concentrated-liquidity-math::tick_math` (`MIN_TICK`/`MAX_TICK`), which this
//! alloy-only leaf does not depend on.

/// Minimum Uniswap V3 tick (the decode-side validation bound for an `int24`
/// tick field). A decoded tick below this indicates a corrupt or synthetic log.
pub const UNISWAP_V3_MIN_TICK: i32 = -887_272;

/// Maximum Uniswap V3 tick (the decode-side validation bound for an `int24`
/// tick field). A decoded tick above this indicates a corrupt or synthetic log.
pub const UNISWAP_V3_MAX_TICK: i32 = 887_272;

/// Extract an `int24` from the tail of a 32-byte ABI word (sign-extended to
/// `int256` in the ABI; the value occupies the last 4 bytes). Range-checks
/// the result against the Uniswap V3 tick bounds
/// [`UNISWAP_V3_MIN_TICK`]..=[`UNISWAP_V3_MAX_TICK`].
///
/// Pass a 32-byte slice: a data word (`&data[offset..offset + 32]`) or a
/// topic word (`&topic.0`). Returns `None` if the slice is shorter than
/// 32 bytes or the value is outside the valid tick range.
#[must_use]
pub fn extract_int24_from_word(word: &[u8]) -> Option<i32> {
    if word.len() < 32 {
        return None;
    }
    let last4: [u8; 4] = word[28..32].try_into().ok()?;
    let value = i32::from_be_bytes(last4);
    if !(UNISWAP_V3_MIN_TICK..=UNISWAP_V3_MAX_TICK).contains(&value) {
        return None;
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode an `int24` tick as a sign-extended 32-byte `int256` word.
    fn int24_word(tick: i32) -> [u8; 32] {
        // Sign-extend: replicate the int24's sign bit into the high 28 bytes.
        let mut word = [0u8; 32];
        word[29..32].copy_from_slice(&tick.to_be_bytes()[1..4]);
        if tick < 0 {
            word.iter_mut().take(29).for_each(|b| *b = 0xff);
            word[28] = 0xff; // propagate sign into byte 28 too
        }
        word
    }

    #[test]
    fn extracts_zero_tick() {
        assert_eq!(extract_int24_from_word(&[0u8; 32]), Some(0));
    }

    #[test]
    fn extracts_positive_tick_at_max_bound() {
        assert_eq!(extract_int24_from_word(&int24_word(887_272)), Some(887_272));
    }

    #[test]
    fn extracts_negative_tick_at_min_bound() {
        assert_eq!(
            extract_int24_from_word(&int24_word(-887_272)),
            Some(-887_272)
        );
    }

    #[test]
    fn rejects_tick_above_max() {
        assert_eq!(extract_int24_from_word(&int24_word(887_273)), None);
    }

    #[test]
    fn rejects_tick_below_min() {
        assert_eq!(extract_int24_from_word(&int24_word(-887_273)), None);
    }

    #[test]
    fn returns_none_for_short_slice() {
        assert_eq!(extract_int24_from_word(&[0u8; 31]), None);
    }

    #[test]
    fn reads_only_tail_ignoring_high_bytes_for_in_range_value() {
        // A value whose high bytes are non-zero but whose tail decodes to an
        // in-range tick must still extract the tail — the ABI sign-extends,
        // so high bytes are sign bits, not magnitude. 0xff..ff for a negative.
        let word = int24_word(-60);
        assert_eq!(extract_int24_from_word(&word), Some(-60));
    }
}
