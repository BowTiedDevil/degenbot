//! The `execute(commands, config)` ABI `config` `uint256` packing.
//!
//! Ports `examples/cmd_stream.py::pack_config` (L137–L171) + the
//! `pack_expected_balance` deprecated alias (L173–L184). Mirrors the
//! `cmd_executor.vy` `execute` config-bitfield layout:
//!
//! - bits `0–7`:   `check_mode` (0=skip, 1=WETH+ETH, 2=ERC6909 WETH)
//! - bits `8–23`:  `bribe_bips` (0=none, 1–10000=basis points)
//! - bits `24–31`: `bribe_recipient_idx` (0=block.coinbase, 1–31=table idx)
//! - bits `32–255`: `expected_value` (pre-tx balance for the selected mode)
//!
//! Bribes moved out of the command stream (opcodes 0x02/0x03 reserved) into
//! this parameter; pass the result as the 2nd arg to `execute(commands, config)`.

use alloy::primitives::U256;

use crate::encoders::MAX_INDEXED_ADDRESSES;

/// Max bribe in basis points (100% = 10000 bips).
pub const MAX_BRIBE_BIPS: u16 = 10_000;

/// Max `check_mode` selector (0=skip, 1=WETH+ETH, 2=ERC6909 WETH, 3=SWEEP).
pub const MAX_CHECK_MODE: u8 = 3;

/// Error raised by an out-of-range config field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// `check_mode` not in `0..=3`.
    #[error("check_mode must be 0–3, got {0}")]
    CheckModeOutOfRange(u8),
    /// `bribe_bips` not in `0..=10000`.
    #[error("bribe_bips must be 0–10000, got {0}")]
    BribeBipsOutOfRange(u16),
    /// `bribe_recipient_idx` not in `0..32`.
    #[error("bribe_recipient_idx must be 0–31, got {0}")]
    BribeRecipientOutOfRange(u8),
}

/// Pack the `execute()` `config` ABI parameter.
///
/// Mirrors `examples/cmd_stream.py::pack_config`. When `bribe_bips > 0`,
/// `expected_value` MUST be the real pre-tx balance — the contract computes
/// `profit = combined_after - expected_value`, so `expected_value=0` with a
/// bribe over-bribes (drains the full balance).
///
/// # Errors
///
/// Returns [`Err`] for any out-of-range field (`check_mode > 3`,
/// `bribe_bips > 10000`, `bribe_recipient_idx ≥ MAX_INDEXED_ADDRESSES (32)`).
pub fn pack_config(
    check_mode: u8,
    expected_value: U256,
    bribe_bips: u16,
    bribe_recipient_idx: u8,
) -> Result<U256, ConfigError> {
    if check_mode > MAX_CHECK_MODE {
        return Err(ConfigError::CheckModeOutOfRange(check_mode));
    }
    if bribe_bips > MAX_BRIBE_BIPS {
        return Err(ConfigError::BribeBipsOutOfRange(bribe_bips));
    }
    // `MAX_INDEXED_ADDRESSES` (32) fits in `u8`; the `<` makes the bound
    // `0..=31` (block.coinbase=0, table idxs 1..31).
    if bribe_recipient_idx as usize >= MAX_INDEXED_ADDRESSES {
        return Err(ConfigError::BribeRecipientOutOfRange(bribe_recipient_idx));
    }

    // (expected_value << 32) | (bribe_recipient_idx << 24) | (bribe_bips << 8) | check_mode
    Ok((expected_value << U256::from(32u64))
        | (U256::from(bribe_recipient_idx) << U256::from(24u64))
        | (U256::from(bribe_bips) << U256::from(8u64))
        | U256::from(check_mode))
}

/// Deprecated alias for [`pack_config`] with `bribe_bips=0` /
/// `bribe_recipient_idx=0`.
///
/// Mirrors `examples/cmd_stream.py::pack_expected_balance`. Kept for callers
/// that predate the bribe config move.
///
/// # Errors
///
/// Returns [`ConfigError::CheckModeOutOfRange`] if `check_mode > 3`.
pub fn pack_expected_balance(check_mode: u8, expected_value: U256) -> Result<U256, ConfigError> {
    pack_config(check_mode, expected_value, 0, 0)
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    #![allow(clippy::too_many_lines)]

    use super::*;

    #[test]
    fn pack_default() {
        // expected_value=0, all-zero fields → 0.
        assert_eq!(pack_config(0, U256::ZERO, 0, 0).unwrap(), U256::ZERO);
    }

    #[test]
    fn pack_check_mode_only() {
        // check_mode=1, everything else zero → 1.
        assert_eq!(pack_config(1, U256::ZERO, 0, 0).unwrap(), U256::from(1u64));
    }

    #[test]
    fn pack_bribe_bips_in_byte_2_3() {
        // bribe_bips=100 → (100 << 8) = 25600.
        assert_eq!(
            pack_config(0, U256::ZERO, 100, 0).unwrap(),
            U256::from(25_600u64)
        );
    }

    #[test]
    fn pack_recipient_in_byte_3() {
        // recipient_idx=5 → (5 << 24) = 83886080.
        assert_eq!(
            pack_config(0, U256::ZERO, 0, 5).unwrap(),
            U256::from(83_886_080u64)
        );
    }

    #[test]
    fn pack_expected_value_high_bits() {
        // expected_value=0x12345 → shifted left 32 bits.
        let v = U256::from(0x12345u64);
        let packed = pack_config(0, v, 0, 0).unwrap();
        assert_eq!(packed, v << U256::from(32u64));
    }

    #[test]
    fn pack_all_fields_compose() {
        // check_mode=2, bribe_bips=500, recipient=3, expected=0xABC
        let expected = U256::from(0xABCu64);
        let expected_shifted = expected << U256::from(32u64);
        let bribe = U256::from(500u64) << U256::from(8u64);
        let recip = U256::from(3u64) << U256::from(24u64);
        let mode = U256::from(2u64);
        let want = expected_shifted | recip | bribe | mode;
        assert_eq!(pack_config(2, expected, 500, 3).unwrap(), want);
    }

    #[test]
    fn pack_expected_balance_alias_matches_zero_bribe() {
        let expected = U256::from(0xDEAD_BEEFu64);
        assert_eq!(
            pack_expected_balance(1, expected).unwrap(),
            pack_config(1, expected, 0, 0).unwrap(),
        );
    }

    #[test]
    fn reject_check_mode_out_of_range() {
        assert_eq!(pack_config(3, U256::ZERO, 0, 0), Ok(U256::from(3u64)));
        assert_eq!(
            pack_config(4, U256::ZERO, 0, 0),
            Err(ConfigError::CheckModeOutOfRange(4))
        );
    }

    #[test]
    fn reject_bribe_bips_out_of_range() {
        assert_eq!(
            pack_config(0, U256::ZERO, 10_001, 0),
            Err(ConfigError::BribeBipsOutOfRange(10_001)),
        );
    }

    #[test]
    fn reject_recipient_out_of_range() {
        // 32 is the MAX_INDEXED_ADDRESSES bound (0..=31 valid).
        assert_eq!(
            pack_config(0, U256::ZERO, 0, 32),
            Err(ConfigError::BribeRecipientOutOfRange(32)),
        );
    }

    #[test]
    fn accept_recipient_at_upper_bound() {
        // 31 is the last valid table index.
        assert!(pack_config(0, U256::ZERO, 0, 31).is_ok());
    }

    #[test]
    fn accept_bribe_at_upper_bound() {
        assert!(pack_config(0, U256::ZERO, 10_000, 0).is_ok());
    }

    #[test]
    fn pack_large_expected_value_no_overflow() {
        // ~2^200 — verifies the U256 shift doesn't clobber the low fields.
        let v = U256::from(1u64) << U256::from(200u64);
        let packed = pack_config(1, v, 300, 2).unwrap();
        // Low 32 bits = (2 << 24) | (300 << 8) | 1.
        let low = (2u64 << 24) | (300u64 << 8) | 1u64;
        assert_eq!(packed & U256::from(u64::from(u32::MAX)), U256::from(low));
        assert_eq!(packed >> U256::from(32u64), v);
    }
}
