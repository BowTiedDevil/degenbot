//! The evidence primitives on their own: identity refusals the committed
//! captures cannot exercise, the join's attribution proof, and the acquisition
//! surface over a recorded endpoint.

#![expect(clippy::unwrap_used, clippy::expect_used)]

use super::*;

use alloy::primitives::{Bytes, Log as AlloyLog};

const CHAIN: u64 = 1;
const BLOCK: u64 = 100;

fn hash(tag: u8) -> B256 {
    B256::new([tag; 32])
}

fn anchor() -> ChainAnchor {
    ChainAnchor::new(CHAIN, BLOCK, hash(0xaa), hash(0x99)).unwrap()
}

fn tx() -> TxRef {
    TxRef::new(anchor(), hash(0xcc), 3).unwrap()
}

fn receipt_log(tx: TxRef, index: u64, address: u8, topic: u8, data: u8) -> Log {
    let inner = AlloyLog::new_unchecked(
        Address::repeat_byte(address),
        vec![B256::new([topic; 32])],
        Bytes::from(vec![data; 4]),
    );
    Log {
        inner,
        block_hash: Some(tx.anchor().hash()),
        block_number: Some(tx.anchor().number()),
        block_timestamp: None,
        transaction_hash: Some(tx.hash()),
        transaction_index: Some(tx.index()),
        log_index: Some(index),
        removed: false,
    }
}

fn trace_log(address: u8, topic: u8, data: u8) -> Value {
    json!({
        "address": format!("{}", Address::repeat_byte(address)),
        "topics": [format!("{}", B256::new([topic; 32]))],
        "data": format!("0x{}", alloy::hex::encode(vec![data; 4])),
    })
}

fn trace_frame(kind: &str, address: u8, logs: Vec<Value>, children: Vec<Value>) -> Value {
    let mut frame = json!({
        "type": kind,
        "to": format!("{}", Address::repeat_byte(address)),
    });
    frame["logs"] = Value::Array(logs);
    frame["calls"] = Value::Array(children);
    frame
}

fn traced(tx: TxRef, root: &Value) -> TracedTransaction {
    TracedTransaction::from_call_tracer_json(tx, root).expect("the fixture trace parses")
}

fn receipt(tx: TxRef, logs: Vec<Log>) -> ReceiptTransaction {
    ReceiptTransaction::new(tx, logs).expect("the fixture receipt carries its identity")
}

// ── receipt identity ────────────────────────────────────────────────────

/// A receipt's logs must ascend by block-wide index: the receipt itself states
/// one order, and a receipt that contradicts it is contradictory evidence rather
/// than a set the join could sort into agreement.
#[test]
fn receipt_logs_must_ascend_by_block_wide_index() {
    let tx = tx();
    assert_eq!(
        ReceiptTransaction::new(
            tx,
            vec![
                receipt_log(tx, 9, 0x01, 0x02, 0x03),
                receipt_log(tx, 7, 0x01, 0x02, 0x03),
            ],
        )
        .unwrap_err(),
        ReceiptRefusal::LogOrder {
            previous: 9,
            actual: 7,
        }
    );
    assert_eq!(
        ReceiptTransaction::new(
            tx,
            vec![
                receipt_log(tx, 7, 0x01, 0x02, 0x03),
                receipt_log(tx, 7, 0x01, 0x02, 0x04),
            ],
        )
        .unwrap_err(),
        ReceiptRefusal::LogOrder {
            previous: 7,
            actual: 7,
        },
        "two logs cannot share one block-wide position"
    );
}

/// Cassette-canonical responses state quantities in DECIMAL, while a node states them
/// as `0x` hex. A complete receipt is evidence in either form: the parser reads both,
/// so a recorded answer and a live one are the same evidence.
#[test]
fn a_decimal_receipt_carries_the_same_positions_as_a_hex_one() {
    let tx = tx();
    let mut document = json!({
        "transactionHash": format!("{}", tx.hash()),
        "transactionIndex": "12",
        "blockNumber": "100",
        "blockHash": format!("{}", tx.anchor().hash()),
        "logs": [],
    });
    let mut log = receipt_log_json(22, 0x01, 0x0a, 0x01);
    log["transactionIndex"] = json!("12");
    log["logIndex"] = json!("22");
    document["logs"] = json!([log]);

    let receipt = ReceiptTransaction::from_receipt_json(anchor(), &document)
        .expect("a decimal receipt is a complete receipt");

    assert_eq!(
        (receipt.tx().index(), receipt.tx().anchor().number()),
        (12, 100),
        "a decimal position is the value it states, not that string read as hex"
    );
    assert_eq!(
        receipt.log_refs().collect::<Vec<_>>(),
        vec![LogRef::new(*receipt.tx(), 22)],
        "the decimal log index is the block-wide position"
    );
    assert_ne!(
        receipt.tx().index(),
        tx.index(),
        "the receipt states its own transaction index, which this fixture differs from"
    );
}

/// A removed log is not canonical evidence: it describes a log the chain no longer
/// states, so a receipt that reports one is refused rather than read as if the log
/// still existed.
#[test]
fn a_receipt_that_carries_a_removed_log_is_refused() {
    let tx = tx();
    let mut log = receipt_log(tx, 7, 0x01, 0x02, 0x03);
    log.removed = true;

    assert_eq!(
        ReceiptTransaction::new(tx, vec![log]).unwrap_err(),
        ReceiptRefusal::RemovedLog { log_index: 7 }
    );

    let mut document = json!({
        "transactionHash": format!("{}", tx.hash()),
        "transactionIndex": "3",
        "blockNumber": "100",
        "blockHash": format!("{}", tx.anchor().hash()),
        "logs": [],
    });
    let mut entry = receipt_log_json(7, 0x01, 0x02, 0x03);
    entry["removed"] = json!(true);
    document["logs"] = json!([entry]);
    assert_eq!(
        ReceiptTransaction::from_receipt_json(anchor(), &document).unwrap_err(),
        ReceiptRefusal::RemovedLog { log_index: 7 }
    );
}

/// A filtered log is a member of the complete receipt only when EVERY identity fact
/// matches: the same index and content emitted by another transaction, or reported in
/// another block (or with no block at all), is a different log.
#[test]
fn only_the_same_log_at_the_same_position_is_a_member() {
    let tx = tx();
    let log = receipt_log(tx, 7, 0x01, 0x02, 0x03);
    let receipt = ReceiptTransaction::new(tx, vec![log.clone()]).unwrap();

    assert!(receipt.contains(&log), "the receipt's own log is a member");

    let mut foreign_transaction = log.clone();
    foreign_transaction.transaction_hash = Some(hash(0xdd));
    assert!(
        !receipt.contains(&foreign_transaction),
        "the same content in another transaction is another log"
    );

    let mut foreign_transaction_index = log.clone();
    foreign_transaction_index.transaction_index = Some(9);
    assert!(
        !receipt.contains(&foreign_transaction_index),
        "the same content at another transaction index is another log"
    );

    let mut foreign_block = log.clone();
    foreign_block.block_hash = Some(hash(0xee));
    assert!(
        !receipt.contains(&foreign_block),
        "the same content in another block is another log"
    );

    let mut no_block_hash = log.clone();
    no_block_hash.block_hash = None;
    assert!(
        !receipt.contains(&no_block_hash),
        "a log that names no block cannot be proved to be in this block"
    );

    let mut no_block_number = log.clone();
    no_block_number.block_number = None;
    assert!(
        !receipt.contains(&no_block_number),
        "a log that names no height cannot be placed in this receipt"
    );

    let mut removed = log.clone();
    removed.removed = true;
    assert!(
        !receipt.contains(&removed),
        "a removed log is not the canonical log the receipt states"
    );
}

/// A log's transaction index must be the receipt's own: two indexes for one
/// transaction is contradictory evidence about which transaction the log belongs to.
#[test]
fn a_receipt_log_with_another_transaction_index_is_refused() {
    let tx = tx();
    let mut log = receipt_log(tx, 7, 0x01, 0x02, 0x03);
    log.transaction_index = Some(9);

    assert_eq!(
        ReceiptTransaction::new(tx, vec![log]).unwrap_err(),
        ReceiptRefusal::LogTransactionIndexMismatch {
            expected: tx.index(),
            actual: 9,
        }
    );
}

/// A log with no index has no block-wide position at all — the same refusal the
/// filtered-acquisition whole-batch validation states, one type up.
#[test]
fn a_receipt_log_without_an_index_has_no_position() {
    let tx = tx();
    let mut log = receipt_log(tx, 7, 0x01, 0x02, 0x03);
    log.log_index = None;

    assert_eq!(
        ReceiptTransaction::new(tx, vec![log]).unwrap_err(),
        ReceiptRefusal::LogWithoutIndex
    );
}

/// A log from another transaction cannot fill this transaction's receipt, and a
/// log from another block cannot fill this block's.
#[test]
fn a_receipt_log_from_another_transaction_or_block_is_refused() {
    let tx = tx();
    let mut foreign = receipt_log(tx, 7, 0x01, 0x02, 0x03);
    foreign.transaction_hash = Some(hash(0xdd));
    assert_eq!(
        ReceiptTransaction::new(tx, vec![foreign]).unwrap_err(),
        ReceiptRefusal::ForeignTransactionLog {
            expected: tx.hash(),
            actual: hash(0xdd),
        }
    );

    let mut elsewhere = receipt_log(tx, 7, 0x01, 0x02, 0x03);
    elsewhere.block_hash = Some(hash(0xee));
    assert_eq!(
        ReceiptTransaction::new(tx, vec![elsewhere]).unwrap_err(),
        ReceiptRefusal::LogBlockHashMismatch {
            expected: tx.anchor().hash(),
            actual: hash(0xee),
        }
    );
}

/// A receipt whose own `logs` member is missing has no complete log list: an
/// empty receipt is not the same evidence as a transaction that emitted nothing.
#[test]
fn a_receipt_without_its_log_list_is_refused() {
    let document = json!({
        "transactionHash": format!("{}", tx().hash()),
        "transactionIndex": "0x3",
        "blockNumber": "0x64",
        "blockHash": format!("{}", anchor().hash()),
    });

    assert_eq!(
        ReceiptTransaction::from_receipt_json(anchor(), &document).unwrap_err(),
        ReceiptRefusal::MissingLogs
    );
}

// ── the join's attribution proof ────────────────────────────────────────

/// A frame emits logs BEFORE and AFTER a child call, so a depth-first flatten of
/// the tree is not the receipt order. The join must attribute by the child's
/// contiguous interval, not by traversal order.
#[test]
fn a_parent_that_emits_around_its_child_joins_both_segments() {
    let tx = tx();
    let logs = vec![
        receipt_log(tx, 0, 0x01, 0x0a, 0x01),
        receipt_log(tx, 1, 0x02, 0x0b, 0x02),
        receipt_log(tx, 2, 0x01, 0x0a, 0x03),
    ];
    let receipt = receipt(tx, logs);
    let root = trace_frame(
        "CALL",
        0x01,
        vec![trace_log(0x01, 0x0a, 0x01), trace_log(0x01, 0x0a, 0x03)],
        vec![trace_frame(
            "STATICCALL",
            0x02,
            vec![trace_log(0x02, 0x0b, 0x02)],
            vec![],
        )],
    );

    let joined = join_receipt_trace(&receipt, &traced(tx, &root)).unwrap();

    let paths: Vec<(u64, Vec<u64>)> = joined
        .logs()
        .iter()
        .map(|entry| (entry.log_ref().index(), entry.call().call_path().to_vec()))
        .collect();
    assert_eq!(
        paths,
        vec![(0, vec![]), (1, vec![0]), (2, vec![])],
        "the parent owns the logs it emitted around its child's call"
    );
}

/// The ROOT call is a real owner: a transaction whose top-level frame emits the
/// log needs no child call to explain it.
#[test]
fn the_root_call_owns_the_logs_it_emits() {
    let tx = tx();
    let receipt = receipt(tx, vec![receipt_log(tx, 4, 0x01, 0x0a, 0x01)]);
    let root = trace_frame("CALL", 0x01, vec![trace_log(0x01, 0x0a, 0x01)], vec![]);

    let joined = join_receipt_trace(&receipt, &traced(tx, &root)).unwrap();

    assert!(joined.logs()[0].call().is_root());
    assert_eq!(joined.logs()[0].log_ref().index(), 4);
}

/// A tracer's call-local log index is not a receipt position: the same log can be
/// index 0 inside its call while the receipt states index 9 in the block.
#[test]
fn a_call_local_trace_index_is_not_a_receipt_position() {
    let tx = tx();
    let receipt = receipt(tx, vec![receipt_log(tx, 9, 0x01, 0x0a, 0x01)]);
    let mut entry = trace_log(0x01, 0x0a, 0x01);
    entry["index"] = json!("0x0");
    entry["position"] = json!("0x0");
    let root = trace_frame("CALL", 0x01, vec![entry], vec![]);

    let joined = join_receipt_trace(&receipt, &traced(tx, &root)).unwrap();

    assert_eq!(
        joined.logs()[0].log_ref().index(),
        9,
        "the receipt states the position, never the tracer"
    );
}

/// A frame's own log that is IDENTICAL to a child's leaves two interleavings that
/// both reproduce the receipt, so the evidence names no single owner: the join
/// refuses instead of picking one.
#[test]
fn indistinguishable_logs_admit_no_single_owner() {
    let tx = tx();
    let log = |index: u64| receipt_log(tx, index, 0x01, 0x0a, 0x01);
    let receipt = receipt(tx, vec![log(0), log(1), log(2)]);
    let root = trace_frame(
        "CALL",
        0x01,
        vec![trace_log(0x01, 0x0a, 0x01), trace_log(0x01, 0x0a, 0x01)],
        vec![trace_frame(
            "CALL",
            0x01,
            vec![trace_log(0x01, 0x0a, 0x01)],
            vec![],
        )],
    );

    assert_eq!(
        join_receipt_trace(&receipt, &traced(tx, &root)).unwrap_err(),
        TraceJoinRefusal::AmbiguousAttribution {
            receipt: 3,
            trace: 3,
        }
    );
}

/// Two siblings emitting the same log are NOT ambiguous: sub-calls run in order, so
/// their intervals — and therefore their logs — are ordered.
#[test]
fn sibling_calls_emitting_the_same_log_are_ordered_by_execution() {
    let tx = tx();
    let log = |index: u64| receipt_log(tx, index, 0x01, 0x0a, 0x01);
    let receipt = receipt(tx, vec![log(0), log(1)]);
    let child = || trace_frame("CALL", 0x01, vec![trace_log(0x01, 0x0a, 0x01)], vec![]);
    let root = trace_frame("CALL", 0x09, vec![], vec![child(), child()]);

    let joined = join_receipt_trace(&receipt, &traced(tx, &root)).unwrap();

    assert_eq!(
        joined
            .logs()
            .iter()
            .map(|entry| entry.call().call_path().to_vec())
            .collect::<Vec<_>>(),
        vec![vec![0], vec![1]]
    );
}

/// A receipt log the trace never emitted in that order has no attribution at all —
/// including the case where the counts agree and only the content differs.
#[test]
fn a_receipt_order_the_trace_cannot_reproduce_refuses_the_join() {
    let tx = tx();
    let receipt = receipt(
        tx,
        vec![
            receipt_log(tx, 0, 0x02, 0x0b, 0x02),
            receipt_log(tx, 1, 0x01, 0x0a, 0x01),
        ],
    );
    let root = trace_frame(
        "CALL",
        0x09,
        vec![],
        vec![
            trace_frame("CALL", 0x01, vec![trace_log(0x01, 0x0a, 0x01)], vec![]),
            trace_frame("CALL", 0x02, vec![trace_log(0x02, 0x0b, 0x02)], vec![]),
        ],
    );

    assert_eq!(
        join_receipt_trace(&receipt, &traced(tx, &root)).unwrap_err(),
        TraceJoinRefusal::LogContentMismatch {
            receipt: 2,
            trace: 2,
        },
        "the child intervals are in execution order, so the receipt cannot be reproduced"
    );
}

/// A frame whose type is not a call type is refused at parse time: an unknown
/// frame cannot be silently treated as an ordinary call.
#[test]
fn a_trace_frame_with_an_unknown_type_is_refused() {
    let document = json!({"type": "SELFDESTRUCT", "to": null});

    assert_eq!(
        TracedTransaction::from_call_tracer_json(tx(), &document).unwrap_err(),
        TraceRefusal::UnknownCallKind {
            kind: "SELFDESTRUCT".to_string(),
        }
    );
}

// ── acquisition ─────────────────────────────────────────────────────────

/// A recorded endpoint that serves exactly ONE answer, keyed through the
/// cassette's own canonicalization — so the request this module issues is the
/// request the assertion pins.
fn recorded_entries(
    answers: &[(&str, Value, Value)],
) -> crate::cassette_replay::CassetteReplayTransport {
    let entries: Vec<String> = answers
        .iter()
        .map(|(method, params, answer)| {
            let key = crate::cassette::entry_key(method, params);
            let response = answer.to_string();
            let digest = alloy::primitives::keccak256(response.as_bytes()).to_string();
            let escaped = key.escape_default().to_string();
            format!("\"{escaped}\": {{\"response\": {response}, \"digest\": \"{digest}\"}}")
        })
        .collect();
    let json = format!(
        "{{\"schema\":\"degenbot.cassette/v1\",\"chain_id\":1,\"provenance\":{{\"source\":\"test\",\"recorded_at\":\"1970-01-01T00:00:00Z\",\"span\":{{\"from_block\":0,\"to_block\":0}}}},\"entries\":{{{}}}}}",
        entries.join(",")
    );
    crate::cassette_replay::CassetteReplayTransport::new(
        crate::cassette::Cassette::from_json_str(&json).expect("the in-memory cassette parses"),
    )
}

fn recorded(
    method: &str,
    params: Value,
    answer: Value,
) -> crate::cassette_replay::CassetteReplayTransport {
    recorded_entries(&[(method, params, answer)])
}

fn receipt_answer(tx: TxRef, logs: Vec<Value>) -> Value {
    let mut answer = json!({
        "result": {
            "transactionHash": format!("{}", tx.hash()),
            "transactionIndex": format!("0x{:x}", tx.index()),
            "blockNumber": format!("0x{:x}", tx.anchor().number()),
            "blockHash": format!("{}", tx.anchor().hash()),
        }
    });
    answer["result"]["logs"] = Value::Array(logs);
    answer
}

fn receipt_log_json(index: u64, address: u8, topic: u8, data: u8) -> Value {
    json!({
        "address": format!("{}", Address::repeat_byte(address)),
        "topics": [format!("{}", B256::new([topic; 32]))],
        "data": format!("0x{}", alloy::hex::encode(vec![data; 4])),
        "blockNumber": format!("0x{:x}", BLOCK),
        "blockHash": format!("{}", anchor().hash()),
        "transactionHash": format!("{}", tx().hash()),
        "transactionIndex": "0x3",
        "logIndex": format!("0x{index:x}"),
        "removed": false,
    })
}

/// The receipt acquisition issues the one request that names the transaction and
/// returns the validated complete receipt.
#[test]
fn the_receipt_acquisition_serves_the_recorded_answer() {
    let tx = tx();
    let transport = recorded(
        "eth_getTransactionReceipt",
        json!([format!("{}", tx.hash())]),
        receipt_answer(tx, vec![receipt_log_json(5, 0x01, 0x0a, 0x01)]),
    );
    let provider = transport.as_alloy_provider();

    let receipt = degenbot_core::runtime::get_runtime()
        .block_on(fetch_receipt(&provider, tx))
        .expect("the recorded receipt is the requested transaction's");

    assert_eq!(receipt.tx(), &tx);
    assert_eq!(receipt.logs().len(), 1);
    assert_eq!(
        receipt.log_refs().collect::<Vec<_>>(),
        vec![LogRef::new(tx, 5)]
    );
    assert_eq!(transport.served_snapshot().requests, 1);
}

/// A transaction the node holds no receipt for is a refusal, never an empty
/// receipt invented from a `null` answer.
#[test]
fn a_missing_receipt_is_not_an_empty_receipt() {
    let tx = tx();
    let transport = recorded(
        "eth_getTransactionReceipt",
        json!([format!("{}", tx.hash())]),
        json!({"result": null}),
    );
    let provider = transport.as_alloy_provider();

    let error = degenbot_core::runtime::get_runtime()
        .block_on(fetch_receipt(&provider, tx))
        .unwrap_err();

    assert!(
        matches!(error, ReceiptEvidenceError::MissingReceipt { hash } if hash == tx.hash()),
        "a null receipt is a missing receipt, got {error:?}"
    );
}

/// A node that does not serve the tracer states a capability fact, not a
/// transient failure: the trace-required caller can distinguish "retry later"
/// from "this endpoint has no call evidence" (ADR-105 D11).
#[test]
fn a_node_without_the_tracer_reports_no_capability() {
    let tx = tx();
    let transport = recorded(
        "debug_traceTransaction",
        trace_parameters(tx),
        json!({"error": {"code": -32601, "message": "Method not found: debug_traceTransaction"}}),
    );
    let provider = transport.as_alloy_provider();

    let error = degenbot_core::runtime::get_runtime()
        .block_on(fetch_trace(&provider, tx))
        .unwrap_err();

    assert!(
        matches!(error, TraceEvidenceError::Unsupported { .. }),
        "a method-not-found is a capability verdict, got {error:?}"
    );
}

/// An endpoint verified to serve another chain than the transaction's anchor
/// cannot answer for that transaction: the same hash on another chain is another
/// transaction, and admitting it would place foreign logs in this block's
/// positions (ADR-105 D1, D11).
#[test]
fn an_endpoint_bound_to_another_chain_refuses_the_evidence() {
    let tx = tx();
    let transport = recorded_entries(&[("eth_chainId", Value::Null, json!({"result": "0x2"}))]);
    let provider = transport.as_alloy_provider();
    degenbot_core::runtime::get_runtime()
        .block_on(provider.bind_to_chain(2))
        .expect("the recorded endpoint serves chain 2");

    assert!(matches!(
        degenbot_core::runtime::get_runtime()
            .block_on(fetch_receipt(&provider, tx))
            .unwrap_err(),
        ReceiptEvidenceError::ChainMismatch {
            anchor: CHAIN,
            endpoint: 2,
        }
    ));
    assert!(matches!(
        degenbot_core::runtime::get_runtime()
            .block_on(fetch_trace(&provider, tx))
            .unwrap_err(),
        TraceEvidenceError::ChainMismatch {
            anchor: CHAIN,
            endpoint: 2,
        }
    ));
    assert_eq!(
        transport.served_snapshot().requests,
        1,
        "the chain refusal answers before the evidence read is issued"
    );
}

/// The trace acquisition asks for the call tree WITH per-frame logs: without
/// `withLog` the answer carries no logs and no join could be proved.
#[test]
fn the_trace_acquisition_requests_frame_logs() {
    let tx = tx();
    let params = trace_parameters(tx);

    assert_eq!(params[1]["tracer"], json!("callTracer"));
    assert_eq!(params[1]["tracerConfig"]["withLog"], json!(true));
}

/// The trace acquisition joins a recorded call tree into the typed evidence, and
/// the served answer's own frames carry the call paths.
#[test]
fn the_trace_acquisition_parses_the_recorded_call_tree() {
    let tx = tx();
    let transport = recorded(
        "debug_traceTransaction",
        trace_parameters(tx),
        json!({
            "result": trace_frame(
                "CALL",
                0x01,
                vec![trace_log(0x01, 0x0a, 0x01)],
                vec![trace_frame("STATICCALL", 0x02, vec![trace_log(0x02, 0x0b, 0x02)], vec![])],
            )
        }),
    );
    let provider = transport.as_alloy_provider();

    let traced = degenbot_core::runtime::get_runtime()
        .block_on(fetch_trace(&provider, tx))
        .expect("the recorded call tree is trace evidence");

    assert_eq!(traced.log_count(), 2);
    assert_eq!(traced.calls().len(), 2);
    assert_eq!(traced.root().children()[0].kind(), CallKind::StaticCall);
    assert_eq!(traced.root().children()[0].call().call_path(), [0]);
}
