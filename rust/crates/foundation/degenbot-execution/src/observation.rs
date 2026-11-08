//! The typed execution observation the engine hands to Assess (ADR-098 D2).
//!
//! Simulation mechanics are the engine's: it executes the transaction, meters
//! gas, captures return data and events, and runs the adapter's declared
//! pre/post probe reads. What crosses back to the adapter is this value — the
//! evidence an adapter interprets, never a simulation handle or a free-form
//! callback.
//!
//! Evidence is reported honestly: a reading the engine could not take is
//! absent (`None`), not zero. An adapter whose assessment needs it fails with
//! [`crate::AssessError::Evidence`] instead of assessing a fabricated figure.
//!
//! Field availability:
//!
//! - `transaction` — always. The complete transaction the engine executed.
//! - `status` — always. [`ExecutionStatus::Success`] or
//!   [`ExecutionStatus::Revert`].
//! - `gas_used` — always. Gas consumed by the top-level call, un-inflated.
//! - `base_fee_next` — always. The simulated block's base fee (wei/gas), the
//!   pricing half of Assess.
//! - `return_data` — when the engine captured the call's return data. A
//!   successful call may return empty data (`Some` with zero bytes); `None`
//!   means the engine did not capture it.
//! - `probe_reads` — one entry per declared [`crate::ProbeSpec`], in the
//!   declared order, present even when a read failed (its `before` / `after`
//!   is then `None`).
//! - `events` — every event the engine captured during the call, in execution
//!   order. Empty when the call emitted none.
//!
//! Traces and access lists stay engine-owned simulation/submission mechanics;
//! they are not assessment evidence and do not cross this boundary.

use alloy::primitives::{Address, Bytes, B256, U256};

use crate::gate::ProbeSpec;
use crate::transaction::TransactionRequest;

/// How the simulated transaction terminated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExecutionStatus {
    /// The call completed without reverting.
    Success,
    /// The call reverted; `ExecutionObservation::return_data` carries the
    /// revert payload when the engine captured it.
    Revert,
}

/// One declared probe read, before and after the call.
///
/// `before` / `after` are `None` when the engine's read failed at that edge —
/// an unavailable observation, never a zero balance.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProbeReading {
    /// The declared read this observation answers.
    pub spec: ProbeSpec,
    /// The pre-call read, when available.
    pub before: Option<U256>,
    /// The post-call read, when available.
    pub after: Option<U256>,
}

impl ProbeReading {
    /// Pair a declared probe spec with the two reads the engine took.
    #[must_use]
    pub const fn new(spec: ProbeSpec, before: Option<U256>, after: Option<U256>) -> Self {
        Self {
            spec,
            before,
            after,
        }
    }

    /// The signed change (`after − before`) this reading observed.
    ///
    /// `None` when either edge is unavailable or the pair does not fit a signed
    /// 128-bit integer — the delta the built-in sum-of-deltas shape consumes.
    #[must_use]
    pub fn delta(&self) -> Option<i128> {
        let before = u128::try_from(self.before?).ok()?;
        let after = u128::try_from(self.after?).ok()?;
        let before = i128::try_from(before).ok()?;
        let after = i128::try_from(after).ok()?;
        after.checked_sub(before)
    }
}

/// One event the engine captured during the call.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CapturedEvent {
    /// The emitting contract.
    pub address: Address,
    /// The indexed topics; `topics[0]` is the event signature hash.
    pub topics: Vec<B256>,
    /// The non-indexed data payload.
    pub data: Bytes,
    /// The log's index within the transaction.
    pub log_index: u32,
}

/// The engine's typed observation of one simulated transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionObservation {
    /// The complete transaction the engine executed.
    pub transaction: TransactionRequest,
    /// How the call terminated.
    pub status: ExecutionStatus,
    /// Gas consumed by the top-level call, un-inflated.
    pub gas_used: u64,
    /// The simulated block's base fee (wei/gas).
    pub base_fee_next: u128,
    /// The call's return data, when the engine captured it.
    pub return_data: Option<Bytes>,
    /// One reading per declared probe, in declared order.
    pub probe_reads: Vec<ProbeReading>,
    /// Events captured during the call, in execution order.
    pub events: Vec<CapturedEvent>,
}

impl ExecutionObservation {
    /// Start an observation from the facts every simulated transaction has.
    ///
    /// Evidence that may be absent (return data, probe reads, events) starts
    /// empty; the engine adds it as it captures it.
    #[must_use]
    pub const fn new(
        transaction: TransactionRequest,
        status: ExecutionStatus,
        gas_used: u64,
        base_fee_next: u128,
    ) -> Self {
        Self {
            transaction,
            status,
            gas_used,
            base_fee_next,
            return_data: None,
            probe_reads: Vec::new(),
            events: Vec::new(),
        }
    }

    /// Attach the call's captured return data.
    #[must_use]
    pub fn with_return_data(mut self, return_data: Bytes) -> Self {
        self.return_data = Some(return_data);
        self
    }

    /// Attach the declared probe readings, in declared order.
    #[must_use]
    pub fn with_probe_reads(mut self, probe_reads: Vec<ProbeReading>) -> Self {
        self.probe_reads = probe_reads;
        self
    }

    /// Attach the captured events, in execution order.
    #[must_use]
    pub fn with_events(mut self, events: Vec<CapturedEvent>) -> Self {
        self.events = events;
        self
    }

    /// Whether the simulated call completed without reverting.
    #[must_use]
    pub const fn succeeded(&self) -> bool {
        matches!(self.status, ExecutionStatus::Success)
    }

    /// The first 32-byte word of the captured return data, if it carries one —
    /// the shape a contract that reports its own figure returns.
    #[must_use]
    pub fn first_return_word(&self) -> Option<U256> {
        let data = self.return_data.as_ref()?;
        let word = data.get(..32)?;
        Some(U256::from_be_slice(word))
    }
}
