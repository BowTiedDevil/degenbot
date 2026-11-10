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

use alloy::primitives::{keccak256, Address, B256, U256};

use degenbot_simulation::sim::evm2::bal_prefetch::{decode_bal_seed, BalPlan, BalSeed};

// The shared benchmark modules carry more surface than one consumer drives
// (the probes divide it); the pins below read the parts this suite owns.
#[path = "../examples/shared/modeled_chain.rs"]
#[expect(dead_code)]
mod modeled_chain;
#[path = "../examples/shared/probe_report.rs"]
#[expect(dead_code)]
mod probe_report;
#[path = "../examples/shared/real_coverage.rs"]
#[expect(dead_code)]
mod real_coverage;
#[path = "../examples/shared/recorded_corpus.rs"]
#[expect(dead_code)]
mod recorded_corpus;
#[path = "../examples/shared/wall_ledger.rs"]
#[expect(dead_code)]
mod wall_ledger;

use modeled_chain::{
    bal_bytes, block_writes, modeled_value, real_block_writes, shared_value, write_selected,
    write_value, ChainState, Coverage,
};
use probe_report::{
    classify_hits, eliminable_ceiling, identity_gate, storage_accounting, HitSplit,
    StorageAccounting,
};
use real_coverage::{fold_writes, RealBlock, RealCoverage};
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

/// One captured block in the fixture's shape, linked to its predecessor.
fn captured_block(number: u64, writes: Vec<(Address, U256)>) -> RealBlock {
    RealBlock {
        number,
        hash: B256::repeat_byte(u8::try_from(number % 251).expect("a small test number")),
        parent_hash: B256::repeat_byte(
            u8::try_from((number - 1) % 251).expect("a small test number"),
        ),
        writes,
        other_writes: 0,
    }
}

/// The fixture over `first..=last` whose lists write exactly `writes`.
fn fixture(first: u64, last: u64, writes: &[(u64, (Address, U256))]) -> RealCoverage {
    let blocks = (first..=last)
        .map(|number| {
            let mut block_writes: Vec<(Address, U256)> = writes
                .iter()
                .filter(|(at, _)| *at == number)
                .map(|(_, identity)| *identity)
                .collect();
            block_writes.sort_unstable();
            captured_block(number, block_writes)
        })
        .collect();
    RealCoverage::from_blocks(vec![key(0x11).0], blocks).expect("a linked captured stretch")
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
            relevance: None,
        },
        Recorded {
            block: 7,
            sim_ns: 1,
            reads: vec![(Address::new([0x22; 20]), vec![U256::from(2u64)])],
            relevance: None,
        },
        Recorded {
            block: 9,
            sim_ns: 1,
            reads: vec![(Address::new([0x11; 20]), vec![U256::from(1u64)])],
            relevance: None,
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
            relevance: None,
        },
        Recorded {
            block: 7,
            sim_ns: 1,
            reads: vec![(address, vec![U256::from(1u64)])],
            relevance: None,
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

#[test]
fn the_real_coverage_fixture_refuses_a_gap_or_parent_linkage_that_does_not_hold() {
    let linked = vec![
        captured_block(7, vec![]),
        captured_block(8, vec![]),
        captured_block(9, vec![]),
    ];
    assert!(
        RealCoverage::from_blocks(vec![], linked).is_ok(),
        "a contiguous stretch whose parents link is one captured fixture"
    );
    let gap = vec![captured_block(7, vec![]), captured_block(9, vec![])];
    let error =
        RealCoverage::from_blocks(vec![], gap).expect_err("a gap is never a captured stretch");
    assert!(
        error.contains("skips block 8"),
        "the failure names the gap: {error}"
    );
    let mut broken = vec![captured_block(7, vec![]), captured_block(8, vec![])];
    broken[1].parent_hash = B256::repeat_byte(0xEE);
    let error = RealCoverage::from_blocks(vec![], broken)
        .expect_err("a parent hash that does not link is never a captured stretch");
    assert!(
        error.contains("names parent"),
        "the failure names the broken link: {error}"
    );
}

#[test]
fn the_real_coverage_fixture_round_trips_through_the_documented_format() {
    let real = fixture(7, 9, &[(8, key(0x11)), (9, key(0x22))]);
    let path = std::env::temp_dir().join(format!("bal_real_coverage_{}.json", std::process::id()));
    let path = path.to_str().expect("a temp path is utf-8");
    real.save(path).expect("the fixture writes");
    let text = std::fs::read_to_string(path).expect("the fixture reads back");
    assert!(
        text.contains(r#""format": "bal-real-coverage-v1""#),
        "the file carries the documented, versioned format tag: {text}"
    );
    let loaded = RealCoverage::load(path).expect("the fixture parses");
    assert_eq!(
        (loaded.first(), loaded.last()),
        (7, 9),
        "the captured stretch survives the round trip"
    );
    assert_eq!(
        loaded.hash(8).expect("block 8 is captured"),
        real.hash(8).expect("block 8 is captured"),
        "the real block identities survive the round trip"
    );
    assert_eq!(
        loaded.writes_at(8),
        &[(key(0x11).0, key(0x11).1)],
        "the exact write identities survive the round trip"
    );
    assert!(
        loaded.is_write(9, key(0x22).0, key(0x22).1),
        "the membership lookup reads the captured identities"
    );
    assert!(
        !loaded.is_write(9, key(0x11).0, key(0x11).1),
        "an identity the list did not write is not a captured write"
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn the_capture_fold_keeps_the_tracked_addresses_writes_and_counts_the_rest() {
    let (tracked, slot_one) = key(0x11);
    let slot_two = U256::from(7u64);
    let (untracked, slot_untracked) = key(0x33);
    let seed = BalSeed {
        writes: vec![
            (tracked, slot_two, U256::from(2u64)),
            (untracked, slot_untracked, U256::from(3u64)),
            (tracked, slot_one, U256::from(1u64)),
        ],
        ..BalSeed::default()
    };
    let tracked_addresses = [tracked].into_iter().collect();
    let (writes, other_writes) = fold_writes(&seed, &tracked_addresses);
    assert_eq!(
        writes,
        vec![(tracked, slot_two), (tracked, slot_one)],
        "the fold keeps exactly the tracked addresses' write identities, in key order"
    );
    assert_eq!(
        other_writes, 1,
        "writes outside the tracked addresses are counted, never kept"
    );
}

#[test]
fn the_distinct_key_storage_accounting_splits_keys_the_way_the_live_probe_counts() {
    let keys: BTreeSet<(Address, U256)> = [key(0x11), key(0x22), key(0x33), key(0x44)]
        .into_iter()
        .collect();
    let mut writes = BTreeSet::new();
    writes.insert(key(0x11));
    writes.insert(key(0x22)); // written AND carried: the write class wins
    let mut snapshot = BTreeSet::new();
    snapshot.insert(key(0x22));
    snapshot.insert(key(0x33));
    let accounting = storage_accounting(&keys, &writes, &snapshot);
    assert_eq!(
        accounting,
        StorageAccounting {
            keys: 4,
            carried: 1,
            final_write: 2,
            dynamic: 1,
        },
        "every distinct key lands in exactly one class: {accounting:?}"
    );
}

#[test]
fn the_shared_chain_s_value_changes_only_at_a_block_both_coverages_write() {
    let first = 100u64;
    let last = 131u64;
    let (address, slot) = key(0x11);
    let base = write_value(first - 1, address, slot);
    let selected: Vec<u64> = (first..=last)
        .filter(|&number| write_selected(number, address, slot))
        .collect();
    let shared = selected[0];
    let modeled_only = *selected
        .iter()
        .find(|&&number| number > shared)
        .expect("the modeled bit selects later blocks too");
    let real_only = (shared + 1..=last)
        .find(|&number| !write_selected(number, address, slot))
        .expect("the modeled bit leaves some later block alone");
    let real = fixture(
        first,
        last,
        &[(shared, (address, slot)), (real_only, (address, slot))],
    );
    assert_eq!(
        shared_value(first, shared - 1, &real, address, slot),
        base,
        "before both coverages write, the identity holds its base value"
    );
    assert_eq!(
        shared_value(first, shared, &real, address, slot),
        write_value(shared, address, slot),
        "the block both coverages write sets the shared value"
    );
    assert_eq!(
        shared_value(first, modeled_only, &real, address, slot),
        write_value(shared, address, slot),
        "a block only the modeled bit writes leaves the shared value alone"
    );
    assert_eq!(
        shared_value(first, real_only, &real, address, slot),
        write_value(shared, address, slot),
        "a block only the real list writes leaves the shared value alone"
    );
}

#[test]
fn both_coverage_modes_serve_identical_read_values() {
    let first = 100u64;
    let last = 104u64;
    let keys: BTreeSet<(Address, U256)> = [key(0x11), key(0x22), key(0x33)].into_iter().collect();
    let real = fixture(
        first,
        last,
        &[(101, key(0x11)), (102, key(0x22)), (102, key(0x33))],
    );
    let synthetic = ChainState::build_coverage(&keys, first, last, &real, Coverage::Synthetic)
        .expect("a chain");
    let real_chain =
        ChainState::build_coverage(&keys, first, last, &real, Coverage::Real).expect("a chain");
    for &(address, slot) in &keys {
        for number in first..=last {
            assert_eq!(
                synthetic.value_at(address, slot, number),
                real_chain.value_at(address, slot, number),
                "the shared chain serves one value per identity at a block whatever coverage's \
                 lists the run replays"
            );
        }
    }
}

#[test]
fn the_real_lists_carry_exactly_the_captured_write_identities_with_shared_values() {
    let first = 100u64;
    let last = 103u64;
    let writes = [(101, key(0x11)), (101, key(0x22)), (102, key(0x33))];
    let real = fixture(first, last, &writes);
    let entries = real_block_writes(first, 101, &real);
    assert_eq!(
        entries
            .iter()
            .map(|(address, slot, _)| (*address, *slot))
            .collect::<Vec<_>>(),
        real.writes_at(101),
        "the real list is exactly the captured write identities"
    );
    for &(address, slot, value) in &entries {
        assert_eq!(
            value,
            shared_value(first, 101, &real, address, slot),
            "each entry carries the shared chain's word after the block"
        );
    }
    let seed = decode_bal_seed(&bal_bytes(&entries), &BalPlan::default())
        .expect("the real-shaped bytes are one list");
    assert_eq!(
        seed.writes, entries,
        "the production decoder reads back exactly the list's writes"
    );
}
