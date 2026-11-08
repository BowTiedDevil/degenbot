//! The `execute(commands, config)` ABI `config` `uint256` packing.
//!
//! Packs into the `cmd_executor.vy` `execute` config-bitfield layout:
//!
//! - bits `0–7`:   `profit_mode` (1=WETH+ETH, 2=ERC6909 WETH)
//! - bits `8–23`:  `bribe_bips` (0=none, 1–10000=basis points)
//! - bits `24–31`: `bribe_recipient_idx` (0=block.coinbase, 1–31=table idx)
//! - bits `32–255`: `min_profit` (minimum gross increase in the selected mode)
//!
//! Bribes moved out of the command stream (opcodes 0x02/0x03 reserved) into
//! this parameter; pass the result as the 2nd arg to `execute(commands, config)`.

use alloy::primitives::U256;

use crate::encoders::MAX_INDEXED_ADDRESSES;

/// Max bribe in basis points (100% = 10000 bips).
pub const MAX_BRIBE_BIPS: u16 = 10_000;

/// Valid range of measured balance-domain selectors.
pub const MIN_PROFIT_MODE: u8 = 1;
pub const MAX_PROFIT_MODE: u8 = 2;

/// Error raised by an out-of-range config field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// `profit_mode` is not physical (1) or ERC6909 (2).
    #[error("profit_mode must be 1 or 2, got {0}")]
    ProfitModeOutOfRange(u8),
    /// `bribe_bips` not in `0..=10000`.
    #[error("bribe_bips must be 0–10000, got {0}")]
    BribeBipsOutOfRange(u16),
    /// `bribe_recipient_idx` not in `0..32`.
    #[error("bribe_recipient_idx must be 0–31, got {0}")]
    BribeRecipientOutOfRange(u8),
}

/// Pack the `execute()` `config` ABI parameter.
///
/// `min_profit` is enforced against the gross balance-domain increase before
/// bribe payment. Gas and other off-chain fees are not part of this word.
///
/// # Errors
///
/// Returns [`Err`] for any out-of-range field (`profit_mode` not 1 or 2,
/// `bribe_bips > 10000`, `bribe_recipient_idx ≥ MAX_INDEXED_ADDRESSES (32)`).
pub fn pack_config(
    profit_mode: u8,
    min_profit: U256,
    bribe_bips: u16,
    bribe_recipient_idx: u8,
) -> Result<U256, ConfigError> {
    if !(MIN_PROFIT_MODE..=MAX_PROFIT_MODE).contains(&profit_mode) {
        return Err(ConfigError::ProfitModeOutOfRange(profit_mode));
    }
    if bribe_bips > MAX_BRIBE_BIPS {
        return Err(ConfigError::BribeBipsOutOfRange(bribe_bips));
    }
    // `MAX_INDEXED_ADDRESSES` (32) fits in `u8`; the `<` makes the bound
    // `0..=31` (block.coinbase=0, table idxs 1..31).
    if bribe_recipient_idx as usize >= MAX_INDEXED_ADDRESSES {
        return Err(ConfigError::BribeRecipientOutOfRange(bribe_recipient_idx));
    }

    // (min_profit << 32) | (bribe_recipient_idx << 24) | (bribe_bips << 8) | profit_mode
    Ok((min_profit << U256::from(32u64))
        | (U256::from(bribe_recipient_idx) << U256::from(24u64))
        | (U256::from(bribe_bips) << U256::from(8u64))
        | U256::from(profit_mode))
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn pack_default() {
        assert_eq!(pack_config(1, U256::ZERO, 0, 0).unwrap(), U256::from(1u64));
    }

    #[test]
    fn pack_bribe_bips_in_byte_2_3() {
        assert_eq!(
            pack_config(1, U256::ZERO, 100, 0).unwrap(),
            U256::from(25_601u64)
        );
    }

    #[test]
    fn pack_recipient_in_byte_3() {
        assert_eq!(
            pack_config(1, U256::ZERO, 0, 5).unwrap(),
            U256::from(83_886_081u64)
        );
    }

    #[test]
    fn pack_min_profit_high_bits() {
        let min_profit = U256::from(0x12345u64);
        let packed = pack_config(1, min_profit, 0, 0).unwrap();
        assert_eq!(packed, (min_profit << U256::from(32u64)) | U256::from(1u64));
    }

    #[test]
    fn pack_all_fields_compose() {
        let min_profit = U256::from(0xABCu64);
        let want = (min_profit << U256::from(32u64))
            | (U256::from(3u64) << U256::from(24u64))
            | (U256::from(500u64) << U256::from(8u64))
            | U256::from(2u64);
        assert_eq!(pack_config(2, min_profit, 500, 3).unwrap(), want);
    }

    #[test]
    fn reject_profit_mode_out_of_range() {
        for mode in [0, 3, 4, u8::MAX] {
            assert_eq!(
                pack_config(mode, U256::ZERO, 0, 0),
                Err(ConfigError::ProfitModeOutOfRange(mode))
            );
        }
    }

    #[test]
    fn reject_bribe_bips_out_of_range() {
        assert_eq!(
            pack_config(1, U256::ZERO, 10_001, 0),
            Err(ConfigError::BribeBipsOutOfRange(10_001)),
        );
    }

    #[test]
    fn reject_recipient_out_of_range() {
        assert_eq!(
            pack_config(1, U256::ZERO, 0, 32),
            Err(ConfigError::BribeRecipientOutOfRange(32)),
        );
    }

    #[test]
    fn accept_recipient_at_upper_bound() {
        assert!(pack_config(1, U256::ZERO, 0, 31).is_ok());
    }

    #[test]
    fn accept_bribe_at_upper_bound() {
        assert!(pack_config(1, U256::ZERO, 10_000, 0).is_ok());
    }

    #[test]
    fn pack_large_min_profit_no_overflow() {
        let min_profit = U256::from(1u64) << U256::from(200u64);
        let packed = pack_config(1, min_profit, 300, 2).unwrap();
        let low = (2u64 << 24) | (300u64 << 8) | 1u64;
        assert_eq!(packed & U256::from(u64::from(u32::MAX)), U256::from(low));
        assert_eq!(packed >> U256::from(32u64), min_profit);
    }
}
