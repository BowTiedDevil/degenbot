//! Trait for buffered liquidity events that support expiry by block number.
//!
//! **Relocated** from `degenbot-bot/src/solvers/liquidity_event_buffer.rs`
//! (a pure value trait, previously mis-homed under `solvers/`); re-exported
//! there at the historical `crate::solvers::liquidity_event_buffer::LiquidityEvent`
//! path. Transient re-export — repointed natively by USPN7M/P2CKRL.

/// Trait for buffered liquidity events that support expiry by block number.
pub trait LiquidityEvent {
    /// The block number at which this event occurred.
    fn block_number(&self) -> u64;
}
