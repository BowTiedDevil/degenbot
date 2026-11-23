//! Which candidate transactions need call-level evidence, and how a complete
//! receipt is bound to the transaction's filtered logs (ADR-105 D2, D5).
//!
//! Filtered log acquisition (the seven `eth_getLogs` passes) names the candidate
//! transactions of a chunk. From there each candidate's parse has ONE of two
//! evidence needs, and the difference is a matter of what the receipt cannot
//! prove:
//!
//! - an operation whose legs are named by a LOG coordinate (`Supply`, `Borrow`,
//!   `Repay`, `Withdraw`, scaled-token mints/burns/transfers, the configuration
//!   events) is proved by the complete receipt alone — those operations keep their
//!   receipt-only path and their existing outputs;
//! - a liquidation's collateral leg is named by a CALL: the seized-collateral
//!   events belong to the liquidation sub-call that emitted them, and two
//!   liquidations of one user against one collateral asset are indistinguishable by
//!   `(user, asset)` or by a transaction-wide log window (the `3e08816b6`
//!   failure). Those transactions need the call trace, and a trace-required
//!   transaction can never silently degrade to receipt-only input.
//!
//! [`bind_transaction_evidence`] is where the two meet: it proves every filtered log
//! is a real member of the transaction's complete receipt, then joins the trace when
//! one was supplied and the join is what proves per-call ownership.

use alloy::primitives::B256;
use alloy::rpc::types::Log;
use degenbot_core::canonical_head::TxRef;
use degenbot_decoders::aave_event_decoder::AAVE_LIQUIDATION_CALL_TOPIC;
use degenbot_rpc::tx_evidence::{
    join_receipt_trace, JoinedLog, JoinedTransaction, ReceiptTransaction, TraceJoinRefusal,
    TracedTransaction,
};

/// What a candidate transaction's operations need before they can be attributed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceRequirement {
    /// Every leg the parse assigns is named by a log coordinate, so the complete
    /// receipt is sufficient evidence.
    ReceiptOnly,
    /// At least one leg is named by a CALL (a liquidation's collateral sub-call),
    /// so the transaction's call trace is required evidence.
    TraceBacked,
}

impl EvidenceRequirement {
    /// The requirement the transaction's filtered logs state.
    ///
    /// A `LiquidationCall` is the operation whose legs a receipt cannot assign: the
    /// seized-collateral events are emitted by the liquidation's own sub-call, which
    /// the receipt records only as an ordered log. Every other Aave operation is
    /// named by its own log, so it stays receipt-only.
    #[must_use]
    pub fn of(filtered: &[&Log]) -> Self {
        let liquidation = filtered
            .iter()
            .any(|log| log.topics().first() == Some(&AAVE_LIQUIDATION_CALL_TOPIC));
        if liquidation {
            Self::TraceBacked
        } else {
            Self::ReceiptOnly
        }
    }
}

/// The evidence a candidate transaction carries into its parse.
///
/// The two arms are different types on purpose (ADR-105 D2): call paths exist only
/// on the trace-backed arm, so a builder that must name a call cannot be handed a
/// receipt-only transaction at all.
#[derive(Debug, Clone)]
pub enum TransactionEvidence {
    /// The transaction's complete receipt, for operations proved by log coordinates.
    ReceiptOnly(ReceiptTransaction),
    /// The transaction's complete receipt joined to its call trace, for operations
    /// whose legs are named by a call.
    TraceBacked(JoinedTransaction),
}

impl TransactionEvidence {
    /// The transaction the evidence describes.
    #[must_use]
    pub const fn tx(&self) -> &TxRef {
        match self {
            Self::ReceiptOnly(receipt) => receipt.tx(),
            Self::TraceBacked(joined) => joined.tx(),
        }
    }

    /// What the evidence carries.
    #[must_use]
    pub const fn requirement(&self) -> EvidenceRequirement {
        match self {
            Self::ReceiptOnly(_) => EvidenceRequirement::ReceiptOnly,
            Self::TraceBacked(_) => EvidenceRequirement::TraceBacked,
        }
    }

    /// The transaction's complete receipt logs, in the receipt's own order.
    #[must_use]
    pub fn logs(&self) -> Vec<&Log> {
        match self {
            Self::ReceiptOnly(receipt) => receipt.logs().iter().collect(),
            Self::TraceBacked(joined) => joined.logs().iter().map(JoinedLog::log).collect(),
        }
    }
}

/// Why a candidate transaction's evidence could not be bound.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EvidenceRefusal {
    /// A filtered log is not a member of the transaction's complete receipt: the
    /// filtered acquisition and the complete receipt do not describe one
    /// transaction, so neither is evidence for this parse.
    #[error("filtered log {log_index} is not a log of the complete receipt")]
    FilteredLogNotInReceipt {
        /// The filtered log's claimed block-wide index.
        log_index: u64,
    },
    /// The transaction's operations need call-level evidence, but no trace was
    /// supplied. A receipt cannot name a liquidation's collateral sub-call, so the
    /// parse is refused rather than run on insufficient evidence.
    #[error("transaction {transaction} needs call evidence, and no trace was supplied")]
    MissingTraceEvidence {
        /// The transaction whose parse needs a trace.
        transaction: B256,
    },
    /// The receipt and the trace could not be joined.
    #[error(transparent)]
    TraceJoin(#[from] TraceJoinRefusal),
}

/// Bind a candidate transaction's filtered logs to its complete receipt and, when
/// the transaction's operations need one, to its call trace.
///
/// Every filtered log is proved to be a member of the complete receipt first: the
/// filtered passes and the receipt must name the same execution, or the binding
/// refuses before any operation sees the logs. A trace-required transaction with no
/// trace is refused — never parsed receipt-only.
///
/// # Errors
///
/// [`EvidenceRefusal::FilteredLogNotInReceipt`] when a filtered log is not in the
/// receipt, [`EvidenceRefusal::MissingTraceEvidence`] when the transaction needs a
/// trace and `traced` is `None`, and [`EvidenceRefusal::TraceJoin`] when the receipt
/// and the trace do not describe one execution with one attribution per log.
pub fn bind_transaction_evidence(
    filtered: &[&Log],
    receipt: ReceiptTransaction,
    traced: Option<TracedTransaction>,
) -> Result<TransactionEvidence, EvidenceRefusal> {
    for log in filtered {
        if !receipt.contains(log) {
            return Err(EvidenceRefusal::FilteredLogNotInReceipt {
                log_index: log.log_index.unwrap_or_default(),
            });
        }
    }
    let Some(traced) = traced else {
        return match EvidenceRequirement::of(filtered) {
            EvidenceRequirement::ReceiptOnly => Ok(TransactionEvidence::ReceiptOnly(receipt)),
            EvidenceRequirement::TraceBacked => Err(EvidenceRefusal::MissingTraceEvidence {
                transaction: receipt.tx().hash(),
            }),
        };
    };
    let joined = join_receipt_trace(&receipt, &traced)?;
    Ok(TransactionEvidence::TraceBacked(joined))
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    use alloy::primitives::{Address, Bytes, Log as AlloyLog};
    use degenbot_core::canonical_head::ChainAnchor;
    use serde_json::Value;

    const RECEIPT: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../../tests/fixtures/aave_receipts/liquidation_collateral_ownership_26458346.json"
    );
    const TRACE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../../tests/fixtures/aave_receipts/liquidation_trace_26458346.json"
    );

    fn json(path: &str) -> Value {
        serde_json::from_slice(&std::fs::read(path).expect("the committed fixture must exist"))
            .expect("the committed fixture is JSON")
    }

    fn anchor(document: &Value) -> ChainAnchor {
        let header = &document["header"];
        ChainAnchor::new(
            document["chain_id"]
                .as_u64()
                .expect("the capture names its chain"),
            header["number"]
                .as_u64()
                .expect("the capture names its block"),
            header["hash"].as_str().unwrap().parse().unwrap(),
            header["parent_hash"].as_str().unwrap().parse().unwrap(),
        )
        .expect("the captured header is a complete identity")
    }

    fn captured() -> (ReceiptTransaction, TracedTransaction) {
        let receipt_document = json(RECEIPT);
        let trace_document = json(TRACE);
        let receipt = ReceiptTransaction::from_receipt_json(
            anchor(&trace_document),
            &receipt_document["receipt"],
        )
        .expect("the captured receipt carries its complete identity");
        let traced =
            TracedTransaction::from_call_tracer_json(*receipt.tx(), &trace_document["trace"])
                .expect("the captured trace is a call tree");
        (receipt, traced)
    }

    /// The transaction's receipt logs as the filtered acquisition would hold them:
    /// owned copies, so the binding can consume the receipt itself.
    fn filtered(receipt: &ReceiptTransaction) -> Vec<Log> {
        receipt.logs().to_vec()
    }

    fn refs(logs: &[Log]) -> Vec<&Log> {
        logs.iter().collect()
    }

    /// Every log of the captured liquidation transaction is a member of its own
    /// complete receipt, and the transaction needs call evidence: it carries six
    /// `LiquidationCall` events.
    #[test]
    fn a_liquidation_transaction_requires_call_evidence() {
        let (receipt, _) = captured();

        let logs = filtered(&receipt);

        assert_eq!(
            EvidenceRequirement::of(&refs(&logs)),
            EvidenceRequirement::TraceBacked
        );
    }

    /// The captured transaction binds its complete receipt to its trace: the
    /// primary acceptance oracle, driven through the Aave-side policy seam.
    #[test]
    fn the_captured_liquidation_binds_its_receipt_to_its_calls() {
        let (receipt, traced) = captured();
        let logs = filtered(&receipt);

        let evidence = bind_transaction_evidence(&refs(&logs), receipt, Some(traced))
            .expect("the committed captures are one execution");

        assert_eq!(evidence.requirement(), EvidenceRequirement::TraceBacked);
        let TransactionEvidence::TraceBacked(joined) = &evidence else {
            panic!("a liquidation transaction is trace-backed");
        };
        assert_eq!(joined.logs().len(), 218);
        assert_eq!(
            evidence
                .logs()
                .iter()
                .map(|log| log.log_index.unwrap())
                .collect::<Vec<_>>(),
            (4..=221).collect::<Vec<_>>(),
            "the bound evidence keeps the receipt's own positions"
        );
        assert_eq!(
            joined.owning_calls().len(),
            163,
            "the collateral legs are attributed to their own calls"
        );
    }

    /// A trace-required transaction cannot be parsed from its receipt alone.
    #[test]
    fn a_trace_required_transaction_cannot_bind_receipt_only() {
        let (receipt, _) = captured();
        let logs = filtered(&receipt);
        let tx = *receipt.tx();

        assert_eq!(
            bind_transaction_evidence(&refs(&logs), receipt, None).unwrap_err(),
            EvidenceRefusal::MissingTraceEvidence {
                transaction: tx.hash(),
            }
        );
    }

    /// A transaction with no liquidation keeps the receipt-only path: its complete
    /// receipt is the parse input, and no call path is invented for it.
    #[test]
    fn a_receipt_only_transaction_needs_no_call_evidence() {
        let (receipt, _) = captured();
        let owned = filtered(&receipt);
        let logs: Vec<&Log> = owned
            .iter()
            .filter(|log| log.log_index == Some(4))
            .collect();

        assert_eq!(
            EvidenceRequirement::of(&logs),
            EvidenceRequirement::ReceiptOnly,
            "a token Transfer names no call"
        );
        let evidence = bind_transaction_evidence(&logs, receipt, None)
            .expect("the complete receipt is sufficient evidence");
        assert_eq!(evidence.requirement(), EvidenceRequirement::ReceiptOnly);
        assert_eq!(
            evidence.logs().len(),
            218,
            "receipt-only processing still reads the transaction's complete receipt"
        );
        assert_eq!(evidence.logs()[0].log_index, Some(4));
    }

    /// A filtered log the complete receipt does not carry means the two
    /// acquisitions describe different executions: the binding refuses before the
    /// parse can see either.
    #[test]
    fn a_filtered_log_outside_the_complete_receipt_is_refused() {
        let (receipt, traced) = captured();
        let tx = *receipt.tx();
        let mut stranger = receipt.logs()[0].clone();
        stranger.inner = AlloyLog::new_unchecked(
            Address::repeat_byte(0x77),
            vec![B256::new([0x33; 32])],
            Bytes::new(),
        );
        let logs: Vec<&Log> = vec![&stranger];

        assert_eq!(
            bind_transaction_evidence(&logs, receipt, Some(traced)).unwrap_err(),
            EvidenceRefusal::FilteredLogNotInReceipt { log_index: 4 }
        );
        assert_eq!(stranger.transaction_hash, Some(tx.hash()));
    }

    /// A filtered log must match the receipt's log in EVERY identity fact: the same
    /// content emitted by another transaction, or reported in another block, is not a
    /// member of the complete receipt and the binding refuses before the parse sees
    /// either.
    #[test]
    fn a_filtered_log_must_match_the_receipt_in_every_identity_fact() {
        let (receipt, traced) = captured();
        let tx = *receipt.tx();
        let mut foreign_transaction = receipt.logs()[0].clone();
        foreign_transaction.transaction_hash = Some(B256::repeat_byte(0xdd));
        let mut foreign_block = receipt.logs()[0].clone();
        foreign_block.block_hash = Some(B256::repeat_byte(0xee));
        let mut no_block = receipt.logs()[0].clone();
        no_block.block_hash = None;

        for stranger in [&foreign_transaction, &foreign_block, &no_block] {
            let logs: Vec<&Log> = vec![stranger];
            assert_eq!(
                bind_transaction_evidence(&logs, receipt.clone(), Some(traced.clone()))
                    .unwrap_err(),
                EvidenceRefusal::FilteredLogNotInReceipt {
                    log_index: stranger.log_index.unwrap(),
                },
                "same content, other identity: {stranger:?}"
            );
        }
        assert_ne!(foreign_transaction.transaction_hash, Some(tx.hash()));
        assert_ne!(foreign_block.block_hash, Some(tx.anchor().hash()));
    }

    /// A trace for another transaction cannot bind this receipt.
    #[test]
    fn a_foreign_trace_cannot_bind_this_receipt() {
        let (receipt, _) = captured();
        let other = TxRef::new(
            *receipt.tx().anchor(),
            B256::new([0xdd; 32]),
            receipt.tx().index(),
        )
        .unwrap();
        let document = json(TRACE);
        let foreign = TracedTransaction::from_call_tracer_json(other, &document["trace"])
            .expect("the call tree is valid evidence for its own transaction");
        let logs = filtered(&receipt);

        assert!(matches!(
            bind_transaction_evidence(&refs(&logs), receipt, Some(foreign)).unwrap_err(),
            EvidenceRefusal::TraceJoin(TraceJoinRefusal::TransactionMismatch { .. })
        ));
    }
}
