//! Behavior pins for the wall-clock ceiling benchmark's instrumentation
//! (`examples/bal_wall_ceiling_probe.rs` and its shared report modules): the
//! bucket partition and its reconciliation, the modeled cost model's charge
//! classification, the eliminable storage-wait ceiling, the completion series,
//! the snapshot-hit classification, and the cross-leg read-digest gate.
//!
//! Every test name states the behavior it pins. Timing is never asserted: the
//! numbers under test are the modeled durations and synthetic spans the report
//! is computed from, not measured wall.

#![expect(
    clippy::expect_used,
    reason = "the synthetic fixtures fail loudly on a broken account"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use alloy::primitives::{keccak256, Address, U256};

use degenbot_simulation::sim::evm2::bal_prefetch::{decode_bal_seed, BalPlan};

// The shared benchmark modules carry more surface than one consumer drives
// (the probes divide it); the pins below read the parts this suite owns.
#[path = "../examples/shared/modeled_chain.rs"]
#[expect(dead_code)]
mod modeled_chain;
#[path = "../examples/shared/probe_report.rs"]
#[expect(dead_code)]
mod probe_report;
#[path = "../examples/shared/recorded_corpus.rs"]
#[expect(dead_code)]
mod recorded_corpus;
#[path = "../examples/shared/wall_ledger.rs"]
#[expect(dead_code)]
mod wall_ledger;

use modeled_chain::{
    bal_bytes, block_writes, modeled_value, write_selected, write_value, ChainState,
};
use probe_report::{classify_hits, eliminable_ceiling, identity_gate, HitSplit};
use recorded_corpus::{
    anchor_keys, candidate_addresses, group_by_anchor, parse_trace_line, read_call_counts,
    universe, Recorded,
};
use wall_ledger::{
    completion_stats, reconcile, Node, ReadKind, Span, WallBucket, WallLedger, WallReport,
};

/// One synthetic span on the leg's timeline.
fn span(bucket: WallBucket, start_ns: u64, end_ns: u64) -> Span {
    Span {
        bucket,
        start_ns,
        end_ns,
    }
}

fn key(tag: u8) -> (Address, U256) {
    (Address::new([tag; 20]), U256::from(u64::from(tag)))
}

#[test]
fn the_six_buckets_sum_to_the_measured_wall_with_gaps_in_scheduler_other() {
    let spans = vec![
        span(WallBucket::RecordedExec, 0, 100),
        span(WallBucket::StorageWait, 100, 250),
        span(WallBucket::BalFetchDecode, 300, 340),
    ];
    let report = reconcile(&spans, 400).expect("the spans fit the window");
    let sum: u64 = report.buckets.iter().sum();
    assert_eq!(
        sum, 400,
        "the six buckets sum to the measured wall: {report:?}"
    );
    assert_eq!(
        report.buckets[WallBucket::SchedulerOther.index()],
        110,
        "the uncovered wall is scheduler/other: {report:?}"
    );
    assert_eq!(report.overlap_ns, 0, "nothing overlaps: {report:?}");
}

#[test]
fn a_wall_moment_two_spans_share_is_charged_once_and_counted_as_overlap() {
    // A mixed round: the storage price and the account price start together.
    let spans = vec![
        span(WallBucket::StorageWait, 0, 200),
        span(WallBucket::AccountWait, 0, 400),
    ];
    let report = reconcile(&spans, 400).expect("the spans fit the window");
    assert_eq!(
        report.buckets[WallBucket::StorageWait.index()],
        200,
        "the shared 200ns is charged once, to the earlier bucket in precedence: {report:?}"
    );
    assert_eq!(
        report.buckets[WallBucket::AccountWait.index()],
        200,
        "the account wait keeps only the tail the storage span does not cover: {report:?}"
    );
    assert_eq!(
        report.overlap_ns, 200,
        "the resolved double occupancy is reported: {report:?}"
    );
    let sum: u64 = report.buckets.iter().sum();
    assert_eq!(
        sum, 400,
        "the six buckets still sum to the measured wall: {report:?}"
    );
}

#[test]
fn a_shared_moment_is_charged_to_the_earlier_bucket_in_precedence() {
    let spans = vec![
        span(WallBucket::AccountWait, 0, 300),
        span(WallBucket::RecordedExec, 100, 200),
    ];
    let report = reconcile(&spans, 300).expect("the spans fit the window");
    assert_eq!(
        report.buckets[WallBucket::RecordedExec.index()],
        100,
        "recorded execution precedes the waits: {report:?}"
    );
    assert_eq!(
        report.buckets[WallBucket::AccountWait.index()],
        200,
        "the account wait keeps only the moments nothing earlier covers: {report:?}"
    );
}

#[test]
fn a_span_past_the_measured_wall_fails_reconciliation() {
    let spans = vec![span(WallBucket::StorageWait, 300, 500)];
    let error = reconcile(&spans, 400).expect_err("an escaping span is never clipped");
    assert!(
        error.contains("escapes the measured wall"),
        "the failure names the escaping span: {error}"
    );
}

#[test]
fn the_recorded_execution_span_is_the_candidates_sim_ns() {
    let ledger = WallLedger::new();
    ledger.spend_exec(1);
    ledger.complete();
    let spans = ledger.spans();
    assert_eq!(spans.len(), 1, "one span per spent execution: {spans:?}");
    assert_eq!(spans[0].bucket, WallBucket::RecordedExec, "{spans:?}");
    assert_eq!(
        spans[0].end_ns - spans[0].start_ns,
        1,
        "the span is the recorded sim_ns, not the sleep's overshoot: {spans:?}"
    );
    assert_eq!(
        ledger.completions().len(),
        1,
        "a completed candidate lands in the completion series"
    );
}

#[test]
fn a_planted_extra_storage_delay_lands_only_in_the_storage_wait_span() {
    let unplanted = Node::new(Duration::from_micros(200), Duration::from_micros(400));
    let planted = unplanted.with_plant(Duration::from_micros(500));
    let ledger = WallLedger::new();
    planted.charge_read(ReadKind::Storage, &ledger);
    planted.charge_read(ReadKind::Account, &ledger);
    let spans = ledger.spans();
    assert_eq!(
        spans[0].end_ns - spans[0].start_ns,
        700_000,
        "the storage wait is price plus plant in ONE storage span: {spans:?}"
    );
    assert_eq!(spans[0].bucket, WallBucket::StorageWait, "{spans:?}");
    assert_eq!(
        spans[1].end_ns - spans[1].start_ns,
        400_000,
        "the account wait is untouched by the plant: {spans:?}"
    );
    assert_eq!(spans[1].bucket, WallBucket::AccountWait, "{spans:?}");
}

#[test]
fn a_mixed_round_charges_each_kind_its_own_price_and_reports_the_shared_part_once() {
    let node = Node::new(Duration::from_micros(200), Duration::from_micros(400));
    let ledger = WallLedger::new();
    node.charge_round(true, true, &ledger);
    let spans = ledger.spans();
    assert_eq!(spans.len(), 2, "one span per carried kind: {spans:?}");
    assert_eq!(spans[0].bucket, WallBucket::StorageWait, "{spans:?}");
    assert_eq!(spans[1].bucket, WallBucket::AccountWait, "{spans:?}");
    assert_eq!(
        spans[0].end_ns - spans[0].start_ns,
        200_000,
        "the storage price is charged as storage wait: {spans:?}"
    );
    assert_eq!(
        spans[1].end_ns - spans[1].start_ns,
        400_000,
        "the account price is charged as account wait: {spans:?}"
    );
    assert_eq!(
        spans[0].start_ns, spans[1].start_ns,
        "the two batches of one round start together: {spans:?}"
    );
    let wall = spans[1].end_ns;
    let report = reconcile(&spans, wall).expect("the round fits its wall");
    assert_eq!(
        report.overlap_ns, 200_000,
        "the shared part is reported once: {report:?}"
    );
    assert_eq!(
        report.buckets.iter().sum::<u64>(),
        wall,
        "the round's wall is partitioned once end to end: {report:?}"
    );
}

#[test]
fn the_eliminated_storage_wait_is_the_cold_wall_share_the_best_policy_removes() {
    let report = |wall, storage| WallReport {
        wall_ns: wall,
        buckets: {
            let mut buckets = [0; 6];
            buckets[WallBucket::StorageWait.index()] = storage;
            buckets
        },
        gross: {
            let mut gross = [0; 6];
            gross[WallBucket::StorageWait.index()] = storage;
            gross
        },
        overlap_ns: 0,
        covered_ns: storage,
    };
    let cold = report(1_000, 600);
    let seed = report(900, 300);
    let carry = report(800, 100);
    let ceiling = eliminable_ceiling(&cold, &[("seed", seed), ("carry", carry)])
        .expect("three measured legs");
    assert_eq!(
        ceiling.policy, "carry",
        "the best policy is named: {ceiling:?}"
    );
    assert_eq!(
        ceiling.eliminated_ns, 500,
        "cold minus the best residual is eliminable: {ceiling:?}"
    );
    assert_eq!(
        ceiling.share_thousandths, 500,
        "the share is over the cold leg's wall: {ceiling:?}"
    );
}

#[test]
fn the_ceiling_grows_when_the_storage_wait_bucket_grows() {
    let report = |wall, storage| WallReport {
        wall_ns: wall,
        buckets: {
            let mut buckets = [0; 6];
            buckets[WallBucket::StorageWait.index()] = storage;
            buckets
        },
        gross: {
            let mut gross = [0; 6];
            gross[WallBucket::StorageWait.index()] = storage;
            gross
        },
        overlap_ns: 0,
        covered_ns: storage,
    };
    let cold = report(1_000, 600);
    let carry = report(800, 100);
    let before = eliminable_ceiling(&cold, &[("carry", carry)]).expect("a measured leg");
    // The planted delay lands only in the storage-wait bucket: cold pays it on
    // every storage wait, the carry leg on far fewer.
    let planted_cold = report(1_500, 1_100);
    let planted_carry = report(850, 150);
    let after =
        eliminable_ceiling(&planted_cold, &[("carry", planted_carry)]).expect("a measured leg");
    assert!(
        after.eliminated_ns > before.eliminated_ns,
        "more eliminable storage wait raises the ceiling: {before:?} then {after:?}"
    );
}

#[test]
fn completion_percentiles_are_nearest_rank_over_the_completion_order() {
    let stats = completion_stats(vec![90, 10, 50, 70, 30]).expect("five completions");
    assert_eq!(stats.count, 5, "{stats:?}");
    assert_eq!(stats.first_ns, 10, "time to first candidate: {stats:?}");
    assert_eq!(stats.p50_ns, 50, "nearest-rank median of five: {stats:?}");
    assert_eq!(stats.p95_ns, 90, "nearest-rank 95th of five: {stats:?}");
}

#[test]
fn hit_classification_prefers_the_anchor_s_final_write_hits_over_carried_hits() {
    let mut read_calls = BTreeMap::new();
    read_calls.insert(key(0x11), 3);
    read_calls.insert(key(0x22), 2);
    read_calls.insert(key(0x33), 5);
    let mut writes = BTreeSet::new();
    writes.insert(key(0x11));
    writes.insert(key(0x22)); // written AND carried: the write wins the class
    let mut carried = BTreeSet::new();
    carried.insert(key(0x22));
    carried.insert(key(0x33));
    let split = classify_hits(&read_calls, &writes, &carried, 12);
    assert_eq!(
        split,
        HitSplit {
            final_write: 5,
            carried: 5,
            other: 2,
        },
        "every recorded read call lands in one class and the hub counter's residual is the rest: {split:?}"
    );
}

#[test]
fn the_corpus_parser_reads_the_trace_row_and_ignores_unrelated_lines() {
    let line = r#"DEBUG [sim] fanout-trace {"block":26375033,"cand":"45853@1","sim_ns":24848099,"reads":["0x000000000004444c5dc75cb358380d2e3de08a90|0x5f6d,0x83a1"]}"#;
    let row = parse_trace_line(line).expect("the row parses");
    assert_eq!(row.block, 26_375_033, "{row:?}");
    assert_eq!(row.sim_ns, 24_848_099, "{row:?}");
    assert_eq!(row.reads.len(), 1, "one read group: {row:?}");
    assert_eq!(row.reads[0].1.len(), 2, "two recorded slots: {row:?}");
    assert!(
        parse_trace_line("a line without a trace row").is_none(),
        "unrelated lines carry no candidate"
    );
}

#[test]
fn rows_group_into_anchors_in_file_order_and_the_universe_is_their_identities() {
    let rows = vec![
        Recorded {
            block: 7,
            sim_ns: 1,
            reads: vec![(Address::new([0x11; 20]), vec![U256::from(1u64)])],
        },
        Recorded {
            block: 7,
            sim_ns: 1,
            reads: vec![(Address::new([0x22; 20]), vec![U256::from(2u64)])],
        },
        Recorded {
            block: 9,
            sim_ns: 1,
            reads: vec![(Address::new([0x11; 20]), vec![U256::from(1u64)])],
        },
    ];
    let groups = group_by_anchor(rows.clone());
    assert_eq!(
        groups
            .iter()
            .map(|(block, rows)| (*block, rows.len()))
            .collect::<Vec<_>>(),
        vec![(7, 2), (9, 1)],
        "consecutive rows of one anchor form one fan-out, in file order"
    );
    let (keys, first, last) = universe(&rows);
    assert_eq!(
        keys.len(),
        2,
        "the universe dedups across anchors: {keys:?}"
    );
    assert_eq!(
        (first, last),
        (7, 9),
        "the universe spans the corpus's blocks"
    );
}

#[test]
fn read_call_counts_count_every_recorded_storage_read_of_an_anchor() {
    let address = Address::new([0x11; 20]);
    let rows = vec![
        Recorded {
            block: 7,
            sim_ns: 1,
            reads: vec![(address, vec![U256::from(1u64), U256::from(2u64)])],
        },
        Recorded {
            block: 7,
            sim_ns: 1,
            reads: vec![(address, vec![U256::from(1u64)])],
        },
    ];
    let counts = read_call_counts(&rows);
    assert_eq!(
        counts.get(&(address, U256::from(1u64))),
        Some(&2),
        "{counts:?}"
    );
    assert_eq!(
        counts.get(&(address, U256::from(2u64))),
        Some(&1),
        "{counts:?}"
    );
    assert_eq!(
        anchor_keys(&rows).len(),
        2,
        "duplicates collapse into one identity"
    );
    assert_eq!(
        candidate_addresses(&rows),
        vec![address],
        "the relevance set is the anchor's distinct candidate addresses, once"
    );
}

#[test]
fn a_key_s_modeled_value_changes_only_at_a_block_whose_write_bit_selects_it() {
    let first = 100u64;
    let address = Address::new([0x11; 20]);
    let slot = U256::from(7u64);
    let base = write_value(first - 1, address, slot);
    assert_eq!(
        modeled_value(first, first - 1, address, slot),
        base,
        "before the modeled range the identity holds its base value"
    );
    let selected = (first..first + 32)
        .find(|&number| write_selected(number, address, slot))
        .expect("some modeled block writes the identity");
    assert_eq!(
        modeled_value(first, selected, address, slot),
        write_value(selected, address, slot),
        "the selecting block's write value wins at that block"
    );
    if !write_selected(selected + 1, address, slot) {
        assert_eq!(
            modeled_value(first, selected + 1, address, slot),
            write_value(selected, address, slot),
            "a block that does not write the identity leaves its value alone"
        );
    }
}

#[test]
fn a_modeled_block_s_final_writes_carry_exactly_the_values_the_chain_serves_after_it() {
    let first = 100u64;
    let last = 104u64;
    let keys: BTreeSet<(Address, U256)> = [
        key(0x11),
        key(0x22),
        key(0x33),
        key(0x44),
        key(0x55),
        key(0x66),
    ]
    .into_iter()
    .collect();
    let chain = ChainState::build(&keys, first, last).expect("a modeled chain over the keys");
    let mut saw_partial = false;
    for number in first..=last {
        let writes = block_writes(first, number, &keys);
        saw_partial |= writes.len() < keys.len();
        for &(address, slot, value) in &writes {
            assert_eq!(
                modeled_value(first, number, address, slot),
                value,
                "a modeled final write carries exactly the value the chain serves after its block"
            );
        }
        for &(address, slot) in &keys {
            assert_eq!(
                chain.value_at(address, slot, number),
                Some(modeled_value(first, number, address, slot)),
                "the chain table the stub serves agrees with the chain function at every block"
            );
        }
    }
    assert!(
        saw_partial,
        "modeled write coverage is partial over the tracked identities"
    );
}

#[test]
fn the_modeled_bal_bytes_decode_through_the_production_decoder_to_the_same_writes() {
    let first = 100u64;
    let keys: BTreeSet<(Address, U256)> = [key(0x11), key(0x22), key(0x33)].into_iter().collect();
    let writes = block_writes(first, 101, &keys);
    let raw = bal_bytes(&writes);
    let seed = decode_bal_seed(&raw, &BalPlan::default()).expect("the modeled bytes are one list");
    assert_eq!(
        seed.writes, writes,
        "the production decoder reads back exactly the modeled final writes"
    );
    assert!(seed.reads.is_empty(), "the modeled lists carry writes only");
}

#[test]
fn the_identity_gate_names_the_diverged_candidate_and_refuses_an_empty_replay() {
    let mut baseline = BTreeMap::new();
    baseline.insert((26_375_033, 0), keccak256(b"one"));
    baseline.insert((26_375_033, 1), keccak256(b"two"));
    let mut diverged = baseline.clone();
    diverged.insert((26_375_033, 1), keccak256(b"other"));
    let mismatches = identity_gate(
        &baseline,
        &[
            ("seed/26375033".to_string(), baseline.clone()),
            ("carry/26375033".to_string(), diverged),
            ("cold/26375033".to_string(), BTreeMap::new()),
        ],
    );
    assert_eq!(
        mismatches.len(),
        2,
        "one divergence and one empty replay: {mismatches:?}"
    );
    assert!(
        mismatches[0].contains("(26375033, 1)"),
        "the divergence names the candidate: {}",
        mismatches[0]
    );
    assert!(
        mismatches[1].contains("replayed no candidate"),
        "an empty replay is a failure, not a pass: {}",
        mismatches[1]
    );
    assert!(
        identity_gate(
            &baseline,
            &[("seed/26375033".to_string(), baseline.clone())]
        )
        .is_empty(),
        "an identical replay is green"
    );
}
