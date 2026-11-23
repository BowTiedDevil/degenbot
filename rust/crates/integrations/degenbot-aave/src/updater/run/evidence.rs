//! Candidate-transaction evidence acquisition: every filtered group's complete
//! receipt, plus its call trace only where a parse leg is named by a CALL
//! (ADR-105 D2, D5).
//!
//! Filtered log acquisition names the chunk's candidate transactions; it does
//! not state what else each transaction did. This stage acquires the missing
//! half per candidate, BEFORE the chunk's transaction opens: the block's exact
//! header (a transaction reference is anchored, ADR-105 D1), the transaction's
//! complete receipt, and — only when `EvidenceRequirement` says a parse leg is
//! named by a call — its call trace. A receipt-only transaction pays for no
//! trace, and a transaction that needs one is refused rather than parsed on
//! insufficient evidence.
//!
//! Acquisition runs outside the chunk's write lock on purpose: the reads are
//! RPC, the refusal classes are evidence classes, and neither may hold the
//! database while it happens (ADR-105 D11). A refusal here leaves the cursor and
//! the store exactly as they were — no chunk transaction has opened.

use std::collections::BTreeMap;

use alloy::primitives::B256;
use degenbot_core::canonical_head::ChainAnchor;
use degenbot_core::errors::ProviderError;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_rpc::tx_evidence::{
    fetch_receipt, fetch_trace, ReceiptEvidenceError, TraceEvidenceError,
};

use crate::updater::evidence::{
    bind_transaction_evidence, EvidenceRefusal, EvidenceRequirement, TransactionEvidence,
};

use super::process::TxGroup;
use crate::updater::aave_fetch::LogIdentityRefusal;

/// One candidate transaction: its filtered group and the bound evidence its
/// consumers read.
pub(super) struct CandidateTransaction<'a> {
    pub(super) group: TxGroup<'a>,
    pub(super) evidence: TransactionEvidence,
}

/// Why a chunk's candidate transactions could not be evidence-bound.
///
/// The variants keep ADR-105 D11's failure classes apart: a transport fault can
/// be retried, an endpoint without trace capability cannot be retried into
/// having one, and evidence that contradicts itself is never retried at all.
#[derive(Debug, thiserror::Error)]
pub enum EvidenceAcquisitionError {
    /// The transport or the node failed a header read.
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// The node holds no complete header at a height a candidate transaction was
    /// emitted in, so the transaction has no exact block identity to bind to.
    #[error("candidate block {number} has no complete header")]
    MissingHeader {
        /// The height the candidate transaction was emitted at.
        number: u64,
    },
    /// The endpoint is verified to serve another chain than the market's: a
    /// receipt of this shape on another chain is another transaction.
    #[error("candidate block {number} is on chain {served}, not the market's chain {expected}")]
    CrossChainBlock {
        /// The height whose header named another chain.
        number: u64,
        /// The chain the market lives on.
        expected: u64,
        /// The chain the endpoint serves.
        served: u64,
    },
    /// The group's own coordinates contradict the block's header: a group lifted
    /// from another block, or a same-height branch.
    #[error(
        "candidate transaction {transaction} is not a transaction of block {block_number}: {source}"
    )]
    Identity {
        /// The transaction the filtered logs name.
        transaction: B256,
        /// The height the filtered logs claim.
        block_number: u64,
        /// The identity refusal (boxed: the refusal is the error path's payload,
        /// not its hot value).
        #[source]
        source: Box<LogIdentityRefusal>,
    },
    /// The transaction's complete receipt could not be acquired, or is not this
    /// transaction's receipt.
    #[error(transparent)]
    Receipt(#[from] ReceiptEvidenceError),
    /// The transaction's call trace could not be acquired, or the endpoint does
    /// not serve one.
    #[error(transparent)]
    Trace(#[from] TraceEvidenceError),
    /// The receipt and the filtered logs, or the receipt and its trace, do not
    /// describe one execution.
    #[error(transparent)]
    Binding(#[from] EvidenceRefusal),
}

/// Bind every candidate transaction of a chunk to its complete receipt and, where
/// its operations need one, to its call trace.
///
/// The blocks are resolved first (one header read per distinct candidate block;
/// the run's already-resolved terminal anchor is reused for its own height), then
/// each group in filtered order acquires its receipt and — only for a
/// `TraceBacked` requirement — its trace, and binds them through
/// `bind_transaction_evidence`.
///
/// # Errors
///
/// One `EvidenceAcquisitionError` per candidate transaction that cannot be
/// evidence-bound: a header the node does not state, a header on another chain, a
/// group whose coordinates contradict the header, a receipt or trace the node
/// cannot serve, or a binding refusal. Every one of them is raised before the
/// chunk's transaction opens.
pub(super) async fn bind_chunk_evidence<'a>(
    provider: &AlloyProvider,
    chain_id: u64,
    terminal: ChainAnchor,
    tx_groups: Vec<TxGroup<'a>>,
) -> Result<Vec<CandidateTransaction<'a>>, EvidenceAcquisitionError> {
    let anchors = candidate_block_anchors(provider, chain_id, terminal, &tx_groups).await?;
    let mut candidates = Vec::with_capacity(tx_groups.len());
    for group in tx_groups {
        // The block map is keyed by every group's own height, so the lookup states
        // an invariant of this function rather than a possibility.
        let anchor = anchors[&group.block_number];
        let tx = group
            .tx_ref(anchor)
            .map_err(|source| EvidenceAcquisitionError::Identity {
                transaction: B256::from(group.tx_hash),
                block_number: group.block_number,
                source: Box::new(source),
            })?;
        let receipt = fetch_receipt(provider, tx).await?;
        let traced = match EvidenceRequirement::of(&group.logs) {
            EvidenceRequirement::ReceiptOnly => None,
            EvidenceRequirement::TraceBacked => Some(fetch_trace(provider, tx).await?),
        };
        let evidence = bind_transaction_evidence(&group.logs, receipt, traced)?;
        candidates.push(CandidateTransaction { group, evidence });
    }
    Ok(candidates)
}

/// The exact anchor of every block a candidate transaction was emitted in.
///
/// One header read per distinct height, plus the run's terminal anchor for its
/// own height (the run already resolved it, and a second read of one block states
/// nothing new).
async fn candidate_block_anchors(
    provider: &AlloyProvider,
    chain_id: u64,
    terminal: ChainAnchor,
    tx_groups: &[TxGroup<'_>],
) -> Result<BTreeMap<u64, ChainAnchor>, EvidenceAcquisitionError> {
    let mut numbers: Vec<u64> = tx_groups.iter().map(|group| group.block_number).collect();
    numbers.sort_unstable();
    numbers.dedup();
    let mut anchors = BTreeMap::new();
    for number in numbers {
        let anchor = if number == terminal.number() {
            terminal
        } else {
            provider
                .get_block_lineage_at(number)
                .await?
                .ok_or(EvidenceAcquisitionError::MissingHeader { number })?
        };
        if anchor.chain_id() != chain_id {
            return Err(EvidenceAcquisitionError::CrossChainBlock {
                number,
                expected: chain_id,
                served: anchor.chain_id(),
            });
        }
        anchors.insert(number, anchor);
    }
    Ok(anchors)
}

/// How many candidate transactions a chunk bound, and how many of them needed
/// call evidence (the chunk's acquisition report).
pub(super) fn trace_backed_count(candidates: &[CandidateTransaction<'_>]) -> usize {
    candidates
        .iter()
        .filter(|candidate| candidate.evidence.requirement() == EvidenceRequirement::TraceBacked)
        .count()
}
