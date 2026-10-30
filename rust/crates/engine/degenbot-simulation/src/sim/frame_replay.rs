//! The frame-replay vocabulary shared by the engine's scratch seam:
//! [`ReplayableTx`], the projected block env ([`ScratchBlock`]), the
//! cold-read counter ([`FrameRpcCounter`]), the predecessor/sequence
//! outcome vocabulary ([`PredStatus`], [`BaseFeeSource`]), and the typed
//! failure taxonomy ([`ReplayFrameError`], [`SequenceReplayError`]).
//!
//! The replay engine itself lives in `sim::evm2::frame_replay`
//! (`Evm2ScratchEvm` + `Evm2CountingFrameDb`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use alloy::primitives::{Address, Bytes, U256};

use crate::sim::RunVerdict;

/// One externally-received signed transaction, recovered-signer-as-data.
/// Field set mirrors the backrun feed frame (`degenbot-rpc::backrun_feed`
/// `BackrunFeedEvent`) minus transport metadata (hash, arrival time,
/// access list) the replay does not consume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayableTx {
    /// The recovered signer (data, not an act — the caller recovers).
    pub from: Address,
    /// `None` = contract creation.
    pub to: Option<Address>,
    pub value: U256,
    pub data: Bytes,
    pub gas_limit: u64,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
    pub nonce: u64,
}

/// How one predecessor in a sequence settled. A predecessor occupies its
/// nonce even when it reverted or halted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredStatus {
    Success,
    Reverted,
    Halted,
    /// The parent state had already consumed the nonce — stale pool evidence.
    /// The predecessor's effects are already in the parent state, so it
    /// commits nothing and the sequence continues.
    AlreadyMined,
}

impl PredStatus {
    /// The trace label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Reverted => "reverted",
            Self::Halted => "halted",
            Self::AlreadyMined => "already_mined",
        }
    }
}

impl From<RunVerdict> for PredStatus {
    fn from(verdict: RunVerdict) -> Self {
        match verdict {
            RunVerdict::Success => Self::Success,
            RunVerdict::Reverted => Self::Reverted,
            RunVerdict::Halted => Self::Halted,
        }
    }
}

/// Which base-fee path validated the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseFeeSource {
    /// The projected next base fee ([`ScratchBlock::base_fee_next`])
    /// validated the frame.
    Projected,
    /// The projection disagreed; the frame settled with the base-fee check
    /// disabled.
    DisabledFallback,
}

/// A frame that could not be replayed against the layered view. The
/// variants carry the reconstructable nonce/saturation evidence, so every
/// observe label downstream is truthful about WHICH class it belonged to.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReplayFrameError {
    /// The wire frame claims a nonce AHEAD of the parent state: pending
    /// predecessors sit between the snapshot and the frame (the dominant
    /// class on today's soak, resolved by gap-prefix replay).
    #[error("gap pending: frame nonce {claimed}, parent expects {expected}")]
    GapPending { claimed: u64, expected: u64 },
    /// The frame nonce is already consumed at the parent state: the block-
    /// boundary race the live feed accumulates. Nothing to rescue; final.
    #[error("already settled: frame nonce {frame} consumed at parent nonce {parent}")]
    AlreadySettled { frame: u64, parent: u64 },
    /// The transaction is structurally unprocessable as sent (zero gas
    /// limit, intrinsic cost above gas limit, malformed chain id, ...) — no
    /// chain state could ever admit it.
    #[error("malformed transaction: {raw}")]
    MalformedTransaction { raw: std::string::String },
    /// Anything else — RPC hydrate failure, validation, or an unexpected
    /// engine variant.
    #[error("replay failed: {raw}")]
    Other { raw: std::string::String },
    /// The projected base fee rejected the tx AND the disabled-base-fee
    /// retry rejected it too: no chain state admits this gas price now, but
    /// the owner may replace it with a repriced tx — transient, never
    /// malformed-terminal and never structural-permanent.
    #[error("mispriced transaction: {raw}")]
    Mispriced { raw: std::string::String },
}

/// A sequence replay's typed failure, split by which side of the sequence
/// aborted. The rescue router must never mistake a predecessor's death for
/// the frame's: a predecessor that cannot run as fetched is pool-pred lane
/// evidence (the owner may replace it), not proof the frame is dead.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SequenceReplayError {
    /// A prefix predecessor failed before the frame ran. The frame is
    /// untouched — its quarantine life cannot be resolved on this evidence.
    #[error("sequence predecessor at nonce {nonce} failed: {source}")]
    Predecessor {
        /// The offending predecessor's claimed nonce.
        nonce: u64,
        /// The predecessor's typed failure class.
        #[source]
        source: ReplayFrameError,
    },
    /// The frame itself failed after the prefix settled cleanly.
    #[error("frame: {0}")]
    Frame(ReplayFrameError),
}

/// The projected block env a scratch frame executes under. `base_fee_next`
/// is the projected next base fee in wei per gas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScratchBlock {
    pub number: u64,
    pub timestamp: u64,
    pub base_fee_next: u128,
}

/// Cold-read counter for a scratch stack's cold path — embed it via the
/// counting bridge and hand the same handle to the scratch engine.
#[derive(Debug, Default)]
pub struct FrameRpcCounter {
    basic: AtomicU64,
    storage: AtomicU64,
}

impl FrameRpcCounter {
    /// Total forwarded cold reads (basic + storage).
    #[must_use]
    pub fn reads(&self) -> u64 {
        self.basic.load(Ordering::Relaxed) + self.storage.load(Ordering::Relaxed)
    }

    /// Count one forwarded `basic` read (the counting bridge).
    pub(crate) fn count_basic(&self) {
        self.basic.fetch_add(1, Ordering::Relaxed);
    }

    /// Count one forwarded `storage` read (the counting bridge).
    pub(crate) fn count_storage(&self) {
        self.storage.fetch_add(1, Ordering::Relaxed);
    }
}

/// The shared handle type (an [`Arc`] around the counter).
pub type SharedFrameRpcCounter = Arc<FrameRpcCounter>;
