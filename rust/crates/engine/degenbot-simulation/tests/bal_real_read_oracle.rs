//! Behavior pins for the real-candidate READ-SET oracle replay's pure layer:
//! the corpus selection keeps each row's own production relevance set, the
//! comparison reads the generation's answer against an independent cold value,
//! and the one-wei plant makes that comparison fail.
//!
//! The comparator is the whole correctness claim, so its negative control is a
//! fixture pin here rather than only a live arm: a comparator that copied the
//! cold leg would pass the live run and fail this suite.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the fixtures and their assertions fail loudly on a broken comparator"
)]

use std::collections::{BTreeMap, BTreeSet};

use alloy::primitives::{Address, U256};
use serde_json::json;

#[path = "../examples/shared/recorded_corpus.rs"]
#[expect(dead_code)]
mod recorded_corpus;

#[path = "../examples/shared/read_oracle.rs"]
#[expect(dead_code)]
mod read_oracle;

use read_oracle::{
    compare, fingerprint, load_tagged_rows, plant_one_wei, select, selection_material,
    write_overlap, ReadAnswer, ReadOrigin, TaggedRow,
};
use recorded_corpus::{parse_trace_line, RelevanceSource};

/// An address for one fixture account.
fn address(tag: u8) -> Address {
    Address::new([tag; 20])
}

/// One fixture trace row, in the recorded wire shape.
fn row(
    block: u64,
    candidate: &str,
    reads: &[(Address, &[u64])],
    relevance: Option<&[Address]>,
) -> TaggedRow {
    let reads: Vec<String> = reads
        .iter()
        .map(|(address, slots)| {
            let slots: Vec<String> = slots.iter().map(|slot| format!("0x{slot:x}")).collect();
            format!("{address:#x}|{}", slots.join(","))
        })
        .collect();
    let relevance: Option<Vec<String>> = relevance.map(|addresses| {
        addresses
            .iter()
            .map(|address| format!("{address:#x}"))
            .collect()
    });
    let mut trace = json!({
        "block": block,
        "cand": candidate,
        "sim_ns": 1,
        "reads": reads,
    });
    if let Some(relevance) = relevance {
        trace["relevance"] = json!(relevance);
    }
    let line = format!("DEBUG [sim] fanout-trace {trace}");
    TaggedRow {
        candidate: candidate.to_string(),
        row: parse_trace_line(&line).expect("the fixture row parses"),
    }
}

/// A six-anchor corpus, three rows per anchor, each row reading its own slots
/// so a selection's diversity is measurable.
fn corpus() -> Vec<TaggedRow> {
    let mut rows = Vec::new();
    for anchor in 0..6u64 {
        for row_index in 0..3u64 {
            let block = 100 + anchor;
            let candidate = format!("{}{row_index}@1", anchor + 1);
            let pool = address(u8::try_from(anchor + 1).unwrap());
            let slots = [row_index + 1, row_index + 10];
            let keys = [(pool, slots.as_slice())];
            rows.push(row(block, &candidate, &keys, Some(&[pool])));
        }
    }
    rows
}

/// The selection spreads anchors across the recorded range, keeps two rows per
/// anchor, and preserves every selected row's own production relevance set.
#[test]
fn the_selection_spreads_anchors_and_keeps_each_rows_production_relevance() {
    let rows = corpus();
    let selection = select(&rows, 3, 2).expect("the corpus selects");
    let blocks: Vec<u64> = selection
        .anchors
        .iter()
        .map(|anchor| anchor.block)
        .collect();
    assert_eq!(
        blocks,
        vec![100, 102, 105],
        "three anchors spread over six: first and last included"
    );
    assert_eq!(selection.rows(), 6);
    assert_eq!(selection.distinct_candidates().len(), 6);
    assert_eq!(selection.oracle_rows(), 0);
    for anchor in &selection.anchors {
        assert_eq!(anchor.rows.len(), 2);
        assert_eq!(
            anchor.fanout, 3,
            "the fan-out width is reported, not truncated"
        );
        for selected in &anchor.rows {
            assert_eq!(
                selected.relevance.source,
                RelevanceSource::Production,
                "a row carrying the field keeps its production derivation"
            );
            assert_eq!(
                selected.relevance.addresses,
                vec![address(u8::try_from(anchor.block - 99).unwrap())],
                "the relevance set is the row's own, never its recorded read walk"
            );
        }
        let first: BTreeSet<_> = anchor.rows[0].keys.iter().copied().collect();
        let added: Vec<_> = anchor.rows[1]
            .keys
            .iter()
            .copied()
            .filter(|key| !first.contains(key))
            .collect();
        assert!(
            !added.is_empty(),
            "the second pick adds read identities the first did not carry"
        );
    }
    assert_eq!(
        selection.read_addresses().len(),
        3,
        "one pool address per selected anchor"
    );
}

/// A legacy row has no production set, and the selection carries it only with
/// the recorded-read oracle label — never as production relevance.
#[test]
fn a_legacy_row_is_selected_only_with_its_recorded_read_oracle_label() {
    let pool = address(0x11);
    let rows = vec![row(7, "1@1", &[(pool, &[1, 2])], None)];
    let selection = select(&rows, 1, 1).expect("the legacy row selects");
    assert_eq!(selection.oracle_rows(), 1);
    let selected = &selection.anchors[0].rows[0];
    assert_eq!(
        selected.relevance.source,
        RelevanceSource::RecordedReadOracle
    );
    assert_eq!(
        selected.relevance.addresses,
        vec![pool],
        "the fallback is the recorded read addresses"
    );
}

/// The loader keeps every row beside the candidate identity the corpus model
/// drops, and a file with no trace row fails instead of replaying nothing.
#[test]
fn the_loader_reads_every_row_beside_its_candidate_and_refuses_an_empty_corpus() {
    let path =
        std::env::temp_dir().join(format!("bal_real_read_oracle_{}.log", std::process::id()));
    let pool = address(0x22);
    let text = format!(
        "a line that is not a trace row\n\
         DEBUG [sim] fanout-trace {{\"block\":5,\"cand\":\"42@1\",\"sim_ns\":1,\"verdict\":\"none\",\"gas\":0,\"reads\":[\"{pool:#x}|0x1\"],\"relevance\":[\"{pool:#x}\"]}}\n\
         DEBUG [sim] fanout-trace {{\"block\":6,\"cand\":\"43@2\",\"sim_ns\":1,\"verdict\":\"none\",\"gas\":0,\"reads\":[\"{pool:#x}|0x2\"]}}\n"
    );
    std::fs::write(&path, text).expect("the fixture file writes");
    let loaded =
        load_tagged_rows(path.to_str().expect("a utf-8 temp path")).expect("the corpus loads");
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].candidate, "42@1");
    assert_eq!(loaded[1].candidate, "43@2");
    assert_eq!(loaded[1].row.block, 6);
    assert_eq!(loaded[0].row.relevance, Some(vec![pool]));

    std::fs::write(&path, "no trace rows here\n").expect("the empty fixture writes");
    let error = load_tagged_rows(path.to_str().expect("a utf-8 temp path"))
        .expect_err("a corpus with no row is a failure");
    assert!(
        error.contains("no `fanout-trace "),
        "unexpected error: {error}"
    );
    std::fs::remove_file(&path).expect("the fixture file removes");
}

/// A zero selection budget has nothing to replay and is refused, not silently
/// widened.
#[test]
fn a_zero_selection_budget_is_refused() {
    let rows = corpus();
    assert!(select(&rows, 0, 1).is_err());
    assert!(select(&rows, 1, 0).is_err());
}

/// A snapshot answer and a dynamic-fallback answer are compared the same way,
/// and a naturally zero word is compared like any other.
#[test]
fn identical_generation_and_cold_values_compare_clean_including_zeros() {
    let answers = vec![
        ReadAnswer {
            key: (address(1), U256::from(1)),
            origin: ReadOrigin::Snapshot,
            value: U256::from(5),
        },
        ReadAnswer {
            key: (address(2), U256::from(2)),
            origin: ReadOrigin::Dynamic,
            value: U256::ZERO,
        },
        ReadAnswer {
            key: (address(3), U256::from(3)),
            origin: ReadOrigin::Snapshot,
            value: U256::ZERO,
        },
    ];
    let mut cold = BTreeMap::new();
    cold.insert((address(1), U256::from(1)), U256::from(5));
    cold.insert((address(2), U256::from(2)), U256::ZERO);
    cold.insert((address(3), U256::from(3)), U256::ZERO);
    let comparison = compare("7@1", &answers, &cold);
    assert_eq!(comparison.compared, 3);
    assert_eq!(comparison.served, 2);
    assert_eq!(comparison.dynamic, 1);
    assert_eq!(comparison.zeros, 2);
    assert_eq!(comparison.served_zeros, 1);
    assert!(comparison.mismatches.is_empty());
}

/// The negative control: one planted wei makes the comparison fail, naming the
/// candidate, identity and side that answered — a comparator that copied the
/// cold leg could never report this.
#[test]
fn a_planted_one_wei_difference_fails_the_comparison() {
    let mut answers = vec![ReadAnswer {
        key: (address(1), U256::from(9)),
        origin: ReadOrigin::Snapshot,
        value: U256::from(1_000),
    }];
    let mut cold = BTreeMap::new();
    cold.insert((address(1), U256::from(9)), U256::from(1_000));
    assert!(compare("8@1", &answers, &cold).mismatches.is_empty());
    assert!(
        plant_one_wei(&mut answers),
        "the plant applies to an answered read"
    );
    assert_eq!(answers[0].value, U256::from(1_001));
    let comparison = compare("8@1", &answers, &cold);
    assert_eq!(comparison.mismatches.len(), 1);
    let mismatch = &comparison.mismatches[0];
    assert_eq!(mismatch.candidate, "8@1");
    assert_eq!(mismatch.key, (address(1), U256::from(9)));
    assert_eq!(mismatch.origin, ReadOrigin::Snapshot);
    assert_eq!(mismatch.cold, Some(U256::from(1_000)));
    assert_eq!(mismatch.generation, U256::from(1_001));
    assert!(
        !plant_one_wei(&mut []),
        "nothing to skew is not a silent success"
    );
}

/// No cold answer for an identity is a disagreement, never a skipped read.
#[test]
fn a_missing_cold_answer_is_a_mismatch_not_a_skip() {
    let answers = vec![ReadAnswer {
        key: (address(1), U256::from(1)),
        origin: ReadOrigin::Dynamic,
        value: U256::from(3),
    }];
    let comparison = compare("9@1", &answers, &BTreeMap::new());
    assert_eq!(comparison.mismatches.len(), 1);
    assert_eq!(comparison.mismatches[0].cold, None);
    assert_eq!(comparison.mismatches[0].origin, ReadOrigin::Dynamic);
}

/// The overlap census counts read identities the anchor's list writes.
#[test]
fn the_write_overlap_counts_read_identities_the_list_writes() {
    let keys = [(address(1), U256::from(1)), (address(2), U256::from(2))];
    let writes: BTreeSet<_> = [(address(2), U256::from(2)), (address(3), U256::from(3))]
        .into_iter()
        .collect();
    assert_eq!(write_overlap(&keys, &writes), 1);
    assert_eq!(write_overlap(&keys, &BTreeSet::new()), 0);
}

/// The fingerprints pin the corpus bytes and the selection's exact rows.
#[test]
fn the_fingerprints_are_deterministic_and_sensitive_to_their_input() {
    assert_eq!(fingerprint(b"corpus"), fingerprint(b"corpus"));
    assert_ne!(fingerprint(b"corpus"), fingerprint(b"corpuz"));
    let rows = corpus();
    let first = select(&rows, 3, 2).expect("selects");
    let second = select(&rows, 3, 2).expect("selects");
    assert_eq!(
        selection_material(&first),
        selection_material(&second),
        "the same corpus selects the same rows"
    );
    let wider = select(&rows, 3, 3).expect("selects");
    assert_ne!(
        fingerprint(selection_material(&first).as_bytes()),
        fingerprint(selection_material(&wider).as_bytes()),
        "a different selection material fingerprints differently"
    );
    let material = selection_material(&first);
    assert!(material.contains("100:"), "the material names each anchor");
    assert!(material.contains("@1"), "the material names each candidate");
}
