//! The event surface this crate emits into the stage-machine runtime.
//!
//! `WsEvent` (the `block_pump`'s merged-stream enum) lived here in all but name
//! since the bot-core extraction: a merged `newHeads` + `logs` stream whose log
//! arm is now the structured [`PoolEvent`] `{ epoch, log_index, payload }`
//! instead of a bare alloy `Log` — the emission shape ingestion promises to
//! the runtime (and to any pure-Rust consumer that subscribes upstream of
//! the stage machine).

use alloy::rpc::types::Log;

/// A decoded-agnostic pool-relevant log event, emitted by the ingestion
/// transport into the runtime.
///
/// `epoch` / `log_index` duplicate what the raw payload also carries so the
/// runtime (and standalone consumers) can order + drop events without
/// touching the payload, and so the eventual non-`Log` transports keep the
/// same surface.
#[derive(Clone, Debug)]
pub struct PoolEvent {
    /// The block epoch the event belongs to (its mined block number).
    /// The runtime's stage machine owns the *cursor* epoch semantics
    /// (reorg rewind bumps the sequence); a `removed: true` log's epoch is
    /// the block it unwinds.
    pub epoch: u64,
    /// On-chain log index (`None` when the node omits it — pending-ish
    /// deliveries).
    pub log_index: Option<u64>,
    /// The raw log payload (the decoders' input).
    pub payload: Log,
}

impl PoolEvent {
    /// Canonicalize a WS/backfill log into the emission shape.
    #[must_use]
    pub fn from_log(log: Log) -> Self {
        Self {
            epoch: log.block_number.unwrap_or(0),
            log_index: log.log_index,
            payload: log,
        }
    }
}

/// Events from the merged WS subscriptions — the ingestion→runtime input.
#[derive(Clone, Debug)]
pub enum IngestEvent {
    /// A new block header arrived.
    BlockHeader {
        /// Block number.
        number: u64,
        /// Block timestamp (seconds).
        timestamp: u64,
        /// Base fee per gas (`None` pre-EIP-1559).
        base_fee_per_gas: Option<u64>,
        /// Gas used by the block.
        gas_used: u64,
        /// Gas limit of the block.
        gas_limit: u64,
    },
    /// A log event arrived from the logs subscription.
    Pool(PoolEvent),
}
