//! The forwarded-header fact both consumption halves read.
//!
//! `newHeads` ticks are chain facts, not engine business: the pump's driver
//! publishes ONE tick per accepted header on the session's ordered stream, and
//! the consumer's block clock reads it there. This module owns the value; the
//! session's event stream owns its delivery.

/// Forwarded `newHeads` tick — the authoritative block clock.
///
/// Distinct from a result batch (which carries solve results + the solve
/// block as metadata): the consumer derives its block clock from
/// `BlockNotification`s pushed by the driver on every new header, NOT from
/// the batch's solve block. The solve block lags by the send debounce + only
/// advances when a batch is actually sent, so using it as the clock makes
/// the bot's `[block: N]` freeze behind the driver's `current_block`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockNotification {
    /// The block number (the clock field).
    pub number: u64,
    /// Block timestamp.
    pub timestamp: u64,
    /// Base fee per gas (None for pre-EIP-1559 blocks).
    pub base_fee_per_gas: Option<u64>,
    /// Gas used in this block.
    pub gas_used: u64,
    /// Gas limit of this block.
    pub gas_limit: u64,
}
