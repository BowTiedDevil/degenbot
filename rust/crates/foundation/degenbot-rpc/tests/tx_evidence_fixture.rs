//! The committed block-26,458,346 receipt and trace evidence: complete receipts,
//! the selective call trace, and the total ordered join that assigns every
//! receipt log its emitting call (ADR-105 D2, D5).
//!
//! The two fixtures are live node answers (see each file's `provenance`):
//! `eth_getTransactionReceipt` for the 218-log liquidation transaction, and
//! `debug_traceTransaction` with `callTracer` + `withLog` over the same
//! transaction. Neither file states a log position the other does: the receipt
//! owns the block-wide `logIndex`, the trace owns the call path, and the join is
//! what proves they describe one execution.

#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use alloy::primitives::B256;
use degenbot_core::canonical_head::{ChainAnchor, TxRef};
use degenbot_rpc::tx_evidence::{
    join_receipt_trace, ReceiptRefusal, ReceiptTransaction, TraceJoinRefusal, TracedTransaction,
};
use serde_json::Value;

/// The committed complete-receipt capture.
const RECEIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/aave_receipts/liquidation_collateral_ownership_26458346.json"
);

/// The committed call-trace capture of the same transaction.
const TRACE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/aave_receipts/liquidation_trace_26458346.json"
);

fn json(path: &str) -> Value {
    serde_json::from_slice(&std::fs::read(path).expect("the committed fixture must exist"))
        .expect("the committed fixture is JSON")
}

fn anchor_of(trace: &Value) -> ChainAnchor {
    let header = &trace["header"];
    ChainAnchor::new(
        trace["chain_id"]
            .as_u64()
            .expect("the capture names its chain"),
        header["number"]
            .as_u64()
            .expect("the capture names its block"),
        header["hash"]
            .as_str()
            .expect("the header names its hash")
            .parse()
            .unwrap(),
        header["parent_hash"]
            .as_str()
            .expect("the header names its parent")
            .parse()
            .unwrap(),
    )
    .expect("the captured header is a complete identity")
}

const ZERO_LOG_FRAME_RECEIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/aave_receipts/liquidation_receipt_zero_log_frames_16919479.json"
);

const ZERO_LOG_FRAME_TRACE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/aave_receipts/liquidation_trace_zero_log_frames_16919479.json"
);

fn captured_at(receipt_path: &str, trace_path: &str) -> (ReceiptTransaction, TracedTransaction) {
    let receipt_document = json(receipt_path);
    let trace_document = json(trace_path);
    let anchor = anchor_of(&trace_document);

    let receipt = ReceiptTransaction::from_receipt_json(anchor, &receipt_document["receipt"])
        .expect("the captured receipt carries its complete identity");
    let tx = *receipt.tx();
    assert_eq!(
        tx.hash().to_string(),
        trace_document["transaction"]["hash"].as_str().unwrap(),
        "both captures describe the same transaction"
    );
    let traced = TracedTransaction::from_call_tracer_json(tx, &trace_document["trace"])
        .expect("the captured trace is a call tree");
    (receipt, traced)
}

fn captured() -> (ReceiptTransaction, TracedTransaction) {
    captured_at(RECEIPT, TRACE)
}

fn call_path(joined: &[(u64, Vec<u64>)], log_index: u64) -> Vec<u64> {
    joined
        .iter()
        .find(|(index, _)| *index == log_index)
        .unwrap_or_else(|| panic!("log {log_index} has no joined call"))
        .1
        .clone()
}

#[test]
fn zero_log_frames_do_not_make_the_captured_attribution_ambiguous() {
    let (receipt, traced) = captured_at(ZERO_LOG_FRAME_RECEIPT, ZERO_LOG_FRAME_TRACE);
    assert_eq!(receipt.logs().len(), 19);
    assert_eq!(traced.log_count(), 19);

    let joined =
        join_receipt_trace(&receipt, &traced).expect("zero-log frames do not change log ownership");
    assert_eq!(joined.logs().len(), 19);
    assert_eq!(call_path(&positions(&joined), 149), vec![0]);
    assert_ne!(call_path(&positions(&joined), 161), vec![0]);
}

fn positions(joined: &degenbot_rpc::tx_evidence::JoinedTransaction) -> Vec<(u64, Vec<u64>)> {
    joined
        .logs()
        .iter()
        .map(|entry| (entry.log_ref().index(), entry.call().call_path().to_vec()))
        .collect()
}

#[test]
fn the_captured_receipt_joins_every_log_to_its_call() {
    let (receipt, traced) = captured();
    assert_eq!(
        receipt.logs().len(),
        218,
        "the capture carries 218 receipt logs"
    );
    assert_eq!(
        traced.log_count(),
        218,
        "the capture carries 218 traced logs"
    );

    let joined = join_receipt_trace(&receipt, &traced)
        .expect("every receipt log has exactly one emitting call");

    let positions: Vec<(u64, Vec<u64>)> = joined
        .logs()
        .iter()
        .map(|entry| (entry.log_ref().index(), entry.call().call_path().to_vec()))
        .collect();
    assert_eq!(positions.len(), 218, "the join is total over the receipt");
    assert_eq!(
        positions
            .iter()
            .map(|(index, _)| *index)
            .collect::<Vec<_>>(),
        (4..=221).collect::<Vec<_>>(),
        "each joined log carries the receipt's own block-wide logIndex, in order"
    );
    for entry in joined.logs() {
        assert_eq!(
            *entry.log_ref().tx(),
            *receipt.tx(),
            "a joined log is qualified by its transaction identity"
        );
        assert_eq!(
            *entry.call().tx(),
            *receipt.tx(),
            "the owning call lives in the same transaction"
        );
    }

    let distinct: std::collections::BTreeSet<&Vec<u64>> =
        positions.iter().map(|(_, path)| path).collect();
    assert_eq!(
        distinct.len(),
        163,
        "the 218 logs come from 163 distinct calls, not one transaction-wide bucket"
    );

    // The two liquidations of the same user against the same aEthWETH
    // collateral own DIFFERENT sub-calls: the first WETH leg's logs belong to
    // the call that closes at log 96, the second's to the call that closes at
    // 196 (the regression 3e08816b6 fixed by a log window now proved by the
    // trace).
    let first_weth = call_path(&positions, 96);
    let second_weth = call_path(&positions, 196);
    assert_ne!(
        first_weth, second_weth,
        "one collateral pair, two liquidation calls"
    );
    assert_eq!(
        call_path(&positions, 89),
        {
            let mut leaf = first_weth.clone();
            leaf.extend([17, 0]);
            leaf
        },
        "the first WETH leg's interest-exceeds Mint is a call under its liquidation"
    );
    for index in [89u64, 93, 94] {
        assert!(
            call_path(&positions, index).starts_with(&first_weth),
            "log {index} belongs to the first WETH liquidation's own call tree"
        );
    }
    for index in [190u64, 191, 194] {
        assert!(
            call_path(&positions, index).starts_with(&second_weth),
            "log {index} belongs to the second WETH liquidation's own call tree"
        );
    }

    // The WBTC leg: its burn and protocol-fee transfer sit under the call that
    // closes with the LiquidationCall at log 78.
    let wbtc = call_path(&positions, 78);
    for index in [73u64, 76] {
        assert!(
            call_path(&positions, index).starts_with(&wbtc),
            "log {index} is emitted inside the WBTC liquidation call"
        );
    }
}

#[test]
fn a_receipt_that_names_no_transaction_has_no_position() {
    let mut receipt = json(RECEIPT);
    receipt["receipt"]
        .as_object_mut()
        .unwrap()
        .remove("transactionHash");

    let anchor = anchor_of(&json(TRACE));
    let refused = ReceiptTransaction::from_receipt_json(anchor, &receipt["receipt"])
        .expect_err("a receipt without its own transaction hash is not evidence");

    assert_eq!(refused, ReceiptRefusal::MissingTransactionHash);
}

#[test]
fn a_receipt_from_another_block_than_the_anchor_is_refused() {
    let document = json(RECEIPT);
    let anchor = anchor_of(&json(TRACE));

    let foreign_block = ChainAnchor::new(
        anchor.chain_id(),
        anchor.number(),
        B256::new([0xbb; 32]),
        anchor.parent_hash(),
    )
    .unwrap();
    let refused = ReceiptTransaction::from_receipt_json(foreign_block, &document["receipt"])
        .expect_err("a receipt emitted in another block cannot fill this anchor's positions");

    assert_eq!(
        refused,
        ReceiptRefusal::BlockHashMismatch {
            expected: B256::new([0xbb; 32]),
            actual: anchor.hash(),
        }
    );

    let other_height = ChainAnchor::new(
        anchor.chain_id(),
        anchor.number() + 1,
        anchor.hash(),
        anchor.parent_hash(),
    )
    .unwrap();
    assert_eq!(
        ReceiptTransaction::from_receipt_json(other_height, &document["receipt"]).unwrap_err(),
        ReceiptRefusal::BlockNumberMismatch {
            expected: anchor.number() + 1,
            actual: anchor.number(),
        }
    );
}

#[test]
fn a_receipt_log_without_its_block_hash_is_not_a_position() {
    let mut document = json(RECEIPT);
    let anchor = anchor_of(&json(TRACE));
    document["receipt"]["logs"][7]
        .as_object_mut()
        .unwrap()
        .remove("blockHash");

    assert_eq!(
        ReceiptTransaction::from_receipt_json(anchor, &document["receipt"]).unwrap_err(),
        ReceiptRefusal::LogWithoutBlockHash { log_index: 11 }
    );
}

#[test]
fn a_truncated_receipt_cannot_join_a_complete_trace() {
    let (receipt, traced) = captured();
    let mut document = json(RECEIPT);
    document["receipt"]["logs"].as_array_mut().unwrap().pop();
    let truncated =
        ReceiptTransaction::from_receipt_json(*receipt.tx().anchor(), &document["receipt"])
            .expect("the remaining 217 logs are still a complete receipt");

    assert_eq!(
        join_receipt_trace(&truncated, &traced).unwrap_err(),
        TraceJoinRefusal::LogCountMismatch {
            receipt: 217,
            trace: 218,
        }
    );
}

#[test]
fn a_receipt_log_that_disagrees_with_the_trace_refuses_the_join() {
    let (receipt, traced) = captured();
    let mut document = json(RECEIPT);
    document["receipt"]["logs"][40]["data"] = Value::String(format!("0x{}", "00".repeat(32)));
    let mutated =
        ReceiptTransaction::from_receipt_json(*receipt.tx().anchor(), &document["receipt"])
            .expect("a mutated log body is still a well-formed receipt");

    assert_eq!(
        join_receipt_trace(&mutated, &traced).unwrap_err(),
        TraceJoinRefusal::LogContentMismatch {
            receipt: 218,
            trace: 218,
        },
        "no interleaving of the traced logs reproduces the mutated receipt"
    );
}

#[test]
fn a_trace_of_another_transaction_cannot_join_this_receipt() {
    let (receipt, _) = captured();
    let other = TxRef::new(
        *receipt.tx().anchor(),
        B256::new([0xcc; 32]),
        receipt.tx().index(),
    )
    .unwrap();
    let trace_document = json(TRACE);
    let foreign = TracedTransaction::from_call_tracer_json(other, &trace_document["trace"])
        .expect("the captured call tree is valid evidence for its own transaction");

    assert_eq!(
        join_receipt_trace(&receipt, &foreign).unwrap_err(),
        TraceJoinRefusal::TransactionMismatch {
            receipt: Box::new(*receipt.tx()),
            trace: Box::new(other),
        }
    );
}

#[test]
fn a_trace_from_another_chain_cannot_join_this_receipt() {
    let (receipt, _) = captured();
    let other_anchor = ChainAnchor::new(
        2,
        receipt.tx().anchor().number(),
        receipt.tx().anchor().hash(),
        receipt.tx().anchor().parent_hash(),
    )
    .unwrap();
    let other = TxRef::new(other_anchor, receipt.tx().hash(), receipt.tx().index()).unwrap();
    let trace_document = json(TRACE);
    let foreign =
        TracedTransaction::from_call_tracer_json(other, &trace_document["trace"]).unwrap();

    assert_eq!(
        join_receipt_trace(&receipt, &foreign).unwrap_err(),
        TraceJoinRefusal::TransactionMismatch {
            receipt: Box::new(*receipt.tx()),
            trace: Box::new(other),
        }
    );
}
