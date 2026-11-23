//! Shared replay-fixture helpers for the Aave updater suites.
//!
//! The hand-built capture ledgers (`aave_discount_token_generation`,
//! `aave_endpoint_generation`, `aave_market_reset`) state only the FILTERED logs a
//! fetch pass would return. A run also acquires each candidate transaction's block
//! header and its complete receipt (ADR-105 D2), so a synthetic capture owes both.
//! Deriving them from the ledger's own `eth_getLogs` answers keeps one fixture per
//! capture instead of two hand-kept copies of the same logs drifting apart.

#![expect(clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};

use degenbot_rpc::cassette::{entry_digest, entry_key, CassetteEntry, CassetteResponse};
use serde_json::{json, Value};

/// One wire quantity of a fixture log, as the decimal the canonical cassette
/// states.
fn quantity(log: &Value, field: &str) -> u64 {
    log[field]
        .as_str()
        .expect("a fixture log states every coordinate it is grouped by")
        .parse()
        .expect("a fixture log's coordinate is a decimal quantity")
}

/// A fixture log's block-wide position, as the receipt's own order states it.
fn log_index(log: &Value) -> u64 {
    quantity(log, "logIndex")
}

/// The evidence entries a synthetic capture owes a candidate acquisition: one
/// block header per candidate height and one COMPLETE receipt per candidate
/// transaction, both derived from the ledger's recorded `eth_getLogs` answers.
///
/// `header` states the block identity a height's header serves (the synthetic
/// captures pin one block hash across every height). The returned entries are
/// keyed exactly as the recorder writes them, so a caller extends its ledger with
/// them directly.
#[must_use]
pub fn candidate_evidence_entries(
    entries: &BTreeMap<String, CassetteEntry>,
    header: impl Fn(u64) -> Value,
) -> Vec<(String, CassetteEntry)> {
    let mut logs_by_tx: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut blocks: BTreeSet<u64> = BTreeSet::new();
    for entry in entries.values() {
        let CassetteResponse::Success { result } = &entry.response else {
            continue;
        };
        let Some(logs) = result.as_array() else {
            continue;
        };
        for log in logs {
            if log.get("logIndex").is_none() {
                continue;
            }
            let block: u64 = quantity(log, "blockNumber");
            let tx = log["transactionHash"]
                .as_str()
                .expect("a fixture log states its transaction")
                .to_string();
            blocks.insert(block);
            logs_by_tx.entry(tx).or_default().push(log.clone());
        }
    }

    let mut out = Vec::new();
    for block in blocks {
        let response = CassetteResponse::Success {
            result: header(block),
        };
        out.push((
            entry_key(
                "eth_getBlockByNumber",
                &json!([format!("0x{block:x}"), false]),
            ),
            CassetteEntry {
                digest: entry_digest(&response),
                response,
            },
        ));
    }
    for (tx, mut logs) in logs_by_tx {
        // Order by position AND content, so the dedupe below can be exact: the
        // SAME log answered by two passes collapses to one, while two DIFFERENT
        // logs claiming one coordinate both survive and reach the receipt's own
        // LogOrder refusal instead of a fixture quietly picking a winner.
        logs.sort_by_key(|log| {
            (
                log_index(log),
                serde_json::to_string(log).expect("a parsed fixture log serializes"),
            )
        });
        logs.dedup();
        let first = &logs[0];
        let response = CassetteResponse::Success {
            result: json!({
                "transactionHash": tx,
                "transactionIndex": first["transactionIndex"].clone(),
                "blockHash": first["blockHash"].clone(),
                "blockNumber": first["blockNumber"].clone(),
                "status": "0x1",
                "logs": logs,
            }),
        };
        out.push((
            entry_key("eth_getTransactionReceipt", &json!([tx])),
            CassetteEntry {
                digest: entry_digest(&response),
                response,
            },
        ));
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;

    use degenbot_core::canonical_head::ChainAnchor;
    use degenbot_rpc::tx_evidence::{ReceiptRefusal, ReceiptTransaction};

    fn log(index: u64, data: &str) -> Value {
        json!({
            "address": "0x0000000000000000000000000000000000000001",
            "blockHash": format!("0x{:064x}", 0xbb_u64),
            "blockNumber": "100",
            "data": data,
            "logIndex": index.to_string(),
            "removed": false,
            "topics": [],
            "transactionHash": format!("0x{:064x}", 0xaa_u64),
            "transactionIndex": "0",
        })
    }

    fn ledger(logs: Vec<Value>) -> BTreeMap<String, CassetteEntry> {
        let response = CassetteResponse::Success {
            result: Value::Array(logs),
        };
        let mut entries = BTreeMap::new();
        entries.insert(
            entry_key("eth_getLogs", &Value::Null),
            CassetteEntry {
                digest: entry_digest(&response),
                response,
            },
        );
        entries
    }

    fn derived_receipt(entries: &BTreeMap<String, CassetteEntry>) -> Value {
        let entry = candidate_evidence_entries(entries, |block| {
            json!({
                "number": block.to_string(),
                "hash": format!("0x{:064x}", 0xbb_u64),
                "parentHash": format!("0x{:064x}", 0xba_u64),
            })
        })
        .into_iter()
        .find(|(key, _)| key.contains("eth_getTransactionReceipt"))
        .expect("a ledger with logs owes one receipt")
        .1;
        let CassetteResponse::Success { result } = entry.response else {
            panic!("the derived receipt is an answer");
        };
        result
    }

    fn anchor() -> ChainAnchor {
        ChainAnchor::new(
            1,
            100,
            format!("0x{:064x}", 0xbb_u64).parse().expect("block hash"),
            format!("0x{:064x}", 0xba_u64).parse().expect("parent hash"),
        )
        .expect("the fixture block identity is complete")
    }

    /// The same log answered by two passes is ONE log: the dedupe collapses
    /// identical values at one coordinate instead of recording it twice.
    #[test]
    fn identical_answers_at_one_coordinate_collapse() {
        let receipt = derived_receipt(&ledger(vec![
            log(0, "0xaa"),
            log(1, "0xbb"),
            log(0, "0xaa"),
        ]));

        let logs = receipt["logs"]
            .as_array()
            .expect("a receipt states its logs");
        assert_eq!(logs.len(), 2, "two positions, not three");
        assert_eq!(logs[0]["logIndex"], "0");
        assert_eq!(logs[1]["logIndex"], "1");
        ReceiptTransaction::from_receipt_json(anchor(), &receipt)
            .expect("the collapsed receipt is valid evidence");
    }

    /// Two DIFFERENT logs claiming one coordinate are contradictory evidence: the
    /// derivation preserves both, so the receipt's own order validation refuses
    /// them rather than a fixture quietly choosing one.
    #[test]
    fn conflicting_answers_at_one_coordinate_are_preserved_for_the_refusal() {
        let receipt = derived_receipt(&ledger(vec![log(0, "0xaa"), log(0, "0xbb")]));

        let logs = receipt["logs"]
            .as_array()
            .expect("a receipt states its logs");
        assert_eq!(
            logs.len(),
            2,
            "conflicting content at one coordinate is not collapsed"
        );
        assert_eq!(logs[0]["logIndex"], logs[1]["logIndex"]);
        assert_eq!(
            ReceiptTransaction::from_receipt_json(anchor(), &receipt).unwrap_err(),
            ReceiptRefusal::LogOrder {
                previous: 0,
                actual: 0,
            },
            "the receipt refuses the duplicate position"
        );
    }
}
