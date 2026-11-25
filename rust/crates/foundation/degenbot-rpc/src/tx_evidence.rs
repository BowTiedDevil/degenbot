//! Complete transaction receipts and the selective call traces that prove which
//! call emitted each receipt log (ADR-105 D2, D5).
//!
//! Filtered log acquisition names the CANDIDATE transactions of a range; it does
//! not state what else the transaction did. A projection that must attribute one
//! transaction's logs to protocol operations needs the whole answer, so it reads
//! the transaction's complete receipt ([`ReceiptTransaction`]) — every log the
//! transaction emitted, with its block-wide `logIndex` and its transaction and
//! block identity.
//!
//! Ownership that a receipt cannot prove needs the execution tree. Only
//! liquidations and other resolvers whose legs are chosen by a CALL (not by a log
//! coordinate) need it, so traces are acquired selectively: a
//! [`TracedTransaction`] is the transaction's call tree, and it is a DIFFERENT
//! type on purpose — call paths exist only here, and a receipt alone never
//! exposes one. Whether a transaction needs one is the caller's policy, not this
//! module's.
//!
//! [`join_receipt_trace`] is the total ordered join. It assigns every receipt log
//! exactly one emitting call, validating the ordered log count, address, topics,
//! and data against the complete receipt before a single `LogRef`/`CallRef` pair
//! exists. Two facts make the attribution provable rather than guessed:
//!
//! - a call's SUBTREE logs occupy ONE contiguous interval of the receipt, because
//!   the call executes uninterrupted; and
//! - each call states its own emitted logs in emission order.
//!
//! The join therefore solves the interleaving of a frame's own logs with its
//! children's intervals, and refuses when no interleaving reproduces the receipt
//! or when more than one does. A trace log's own index is the frame-local position
//! the tracer reported; it is NOT carried as identity anywhere in this module,
//! because a call-local index names a different log than the receipt's block-wide
//! one (ADR-105 D2).

use std::collections::HashMap;

use alloy::primitives::{Address, Bytes, B256};
use alloy::rpc::types::Log;
use degenbot_core::canonical_head::{CallRef, ChainAnchor, LogRef, TxRef};
use degenbot_core::errors::ProviderError;
use serde_json::{json, Value};

use crate::provider::AlloyProvider;

/// The JSON-RPC codes a node answers with when the method does not exist: a node
/// without a tracer capability states that as a method-not-found, which is a
/// statement about the node's capability rather than a transient fault (ADR-105
/// D11).
const MISSING_METHOD_CODES: [i64; 2] = [-32_601, -32_602];

/// Message fragments naming a tracer capability the node does not serve. Kept
/// narrow on purpose: matching a bare "unsupported" would turn an ordinary
/// transport or revert failure into a capability verdict the node never stated.
const MISSING_TRACER_MARKERS: [&str; 3] =
    ["unsupported tracer", "unknown tracer", "tracer not found"];

/// One transaction's COMPLETE receipt: every log it emitted, in the block's own
/// order, qualified by its exact transaction and block identity.
///
/// Construction validates that the logs really are this transaction's and really
/// were emitted in the anchor's block: a receipt log without a block hash, a
/// receipt from another block, or logs out of block order are refusals, never
/// repaired evidence. The logs keep the receipt's own block-wide `logIndex`, which
/// is the only index this type exposes; a receipt-relative enumeration would name
/// a different log and is not a position (ADR-105 D1, D2).
#[derive(Debug, Clone)]
pub struct ReceiptTransaction {
    tx: TxRef,
    logs: Vec<Log>,
}

impl ReceiptTransaction {
    /// The complete receipt `logs` of the transaction `tx`.
    ///
    /// # Errors
    ///
    /// One [`ReceiptRefusal`] per log that cannot be placed in `tx`'s own block and
    /// transaction: a missing block number, block hash, transaction hash, or log
    /// index; a block, block hash, or transaction that disagrees with `tx`; or a
    /// log index that does not strictly ascend.
    pub fn new(tx: TxRef, logs: Vec<Log>) -> Result<Self, ReceiptRefusal> {
        let anchor = tx.anchor();
        let mut previous_index: Option<u64> = None;
        for log in &logs {
            let Some(number) = log.block_number else {
                return Err(ReceiptRefusal::LogWithoutBlockNumber);
            };
            if number != anchor.number() {
                return Err(ReceiptRefusal::LogBlockNumberMismatch {
                    expected: anchor.number(),
                    actual: number,
                });
            }
            let Some(block_hash) = log.block_hash else {
                return Err(ReceiptRefusal::LogWithoutBlockHash {
                    log_index: log.log_index.unwrap_or_default(),
                });
            };
            if block_hash != anchor.hash() {
                return Err(ReceiptRefusal::LogBlockHashMismatch {
                    expected: anchor.hash(),
                    actual: block_hash,
                });
            }
            let Some(hash) = log.transaction_hash else {
                return Err(ReceiptRefusal::LogWithoutTransactionHash {
                    log_index: log.log_index.unwrap_or_default(),
                });
            };
            if hash != tx.hash() {
                return Err(ReceiptRefusal::ForeignTransactionLog {
                    expected: tx.hash(),
                    actual: hash,
                });
            }
            let Some(transaction_index) = log.transaction_index else {
                return Err(ReceiptRefusal::LogWithoutTransactionIndex {
                    log_index: log.log_index.unwrap_or_default(),
                });
            };
            if transaction_index != tx.index() {
                return Err(ReceiptRefusal::LogTransactionIndexMismatch {
                    expected: tx.index(),
                    actual: transaction_index,
                });
            }
            let Some(index) = log.log_index else {
                return Err(ReceiptRefusal::LogWithoutIndex);
            };
            if log.removed {
                return Err(ReceiptRefusal::RemovedLog { log_index: index });
            }
            if let Some(previous) = previous_index {
                if index <= previous {
                    return Err(ReceiptRefusal::LogOrder {
                        previous,
                        actual: index,
                    });
                }
            }
            previous_index = Some(index);
        }
        Ok(Self { tx, logs })
    }

    /// The complete receipt of the transaction `receipt` names, at `anchor`.
    ///
    /// This is the parse half of the acquisition seam: `eth_getTransactionReceipt`
    /// and the committed captures carry the same shape, so a recorded answer and a
    /// live one become the same evidence.
    ///
    /// # Errors
    ///
    /// [`ReceiptRefusal::MissingTransactionHash`],
    /// [`ReceiptRefusal::MissingTransactionIndex`],
    /// [`ReceiptRefusal::MissingBlockNumber`], [`ReceiptRefusal::MissingBlockHash`]
    /// when the receipt omits its own identity, [`ReceiptRefusal::MissingLogs`] when
    /// it carries no log list, [`ReceiptRefusal::MalformedLog`] when a log member is
    /// not a log, plus every [`ReceiptTransaction::new`] refusal.
    pub fn from_receipt_json(anchor: ChainAnchor, receipt: &Value) -> Result<Self, ReceiptRefusal> {
        let hash = parse_hash(&receipt["transactionHash"])
            .ok_or(ReceiptRefusal::MissingTransactionHash)?;
        let index = parse_quantity(&receipt["transactionIndex"])
            .ok_or(ReceiptRefusal::MissingTransactionIndex)?;
        let number =
            parse_quantity(&receipt["blockNumber"]).ok_or(ReceiptRefusal::MissingBlockNumber)?;
        let block_hash =
            parse_hash(&receipt["blockHash"]).ok_or(ReceiptRefusal::MissingBlockHash)?;
        if number != anchor.number() {
            return Err(ReceiptRefusal::BlockNumberMismatch {
                expected: anchor.number(),
                actual: number,
            });
        }
        if block_hash != anchor.hash() {
            return Err(ReceiptRefusal::BlockHashMismatch {
                expected: anchor.hash(),
                actual: block_hash,
            });
        }
        let tx = TxRef::new(anchor, hash, index)
            .map_err(|error| ReceiptRefusal::TransactionIdentity(error.to_string()))?;
        let entries = receipt["logs"]
            .as_array()
            .ok_or(ReceiptRefusal::MissingLogs)?;
        let mut logs = Vec::with_capacity(entries.len());
        for (position, entry) in entries.iter().enumerate() {
            logs.push(
                serde_json::from_value(entry.clone())
                    .map_err(|_| ReceiptRefusal::MalformedLog { position })?,
            );
        }
        Self::new(tx, logs)
    }

    /// The transaction whose complete receipt this is.
    #[must_use]
    pub const fn tx(&self) -> &TxRef {
        &self.tx
    }

    /// Every log of the transaction, in the block's own order.
    #[must_use]
    pub fn logs(&self) -> &[Log] {
        &self.logs
    }

    /// The exact position of each log of the receipt, in order.
    pub fn log_refs(&self) -> impl Iterator<Item = LogRef> + '_ {
        self.logs
            .iter()
            .filter_map(|log| log.log_index)
            .map(|index| LogRef::new(self.tx, index))
    }

    /// The log at `log_ref`'s exact position, when the receipt carries it.
    #[must_use]
    pub fn log(&self, log_ref: LogRef) -> Option<&Log> {
        if log_ref.tx() != &self.tx {
            return None;
        }
        self.logs
            .iter()
            .find(|log| log.log_index == Some(log_ref.index()))
    }

    /// Whether `candidate` is one of the receipt's logs: the SAME block-wide index,
    /// emitter, topics, and data, AND the same block and transaction identity.
    ///
    /// This is how a filtered-acquisition log is proved to be a real member of the
    /// complete receipt instead of an independently normalized one. Index and content
    /// are not enough on their own — the same content emitted by another transaction,
    /// or reported in another block, is a different log — and a candidate that states
    /// no block, no block hash, no transaction index, or is removed is not the canonical
    /// log the receipt states.
    #[must_use]
    pub fn contains(&self, candidate: &Log) -> bool {
        let Some(index) = candidate.log_index else {
            return false;
        };
        let Some(number) = candidate.block_number else {
            return false;
        };
        let Some(block_hash) = candidate.block_hash else {
            return false;
        };
        let Some(transaction_hash) = candidate.transaction_hash else {
            return false;
        };
        let Some(transaction_index) = candidate.transaction_index else {
            return false;
        };
        if number != self.tx.anchor().number()
            || block_hash != self.tx.anchor().hash()
            || transaction_hash != self.tx.hash()
            || transaction_index != self.tx.index()
            || candidate.removed
        {
            return false;
        }
        self.logs.iter().any(|log| {
            log.log_index == Some(index)
                && log.address() == candidate.address()
                && log.topics() == candidate.topics()
                && log.data().data == candidate.data().data
        })
    }
}

/// Why a complete receipt could not be built: identity the receipt states
/// differently from the transaction it claims to describe, or does not state at
/// all.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReceiptRefusal {
    /// The receipt names no transaction hash, so it has no transaction identity.
    #[error("the receipt names no transaction hash")]
    MissingTransactionHash,
    /// The receipt names no transaction index, so its transaction has no position
    /// in the block.
    #[error("the receipt names no transaction index")]
    MissingTransactionIndex,
    /// The receipt names no block number.
    #[error("the receipt names no block number")]
    MissingBlockNumber,
    /// The receipt names no block hash: a height alone cannot distinguish a
    /// same-height branch.
    #[error("the receipt names no block hash")]
    MissingBlockHash,
    /// The receipt carries no `logs` member at all: a receipt whose log list cannot
    /// be read is not a complete receipt.
    #[error("the receipt carries no logs")]
    MissingLogs,
    /// A `logs` member is not a log.
    #[error("receipt log {position} is not a log")]
    MalformedLog {
        /// The member's position in the receipt's log array.
        position: usize,
    },
    /// The receipt's block number is not the anchor's.
    #[error("the receipt names block {actual}, but the anchor is block {expected}")]
    BlockNumberMismatch {
        /// The anchor's height.
        expected: u64,
        /// The height the receipt states.
        actual: u64,
    },
    /// The receipt's block hash is not the anchor's: the evidence belongs to a
    /// different block than the one the caller is filling.
    #[error("the receipt names block hash {actual}, but the anchor hashes to {expected}")]
    BlockHashMismatch {
        /// The anchor's own hash.
        expected: B256,
        /// The hash the receipt states.
        actual: B256,
    },
    /// The transaction's identity could not be built from the receipt's own fields.
    #[error("the receipt does not name a transaction position: {0}")]
    TransactionIdentity(String),
    /// A log names no block number.
    #[error("a receipt log names no block number")]
    LogWithoutBlockNumber,
    /// A log names no block hash.
    #[error("the receipt log at index {log_index} names no block hash")]
    LogWithoutBlockHash {
        /// The index the log claims, when it claims one.
        log_index: u64,
    },
    /// A log names no transaction hash.
    #[error("the receipt log at index {log_index} names no transaction hash")]
    LogWithoutTransactionHash {
        /// The index the log claims, when it claims one.
        log_index: u64,
    },
    /// A log names no index, so it has no block-wide position.
    #[error("a receipt log names no log index")]
    LogWithoutIndex,
    /// The log is reported as removed: it describes a log the chain no longer states,
    /// so it is not canonical evidence and cannot hold a position.
    #[error("the receipt log at index {log_index} is removed")]
    RemovedLog {
        /// The index the removed log claims.
        log_index: u64,
    },
    /// A log names no transaction index, so it does not state which transaction of the
    /// block it belongs to.
    #[error("the receipt log at index {log_index} names no transaction index")]
    LogWithoutTransactionIndex {
        /// The index the log claims, when it claims one.
        log_index: u64,
    },
    /// A log's transaction index is not the receipt's.
    #[error("a receipt log claims transaction index {actual}, but the receipt is {expected}")]
    LogTransactionIndexMismatch {
        /// The receipt's transaction index.
        expected: u64,
        /// The index the log states.
        actual: u64,
    },
    /// A log's height is not the receipt's block.
    #[error("a receipt log names block {actual}, but the receipt is block {expected}")]
    LogBlockNumberMismatch {
        /// The receipt's height.
        expected: u64,
        /// The height the log states.
        actual: u64,
    },
    /// A log's block hash is not the receipt's block.
    #[error("a receipt log names block hash {actual}, but the receipt is {expected}")]
    LogBlockHashMismatch {
        /// The receipt block's own hash.
        expected: B256,
        /// The hash the log states.
        actual: B256,
    },
    /// A log belongs to another transaction than the receipt's.
    #[error("a receipt log belongs to transaction {actual}, but the receipt is {expected}")]
    ForeignTransactionLog {
        /// The receipt's transaction hash.
        expected: B256,
        /// The hash the log states.
        actual: B256,
    },
    /// The logs do not ascend by block-wide index: the receipt's own order is
    /// contradictory evidence.
    #[error("receipt log index {actual} does not follow {previous}")]
    LogOrder {
        /// The index of the log before it.
        previous: u64,
        /// The offending index.
        actual: u64,
    },
}

/// One log as a CALL states it: emitter, topics, and data — and deliberately NO
/// position.
///
/// A tracer reports each log with the frame-local index its own tooling uses. That
/// index names a position inside the call, not the block-wide `logIndex` the
/// receipt states, so it is dropped at the parse boundary: a value that cannot
/// reach a [`LogRef`] cannot be mistaken for one (ADR-105 D2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceLog {
    /// The emitting contract.
    pub address: Address,
    /// The indexed topics, in order.
    pub topics: Vec<B256>,
    /// The unindexed data.
    pub data: Bytes,
}

/// The call type a frame executed as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    /// `CALL` — a call transferring value and running the callee's code.
    Call,
    /// `STATICCALL` — a call that cannot mutate state.
    StaticCall,
    /// `DELEGATECALL` — the callee's code in the caller's storage context.
    DelegateCall,
    /// `CALLCODE` — the legacy delegate form.
    CallCode,
    /// `CREATE` — contract creation.
    Create,
    /// `CREATE2` — deterministic contract creation.
    Create2,
}

impl CallKind {
    /// The tracer's own type string, as the acquirer states it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Call => "CALL",
            Self::StaticCall => "STATICCALL",
            Self::DelegateCall => "DELEGATECALL",
            Self::CallCode => "CALLCODE",
            Self::Create => "CREATE",
            Self::Create2 => "CREATE2",
        }
    }
}

/// One call of a transaction's execution tree, with the logs it emitted itself.
///
/// `call` names the frame by its trace path, so the ROOT frame (the empty path)
/// is a valid, ordinary call that can emit logs of its own. `children` are the
/// frame's sub-calls in execution order. The frame's own logs are its own; logs
/// emitted by its children are the children's, which is what makes a per-call
/// account possible at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceCall {
    call: CallRef,
    kind: CallKind,
    target: Option<Address>,
    logs: Vec<TraceLog>,
    children: Vec<TraceCall>,
}

impl TraceCall {
    /// The frame's exact call identity.
    #[must_use]
    pub const fn call(&self) -> &CallRef {
        &self.call
    }

    /// The call type the frame executed as.
    #[must_use]
    pub const fn kind(&self) -> CallKind {
        self.kind
    }

    /// The callee, when the trace states one (a creation frame may not).
    #[must_use]
    pub const fn target(&self) -> Option<Address> {
        self.target
    }

    /// The logs this frame emitted itself, in emission order.
    #[must_use]
    pub fn logs(&self) -> &[TraceLog] {
        &self.logs
    }

    /// The frame's sub-calls, in execution order.
    #[must_use]
    pub fn children(&self) -> &[TraceCall] {
        &self.children
    }
}

/// One transaction's call tree: the trace-backed evidence a receipt cannot state.
///
/// Only this type exposes call paths. A caller holding a [`ReceiptTransaction`]
/// alone has no way to name a call, which is exactly the distinction
/// trace-required operations need (ADR-105 D2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TracedTransaction {
    tx: TxRef,
    root: TraceCall,
}

impl TracedTransaction {
    /// The call tree `trace` describes, for the transaction `tx`.
    ///
    /// This is the parse half of trace acquisition: a `callTracer` answer with
    /// `withLog` enabled and a committed capture carry the same shape.
    ///
    /// # Errors
    ///
    /// [`TraceRefusal::MissingCallKind`] / [`TraceRefusal::UnknownCallKind`] for a
    /// frame whose type is absent or not a call type, [`TraceRefusal::MissingTarget`]
    /// when the frame states a target that is not an address,
    /// [`TraceRefusal::MalformedLogs`] / [`TraceRefusal::MalformedLog`] for a log
    /// list that is not a log list, and [`TraceRefusal::MissingLogAddress`] /
    /// [`TraceRefusal::MissingLogTopics`] / [`TraceRefusal::MissingLogData`] for a log
    /// missing one of its own facts.
    pub fn from_call_tracer_json(tx: TxRef, trace: &Value) -> Result<Self, TraceRefusal> {
        let root = parse_call(tx, trace, Vec::new())?;
        Ok(Self { tx, root })
    }

    /// The transaction whose execution tree this is.
    #[must_use]
    pub const fn tx(&self) -> &TxRef {
        &self.tx
    }

    /// The transaction's root call.
    #[must_use]
    pub const fn root(&self) -> &TraceCall {
        &self.root
    }

    /// Every call of the tree, in depth-first execution-entry order (a child
    /// follows its parent).
    #[must_use]
    pub fn calls(&self) -> Vec<&TraceCall> {
        let mut out = Vec::new();
        let mut stack = vec![&self.root];
        while let Some(call) = stack.pop() {
            out.push(call);
            for child in call.children.iter().rev() {
                stack.push(child);
            }
        }
        out
    }

    /// How many logs the transaction's execution emitted across every call.
    #[must_use]
    pub fn log_count(&self) -> usize {
        self.calls().into_iter().map(|call| call.logs.len()).sum()
    }
}

/// Why a call trace could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TraceRefusal {
    /// A frame names no call type, so it is not a call.
    #[error("a trace frame names no call type")]
    MissingCallKind,
    /// A frame's call type is not one this module can name.
    #[error("trace frame type {kind} is not a call type")]
    UnknownCallKind {
        /// The type string the tracer stated.
        kind: String,
    },
    /// A frame states a target that is not an address.
    #[error("trace frame {path:?} states a target that is not an address")]
    MissingTarget {
        /// The frame's trace path.
        path: Vec<u64>,
    },
    /// A frame's logs member is not a list.
    #[error("trace frame {path:?} carries no log list")]
    MalformedLogs {
        /// The frame's trace path.
        path: Vec<u64>,
    },
    /// A log member of a frame is not a log.
    #[error("a log of trace frame {path:?} is not a log")]
    MalformedLog {
        /// The frame's trace path.
        path: Vec<u64>,
    },
    /// A trace log names no emitter.
    #[error("a log of trace frame {path:?} names no address")]
    MissingLogAddress {
        /// The frame's trace path.
        path: Vec<u64>,
    },
    /// A trace log names no topics list.
    #[error("a log of trace frame {path:?} names no topics")]
    MissingLogTopics {
        /// The frame's trace path.
        path: Vec<u64>,
    },
    /// A trace log names no data.
    #[error("a log of trace frame {path:?} names no data")]
    MissingLogData {
        /// The frame's trace path.
        path: Vec<u64>,
    },
}

/// One receipt log with the exact position it was emitted at and the call that
/// emitted it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinedLog {
    log: Log,
    log_ref: LogRef,
    call: CallRef,
}

impl JoinedLog {
    /// The receipt log itself.
    #[must_use]
    pub const fn log(&self) -> &Log {
        &self.log
    }

    /// The log's exact position: its transaction and block-wide log index.
    #[must_use]
    pub const fn log_ref(&self) -> LogRef {
        self.log_ref
    }

    /// The call that emitted the log.
    #[must_use]
    pub const fn call(&self) -> &CallRef {
        &self.call
    }
}

/// A complete receipt joined to its call trace: every log carries both its exact
/// receipt position and the call that emitted it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinedTransaction {
    tx: TxRef,
    logs: Vec<JoinedLog>,
}

impl JoinedTransaction {
    /// The transaction the joined logs belong to.
    #[must_use]
    pub const fn tx(&self) -> &TxRef {
        &self.tx
    }

    /// Every joined log, in the receipt's own order.
    #[must_use]
    pub fn logs(&self) -> &[JoinedLog] {
        &self.logs
    }

    /// The joined log at `log_ref`'s position, when the join carries it.
    #[must_use]
    pub fn log(&self, log_ref: LogRef) -> Option<&JoinedLog> {
        if log_ref.tx() != &self.tx {
            return None;
        }
        self.logs
            .iter()
            .find(|joined| joined.log_ref.index() == log_ref.index())
    }

    /// The logs the call `call` emitted itself, in the receipt's order.
    pub fn logs_of(&self, call: &CallRef) -> impl Iterator<Item = &JoinedLog> + '_ {
        let call = call.clone();
        self.logs.iter().filter(move |joined| joined.call == call)
    }

    /// The distinct calls that emitted at least one log.
    #[must_use]
    pub fn owning_calls(&self) -> Vec<&CallRef> {
        let mut seen: Vec<&CallRef> = Vec::new();
        for joined in &self.logs {
            if !seen.contains(&&joined.call) {
                seen.push(&joined.call);
            }
        }
        seen
    }
}

/// Why a receipt and a trace could not be joined.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TraceJoinRefusal {
    /// The receipt and the trace describe different transactions — including the
    /// same hash on two different chains, which is two different identities.
    #[error(
        "the receipt is {} of block {}, but the trace is {} of block {}",
        receipt.hash(),
        receipt.anchor().number(),
        trace.hash(),
        trace.anchor().number()
    )]
    TransactionMismatch {
        /// The receipt's transaction position.
        receipt: Box<TxRef>,
        /// The trace's transaction position.
        trace: Box<TxRef>,
    },
    /// The receipt and the trace state different log counts.
    #[error("the receipt carries {receipt} logs, the trace carries {trace}")]
    LogCountMismatch {
        /// The complete receipt's log count.
        receipt: usize,
        /// The trace's log count.
        trace: usize,
    },
    /// No interleaving of the traced logs reproduces the receipt's own order, so
    /// the two captures do not describe one execution. A partial join never
    /// degrades into a receipt-only interpretation.
    #[error("{receipt} receipt logs and {trace} traced logs admit no consistent attribution")]
    LogContentMismatch {
        /// The complete receipt's log count.
        receipt: usize,
        /// The trace's log count.
        trace: usize,
    },
    /// More than one attribution reproduces the receipt exactly: the evidence does
    /// not name ONE emitting call for every log.
    #[error("{receipt} receipt logs admit more than one attribution over {trace} traced logs")]
    AmbiguousAttribution {
        /// The complete receipt's log count.
        receipt: usize,
        /// The trace's log count.
        trace: usize,
    },
}

/// Join a complete receipt to its call trace, assigning every receipt log the one
/// call that emitted it.
///
/// The join is total and ordered: the result carries every receipt log in the
/// receipt's own order, each with its block-wide [`LogRef`] and its emitting
/// [`CallRef`]. Attribution is proved by the trace's call tree — a call's subtree
/// logs are one contiguous interval of the receipt and a call's own logs are in
/// emission order — so the count, emitter, topics, and data of every position are
/// validated before any pair exists.
///
/// # Errors
///
/// [`TraceJoinRefusal::TransactionMismatch`] when the two captures describe
/// different transactions, [`TraceJoinRefusal::LogCountMismatch`] when their log
/// counts differ, [`TraceJoinRefusal::LogContentMismatch`] when the receipt order
/// cannot be reproduced from the traced logs, and
/// [`TraceJoinRefusal::AmbiguousAttribution`] when it can be reproduced more than
/// one way.
pub fn join_receipt_trace(
    receipt: &ReceiptTransaction,
    traced: &TracedTransaction,
) -> Result<JoinedTransaction, TraceJoinRefusal> {
    if receipt.tx() != traced.tx() {
        return Err(TraceJoinRefusal::TransactionMismatch {
            receipt: Box::new(*receipt.tx()),
            trace: Box::new(*traced.tx()),
        });
    }
    let counts = (receipt.logs().len(), traced.log_count());
    if counts.0 != counts.1 {
        return Err(TraceJoinRefusal::LogCountMismatch {
            receipt: counts.0,
            trace: counts.1,
        });
    }

    let layout = LogFrameLayout::of(traced.root());
    let mut solver = Solver {
        receipt: receipt.logs(),
        layout: &layout,
        solutions: HashMap::new(),
    };
    let Some(root) = layout.root else {
        return Ok(JoinedTransaction {
            tx: *receipt.tx(),
            logs: Vec::new(),
        });
    };
    match solver.count(root, 0) {
        0 => {
            return Err(TraceJoinRefusal::LogContentMismatch {
                receipt: counts.0,
                trace: counts.1,
            });
        }
        1 => {}
        _ => {
            return Err(TraceJoinRefusal::AmbiguousAttribution {
                receipt: counts.0,
                trace: counts.1,
            });
        }
    }

    let mut placements = Vec::with_capacity(counts.0);
    solver.place(root, 0, &mut placements);
    let logs = placements
        .into_iter()
        .map(|(frame, position)| {
            let log = receipt.logs()[position].clone();
            let log_ref = LogRef::new(*receipt.tx(), log.log_index.unwrap_or_default());
            JoinedLog {
                log,
                log_ref,
                call: solver.layout.frames[frame].call.clone(),
            }
        })
        .collect();
    Ok(JoinedTransaction {
        tx: *receipt.tx(),
        logs,
    })
}

/// One frame of the flattened call tree the join solves over.
struct Frame {
    call: CallRef,
    logs: Vec<TraceLog>,
    children: Vec<usize>,
    subtree_logs: usize,
}

/// The frames that can consume a receipt position, flattened in depth-first
/// entry order.
///
/// A frame whose whole subtree emitted no log consumes no receipt position, so it
/// is dropped once here instead of being skipped by every walk: the attribution
/// search and the reconstruction that replays it both read this one layout, so
/// they cannot disagree about which frames take part. A retained frame keeps the
/// [`CallRef`] the tracer stated — its ORIGINAL trace path — so dropping a frame
/// never renames its descendants, and a filtered child list keeps the surviving
/// children in execution order.
struct LogFrameLayout {
    frames: Vec<Frame>,
    root: Option<usize>,
}

impl LogFrameLayout {
    /// The layout of the tree under a root: empty when the whole tree emitted no
    /// log, so a transaction with no logs joins to no positions.
    fn of(root: &TraceCall) -> Self {
        let mut layout = Self {
            frames: Vec::new(),
            root: None,
        };
        layout.root = layout.push(root);
        layout
    }

    fn push(&mut self, call: &TraceCall) -> Option<usize> {
        let index = self.frames.len();
        self.frames.push(Frame {
            call: call.call().clone(),
            logs: call.logs().to_vec(),
            children: Vec::new(),
            subtree_logs: 0,
        });
        let mut children = Vec::with_capacity(call.children().len());
        for child in call.children() {
            if let Some(child) = self.push(child) {
                children.push(child);
            }
        }
        let subtree_logs = call.logs().len()
            + children
                .iter()
                .map(|child| self.frames[*child].subtree_logs)
                .sum::<usize>();
        if subtree_logs == 0 {
            self.frames.truncate(index);
            return None;
        }
        self.frames[index].children = children;
        self.frames[index].subtree_logs = subtree_logs;
        Some(index)
    }
}

/// The attribution search over one frame's interval.
///
/// `count(frame, start)` answers how many interleavings of the frame's own logs
/// with its children's contiguous intervals reproduce the receipt from `start`
/// onwards, capped at two: zero means the captures disagree, one is the proof the
/// join needs, and two means the evidence names no single attribution.
struct Solver<'a> {
    receipt: &'a [Log],
    layout: &'a LogFrameLayout,
    solutions: HashMap<(usize, usize), u8>,
}

/// The per-frame interval bookkeeping a walk needs: the children's cumulative log
/// counts plus, for every `(child_index, position)` state, how many attributions
/// complete it (capped at two).
struct FrameTable {
    cumulative: Vec<usize>,
    finish: Vec<Vec<u8>>,
}

impl Solver<'_> {
    fn count(&mut self, frame: usize, start: usize) -> u8 {
        if let Some(count) = self.solutions.get(&(frame, start)) {
            return *count;
        }
        let count = self
            .table(frame, start)
            .map_or(0, |table| table.finish[0][0]);
        self.solutions.insert((frame, start), count);
        count
    }

    fn table(&mut self, frame: usize, start: usize) -> Option<FrameTable> {
        let (logs_len, subtree_logs, children) = {
            let entry = &self.layout.frames[frame];
            (entry.logs.len(), entry.subtree_logs, entry.children.clone())
        };
        let end = start.checked_add(subtree_logs)?;
        if end > self.receipt.len() {
            return None;
        }
        let mut cumulative = Vec::with_capacity(children.len() + 1);
        cumulative.push(0);
        for child in &children {
            let previous = *cumulative.last()?;
            cumulative.push(previous + self.layout.frames[*child].subtree_logs);
        }
        let span = end - start + 1;
        let mut finish = vec![vec![0u8; span]; children.len() + 1];
        finish[children.len()][span - 1] = 1;
        for index in (0..=children.len()).rev() {
            for position in (start..=end).rev() {
                if index == children.len() && position == end {
                    continue;
                }
                let own = position.saturating_sub(start + cumulative[index]);
                let mut total = 0u8;
                if own < logs_len && position < end && self.matches(frame, own, position) {
                    total = finish[index][position + 1 - start];
                }
                if index < children.len() {
                    let child = children[index];
                    let next = position + self.layout.frames[child].subtree_logs;
                    if next <= end && self.count(child, position) > 0 {
                        total = cap(total + finish[index + 1][next - start]);
                    }
                }
                finish[index][position - start] = total;
            }
        }
        Some(FrameTable { cumulative, finish })
    }

    /// Whether the frame's `own`-th log is the receipt log at `position`.
    fn matches(&self, frame: usize, own: usize, position: usize) -> bool {
        let traced = &self.layout.frames[frame].logs[own];
        let receipt = &self.receipt[position];
        traced.address == receipt.address()
            && traced.topics == receipt.topics()
            && traced.data == receipt.data().data
    }

    /// Walk the unique attribution, recording `(frame, receipt position)` for every
    /// log a frame emitted itself.
    fn place(&mut self, frame: usize, start: usize, out: &mut Vec<(usize, usize)>) {
        let Some(table) = self.table(frame, start) else {
            return;
        };
        let (logs_len, children) = {
            let entry = &self.layout.frames[frame];
            (entry.logs.len(), entry.children.clone())
        };
        let end = start + self.layout.frames[frame].subtree_logs;
        let mut index = 0;
        let mut position = start;
        while position < end {
            let own = position.saturating_sub(start + table.cumulative[index]);
            let takes_own = own < logs_len
                && position < end
                && self.matches(frame, own, position)
                && table.finish[index][position + 1 - start] > 0;
            if takes_own {
                out.push((frame, position));
                position += 1;
            } else {
                let child = children[index];
                let child_start = position;
                self.place(child, child_start, out);
                position += self.layout.frames[child].subtree_logs;
                index += 1;
            }
        }
    }
}

/// Two is as far as a count needs to go: the join refuses anything but one, so the
/// exact number past two is not a fact any caller can act on.
fn cap(count: u8) -> u8 {
    count.min(2)
}

/// Parse one frame of a `callTracer` answer and its sub-calls.
fn parse_call(tx: TxRef, value: &Value, path: Vec<u64>) -> Result<TraceCall, TraceRefusal> {
    let kind = parse_kind(&value["type"]).ok_or_else(|| match value["type"].as_str() {
        Some(kind) => TraceRefusal::UnknownCallKind {
            kind: kind.to_string(),
        },
        None => TraceRefusal::MissingCallKind,
    })?;
    let target = match value.get("to") {
        None | Some(Value::Null) => None,
        Some(address) => Some(
            parse_address(address)
                .ok_or_else(|| TraceRefusal::MissingTarget { path: path.clone() })?,
        ),
    };
    let mut logs = Vec::new();
    match value.get("logs") {
        None | Some(Value::Null) => {}
        Some(Value::Array(entries)) => {
            logs.reserve(entries.len());
            for entry in entries {
                logs.push(parse_trace_log(entry, &path)?);
            }
        }
        Some(_) => return Err(TraceRefusal::MalformedLogs { path }),
    }
    let mut children = Vec::new();
    match value.get("calls") {
        None | Some(Value::Null) => {}
        Some(Value::Array(entries)) => {
            children.reserve(entries.len());
            for (index, child) in entries.iter().enumerate() {
                let mut child_path = path.clone();
                child_path.push(u64::try_from(index).unwrap_or(u64::MAX));
                children.push(parse_call(tx, child, child_path)?);
            }
        }
        Some(_) => return Err(TraceRefusal::MalformedLogs { path }),
    }
    Ok(TraceCall {
        call: CallRef::new(tx, path),
        kind,
        target,
        logs,
        children,
    })
}

/// Parse one trace log, keeping only the facts that describe the log itself.
fn parse_trace_log(value: &Value, path: &[u64]) -> Result<TraceLog, TraceRefusal> {
    let address = value
        .get("address")
        .and_then(parse_address)
        .ok_or_else(|| TraceRefusal::MissingLogAddress {
            path: path.to_vec(),
        })?;
    let topics = value
        .get("topics")
        .and_then(Value::as_array)
        .ok_or_else(|| TraceRefusal::MissingLogTopics {
            path: path.to_vec(),
        })?
        .iter()
        .map(|topic| {
            parse_hash(topic).ok_or_else(|| TraceRefusal::MissingLogTopics {
                path: path.to_vec(),
            })
        })
        .collect::<Result<Vec<B256>, _>>()?;
    let data = value
        .get("data")
        .and_then(Value::as_str)
        .and_then(|raw| raw.parse::<Bytes>().ok())
        .ok_or_else(|| TraceRefusal::MissingLogData {
            path: path.to_vec(),
        })?;
    Ok(TraceLog {
        address,
        topics,
        data,
    })
}

fn parse_kind(value: &Value) -> Option<CallKind> {
    match value.as_str()?.to_ascii_uppercase().as_str() {
        "CALL" => Some(CallKind::Call),
        "STATICCALL" => Some(CallKind::StaticCall),
        "DELEGATECALL" => Some(CallKind::DelegateCall),
        "CALLCODE" => Some(CallKind::CallCode),
        "CREATE" => Some(CallKind::Create),
        "CREATE2" => Some(CallKind::Create2),
        _ => None,
    }
}

fn parse_address(value: &Value) -> Option<Address> {
    value.as_str()?.parse::<Address>().ok()
}

fn parse_hash(value: &Value) -> Option<B256> {
    value.as_str()?.parse::<B256>().ok()
}

/// One JSON-RPC quantity, in either form the wire states it.
///
/// A node answers hex with the 0x prefix; the cassette's canonical responses state the
/// same quantity in decimal. Both name one number, so both are read — a receipt is
/// complete evidence in either form, and an answer that is neither is a missing fact
/// rather than a zero.
fn parse_quantity(value: &Value) -> Option<u64> {
    let raw = value.as_str()?;
    match raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => raw.parse::<u64>().ok(),
    }
}

/// Why a complete receipt could not be acquired.
#[derive(Debug, thiserror::Error)]
pub enum ReceiptEvidenceError {
    /// The transport or the node failed the read.
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// The node holds no receipt for the transaction: a candidate transaction with
    /// no receipt has no complete evidence, and an empty receipt would be an
    /// invented one.
    #[error("the node holds no receipt for transaction {hash}")]
    MissingReceipt {
        /// The transaction asked for.
        hash: B256,
    },
    /// The endpoint is verified to serve another chain than the transaction's
    /// anchor: a receipt of this hash on another chain is another transaction.
    #[error("the endpoint serves chain {endpoint}, but the receipt is on chain {anchor}")]
    ChainMismatch {
        /// The chain the anchor names.
        anchor: u64,
        /// The chain the endpoint is verified to serve.
        endpoint: u64,
    },
    /// The served receipt does not describe this transaction's block.
    #[error(transparent)]
    Receipt(#[from] ReceiptRefusal),
}

/// Why a call trace could not be acquired.
#[derive(Debug, thiserror::Error)]
pub enum TraceEvidenceError {
    /// The transport or the node failed the read.
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// The endpoint does not serve the trace method, or does not serve the tracer
    /// the caller asked for: no call-level evidence is available from this node,
    /// which is a capability statement rather than a transient failure (ADR-105
    /// D11).
    #[error("{method} is not served by this endpoint: {cause}")]
    Unsupported {
        /// The JSON-RPC method the node rejected.
        method: &'static str,
        /// The node's own refusal.
        cause: ProviderError,
    },
    /// The node holds no trace for the transaction.
    #[error("the node holds no trace for transaction {hash}")]
    MissingTrace {
        /// The transaction asked for.
        hash: B256,
    },
    /// The endpoint is verified to serve another chain than the transaction's
    /// anchor.
    #[error("the endpoint serves chain {endpoint}, but the trace is on chain {anchor}")]
    ChainMismatch {
        /// The chain the anchor names.
        anchor: u64,
        /// The chain the endpoint is verified to serve.
        endpoint: u64,
    },
    /// The served trace is not a call tree for this transaction.
    #[error(transparent)]
    Trace(#[from] TraceRefusal),
}

/// The `debug_traceTransaction` parameters this module asks for: the call tree
/// with each frame's own logs.
///
/// `callTracer` names frames by their nested `calls`, so a frame's path is the
/// child indices from the root; `withLog` is what makes the trace carry logs at
/// all, and without it the join would have no traced logs to attribute.
fn trace_parameters(tx: TxRef) -> Value {
    json!([
        format!("{}", tx.hash()),
        { "tracer": "callTracer", "tracerConfig": { "withLog": true } }
    ])
}

/// Whether the node's failure rejects the trace method or its tracer (a capability
/// verdict) rather than failing a particular read.
fn rejects_trace_capability(cause: &ProviderError) -> bool {
    let ProviderError::RpcError { code, message } = cause else {
        return false;
    };
    if MISSING_METHOD_CODES.contains(code) {
        return true;
    }
    let lower = message.to_lowercase();
    lower.contains("tracer")
        && MISSING_TRACER_MARKERS
            .iter()
            .any(|marker| lower.contains(marker))
}

/// Refuse an anchor whose chain the endpoint is verified to serve differently.
///
/// An UNBOUND provider (`None`) states no chain, so it is not a disagreement; the
/// receipt's own block hash is what proves the block, and a wrong-chain endpoint
/// cannot reproduce it.
fn check_endpoint_chain(provider: &AlloyProvider, anchor: &ChainAnchor) -> Result<(), (u64, u64)> {
    match provider.verified_chain_id() {
        Some(endpoint) if endpoint != anchor.chain_id() => Err((anchor.chain_id(), endpoint)),
        _ => Ok(()),
    }
}

/// Acquire the COMPLETE receipt of `tx` from `provider`.
///
/// One `eth_getTransactionReceipt` round trip; the served receipt is validated
/// against the transaction's exact anchor, so a receipt from another block, another
/// transaction, or a branch at the same height is a refusal rather than evidence
/// quietly attributed to the wrong position (ADR-105 D2).
///
/// # Errors
///
/// [`ReceiptEvidenceError::ChainMismatch`] when the endpoint is verified to serve
/// another chain, [`ReceiptEvidenceError::Provider`] when the read fails,
/// [`ReceiptEvidenceError::MissingReceipt`] when the node holds no receipt, and
/// [`ReceiptEvidenceError::Receipt`] when the receipt names a different position.
pub async fn fetch_receipt(
    provider: &AlloyProvider,
    tx: TxRef,
) -> Result<ReceiptTransaction, ReceiptEvidenceError> {
    if let Err((anchor, endpoint)) = check_endpoint_chain(provider, tx.anchor()) {
        return Err(ReceiptEvidenceError::ChainMismatch { anchor, endpoint });
    }
    let served: Value = provider
        .request_raw(
            "eth_getTransactionReceipt",
            json!([format!("{}", tx.hash())]),
            "eth_getTransactionReceipt failed",
        )
        .await?;
    if served.is_null() {
        return Err(ReceiptEvidenceError::MissingReceipt { hash: tx.hash() });
    }
    let receipt = ReceiptTransaction::from_receipt_json(*tx.anchor(), &served)?;
    if receipt.tx() != &tx {
        return Err(ReceiptEvidenceError::Receipt(
            ReceiptRefusal::TransactionIdentity(format!(
                "the node served {} at transaction index {}, not the requested transaction",
                receipt.tx().hash(),
                receipt.tx().index()
            )),
        ));
    }
    Ok(receipt)
}

/// Acquire the call trace of `tx` from `provider`.
///
/// One `debug_traceTransaction` round trip with `callTracer` + `withLog`. The
/// caller decides whether the transaction needs one: a trace-required operation
/// cannot be served by a receipt, so this is a separate acquisition rather than a
/// field of the receipt (ADR-105 D2).
///
/// # Errors
///
/// [`TraceEvidenceError::ChainMismatch`] when the endpoint is verified to serve
/// another chain, [`TraceEvidenceError::Unsupported`] when the node does not serve
/// the trace method or the tracer, [`TraceEvidenceError::Provider`] when the read
/// fails for another reason, [`TraceEvidenceError::MissingTrace`] when the node
/// holds no trace, and [`TraceEvidenceError::Trace`] when the answer is not a call
/// tree.
pub async fn fetch_trace(
    provider: &AlloyProvider,
    tx: TxRef,
) -> Result<TracedTransaction, TraceEvidenceError> {
    if let Err((anchor, endpoint)) = check_endpoint_chain(provider, tx.anchor()) {
        return Err(TraceEvidenceError::ChainMismatch { anchor, endpoint });
    }
    let served: Value = provider
        .request_raw(
            "debug_traceTransaction",
            trace_parameters(tx),
            "debug_traceTransaction failed",
        )
        .await
        .map_err(|cause| {
            if rejects_trace_capability(&cause) {
                TraceEvidenceError::Unsupported {
                    method: "debug_traceTransaction",
                    cause,
                }
            } else {
                TraceEvidenceError::Provider(cause)
            }
        })?;
    if served.is_null() {
        return Err(TraceEvidenceError::MissingTrace { hash: tx.hash() });
    }
    TracedTransaction::from_call_tracer_json(tx, &served).map_err(TraceEvidenceError::from)
}

#[cfg(test)]
mod tests;
