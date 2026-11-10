//! Behavior pins for the paired relevance analysis: the production-derived
//! relevance set and the recorded-read oracle scope the SAME recorded
//! candidates, so a measured difference is a relevance effect and nothing else.
//!
//! The corpus traces live in `/tmp` and are the probes' input; the pins here run
//! over synthetic rows plus the committed real write-coverage capture, so the
//! suite stays hermetic. Every test name states the behavior it pins: the
//! pairing never falls back silently, the set difference names both directions,
//! the write split shows which identities only one set can seed, the regime
//! fold refuses a missing measurement, the reuse delta's sign means "production
//! paid more", an over-window hit re-read is the conversion an idle window
//! makes, the gap percentiles are nearest-rank, and the committed production
//! capture spans exactly the manager's stretch.

#![expect(
    clippy::expect_used,
    clippy::print_stdout,
    reason = "the pins fail loudly on a broken fixture, and the shared lane driver prints its debug               lines"
)]

use std::collections::BTreeMap;

use alloy::primitives::{Address, U256};

use degenbot_simulation::sim::evm2::delta_snapshot::ReadOrigin;

#[path = "../examples/shared/probe_report.rs"]
#[expect(dead_code)]
mod probe_report;

#[path = "../examples/shared/real_coverage.rs"]
#[expect(dead_code)]
mod real_coverage;

#[path = "../examples/shared/recorded_corpus.rs"]
#[expect(dead_code)]
mod recorded_corpus;

#[path = "../examples/shared/relevance_pairs.rs"]
#[expect(dead_code)]
mod relevance_pairs;

#[path = "../examples/shared/session_drive.rs"]
#[expect(dead_code)]
mod session_drive;

#[path = "../examples/shared/wall_ledger.rs"]
#[expect(dead_code)]
mod wall_ledger;

use probe_report::StorageAccounting;
use real_coverage::RealCoverage;
use recorded_corpus::Recorded;
use relevance_pairs::{
    fold_regimes, pair_anchors, re_reads, retention_delta, summarize, traced_accounting,
    write_split, AnchorReuse, RelevanceVariant, ReuseComparison,
};
use session_drive::ReadEvent;

/// An address for one scenario account.
fn address(tag: u8) -> Address {
    Address::new([tag; 20])
}

/// One recorded candidate reading `reads`, carrying `relevance` when given.
fn row(block: u64, reads: &[(Address, &[u64])], relevance: Option<&[Address]>) -> Recorded {
    Recorded {
        block,
        sim_ns: 1,
        reads: reads
            .iter()
            .map(|(address, slots)| {
                (
                    *address,
                    slots.iter().map(|slot| U256::from(*slot)).collect(),
                )
            })
            .collect(),
        relevance: relevance.map(<[Address]>::to_vec),
    }
}

/// One fan-out pair over a single anchor.
fn fanout(block: u64, rows: Vec<Recorded>) -> (u64, Vec<Recorded>) {
    (block, rows)
}

/// A pairing over a corpus row that carries no production relevance must fail
/// loudly: reporting the recorded read addresses as production relevance is the
/// exact fallback a paired measurement cannot hide.
#[test]
fn a_pairing_refuses_a_row_without_the_production_field() {
    let corpus = vec![fanout(
        7,
        vec![
            row(7, &[(address(0x11), &[1])], Some(&[address(0x11)])),
            row(7, &[(address(0x22), &[2])], None),
        ],
    )];
    let error = pair_anchors(&corpus).expect_err("a missing field is not a pairing");
    assert!(
        error.contains("block 7") && error.contains("candidate 1"),
        "the error names the unfinished row: {error}"
    );
}

/// Both sets come from the same rows: the production field's union, and the
/// distinct recorded read addresses. Neither derivations re-reads the log.
#[test]
fn a_pairing_reads_both_sets_from_the_same_recorded_rows() {
    let corpus = vec![fanout(
        7,
        vec![
            row(
                7,
                &[(address(0x11), &[1, 2]), (address(0x22), &[3])],
                Some(&[address(0x11), address(0x33)]),
            ),
            row(
                7,
                &[(address(0x22), &[4])],
                Some(&[address(0x33), address(0x44)]),
            ),
        ],
    )];
    let pairs = pair_anchors(&corpus).expect("every row carries its field");
    assert_eq!(pairs.len(), 1, "one anchor, one pair");
    let pair = &pairs[0];
    assert_eq!(pair.block, 7);
    assert_eq!(
        pair.candidates, 2,
        "the identical fan-out both sets describe"
    );
    assert_eq!(
        pair.set(RelevanceVariant::Production),
        [address(0x11), address(0x33), address(0x44)],
        "the production set is the rows' union, first-seen order, deduped"
    );
    assert_eq!(
        pair.set(RelevanceVariant::Oracle),
        [address(0x11), address(0x22)],
        "the oracle is the distinct read addresses, in recorded order"
    );
}

/// The difference names both directions: read addresses production excludes
/// (reuse production cannot seed), and relevant addresses the walk never read
/// (seeds an oracle cannot know about).
#[test]
fn the_pair_difference_names_the_read_addresses_production_excludes() {
    let corpus = vec![fanout(
        9,
        vec![row(
            9,
            &[(address(0x22), &[1]), (address(0x33), &[2])],
            Some(&[address(0x22), address(0x44)]),
        )],
    )];
    let pair = &pair_anchors(&corpus).expect("the row carries its field")[0];
    let difference = pair.difference();
    assert_eq!(
        difference.excluded_read,
        [address(0x33)],
        "0x33 was read and is outside production relevance"
    );
    assert_eq!(
        difference.unread_relevant,
        [address(0x44)],
        "0x44 is production-relevant and was never read"
    );
    assert_eq!(difference.shared, 1, "0x22 is both read and relevant");
}

/// The block's real writes split by relevance membership: one identity only the
/// oracle's set can seed at an unknown slot — the mechanism behind any reuse
/// difference the replay measures.
#[test]
fn the_block_write_split_names_the_identities_only_the_oracle_can_seed() {
    let writes = [
        (address(0x22), U256::from(1u64)),
        (address(0x33), U256::from(2u64)),
        (address(0x33), U256::from(3u64)),
    ];
    let production = write_split(&writes, &[address(0x22)]);
    assert_eq!(production.inside, 1);
    assert_eq!(production.outside, 2, "both 0x33 writes need a known slot");
    let oracle = write_split(&writes, &[address(0x22), address(0x33)]);
    assert_eq!(oracle.inside, 3);
    assert_eq!(oracle.outside, 0, "the oracle names every written address");
}

/// One reuse measurement per variant.
fn reuse(dynamic: usize, carried: usize, seeds: usize, snapshot: usize) -> AnchorReuse {
    AnchorReuse {
        storage: StorageAccounting {
            keys: dynamic + carried,
            carried,
            final_write: 0,
            dynamic,
        },
        seeds,
        snapshot,
        ..AnchorReuse::default()
    }
}

/// Regimes fold the anchors the measured maps hold, and a missing measurement is
/// refused instead of folded as zero.
#[test]
fn the_regime_fold_sums_one_density_and_refuses_a_missing_anchor() {
    let corpus = vec![
        fanout(
            10,
            vec![row(10, &[(address(0x40), &[1])], Some(&[address(0x41)]))],
        ),
        fanout(
            20,
            (0..20)
                .map(|_| row(20, &[(address(0x50), &[1])], Some(&[address(0x51)])))
                .collect(),
        ),
    ];
    let production: BTreeMap<u64, AnchorReuse> =
        [(10, reuse(5, 1, 2, 9)), (20, reuse(7, 3, 4, 30))]
            .into_iter()
            .collect();
    let oracle: BTreeMap<u64, AnchorReuse> = [(10, reuse(4, 2, 2, 9)), (20, reuse(6, 4, 4, 31))]
        .into_iter()
        .collect();
    let pairs = pair_anchors(&corpus).expect("both anchors carry their field");
    let regimes = fold_regimes(&pairs, &production, &oracle).expect("both anchors measured");
    assert_eq!(regimes.len(), 2, "one row per recorded regime");
    let sparse = &regimes[0];
    assert_eq!(sparse.regime.label(), "sparse");
    assert_eq!((sparse.anchors, sparse.candidates), (1, 1));
    assert_eq!(sparse.production_addresses, 1);
    assert_eq!(sparse.oracle_addresses, 1);
    assert_eq!(
        (sparse.excluded_read, sparse.unread_relevant),
        (1, 1),
        "the anchor read 0x40 and named only 0x41: one excluded read, one unread \
         relevant address"
    );
    let moderate = &regimes[1];
    assert_eq!(moderate.regime.label(), "moderate");
    assert_eq!(moderate.candidates, 20);
    assert_eq!((moderate.excluded_read, moderate.unread_relevant), (1, 1));
    assert_eq!(
        moderate.reuse.dynamic_delta(),
        1,
        "the regime's production leg paid one more dynamic member"
    );
    assert_eq!(moderate.reuse.snapshot_delta(), -1);

    let missing: BTreeMap<u64, AnchorReuse> = [(10, reuse(5, 1, 2, 9))].into_iter().collect();
    let error = fold_regimes(&pairs, &missing, &oracle).expect_err("block 20 is unmeasured");
    assert!(
        error.contains("block 20"),
        "the fold names the gap: {error}"
    );
}

/// The reuse delta's sign means "production paid more": dynamic members and
/// snapshot entries both point the same way.
#[test]
fn the_reuse_delta_is_positive_where_production_pays_more_dynamic_members() {
    let comparison = ReuseComparison::new(reuse(12, 7, 2, 40), reuse(9, 10, 3, 42));
    assert_eq!(
        comparison.dynamic_delta(),
        3,
        "production paid 3 more fetches"
    );
    assert_eq!(comparison.carried_delta(), -3, "and carried 3 fewer hits");
    assert_eq!(comparison.seed_delta(), -1, "with one fewer seed");
    assert_eq!(comparison.snapshot_delta(), -2, "and a smaller snapshot");
    assert_eq!(
        ReuseComparison::new(reuse(4, 1, 1, 5), reuse(4, 1, 1, 5)).dynamic_delta(),
        0,
        "identical sets measure identically"
    );
    let applied = ReuseComparison::new(
        AnchorReuse {
            overwrites: 12,
            skipped: 5,
            ..AnchorReuse::default()
        },
        AnchorReuse {
            overwrites: 14,
            skipped: 3,
            ..AnchorReuse::default()
        },
    );
    assert_eq!(
        applied.overwrite_delta(),
        -2,
        "the oracle applied two more known-slot overwrites"
    );
    assert_eq!(applied.skip_delta(), 2, "and left two fewer writes unknown");
}

/// A key's answer path is what the leg traced: the anchor's own final write wins
/// even when the hub also served it, a key every read of which the chain answered
/// is carried, and a key any read of which took the round path is a dynamic
/// member — a same-anchor duplicate that later hits does not un-pay the member.
/// This is the accounting an expiring retention policy needs, because the
/// held-set model ignores expiry.
#[test]
fn a_traced_accounting_classifies_by_the_answer_path_the_leg_reported() {
    let written = (address(0x11), U256::from(1u64));
    let carried = (address(0x11), U256::from(2u64));
    let dynamic = (address(0x11), U256::from(3u64));
    let shared = (address(0x11), U256::from(4u64));
    let unanswered = (address(0x11), U256::from(5u64));
    let keys = [written, carried, dynamic, shared, unanswered]
        .into_iter()
        .collect();
    let applied = [written].into_iter().collect();
    let traced = |key, origin| ReadEvent {
        block: 5,
        key,
        value: U256::from(9u64),
        origin: Some(origin),
    };
    let reads = vec![
        traced(written, ReadOrigin::Hub),
        traced(carried, ReadOrigin::Snapshot),
        traced(carried, ReadOrigin::Snapshot),
        traced(dynamic, ReadOrigin::Hub),
        traced(shared, ReadOrigin::Hub),
        traced(shared, ReadOrigin::Snapshot),
    ];
    let accounting = traced_accounting(&keys, &applied, &reads);
    assert_eq!(accounting.keys, 5);
    assert_eq!(
        accounting.final_write, 1,
        "the anchor's own final write wins over the path that served it"
    );
    assert_eq!(
        accounting.carried, 1,
        "only the key every read of which the chain answered was carried"
    );
    assert_eq!(
        accounting.dynamic, 2,
        "the round-answered key and the duplicate whose first read paid a member"
    );
    assert_eq!(
        accounting.keys, 5,
        "and a key the leg never traced pays nothing, so the classes do not sum \
         to the key count — which the delta-versus-control gate turns red"
    );
}

/// A traced leg's storage read.
fn read(block: u64, slot: u64, hit: bool) -> ReadEvent {
    ReadEvent {
        block,
        key: (address(0x11), U256::from(1u64)),
        value: U256::from(slot),
        origin: Some(if hit {
            ReadOrigin::Snapshot
        } else {
            ReadOrigin::Hub
        }),
    }
}

/// A hit re-read inside the window stays a hit; one beyond it is exactly what
/// the windowed leg converts to a round, and the measured extra members are the
/// cost of that conversion.
#[test]
fn an_over_window_hit_re_read_is_what_the_idle_window_converts() {
    let unbounded = vec![read(10, 1, false), read(14, 2, true), read(30, 3, true)];
    let windowed = vec![read(10, 1, false), read(14, 2, true), read(30, 3, false)];
    let delta = retention_delta(
        &unbounded,
        &windowed,
        &AnchorReuse {
            storage_members: 9,
            live: Some(2),
            ..AnchorReuse::default()
        },
        &AnchorReuse {
            storage_members: 11,
            live: Some(1),
            ..AnchorReuse::default()
        },
        8,
    );
    assert_eq!(delta.hits, 2, "two of the three reads were re-reads");
    assert_eq!(
        delta.over_window, 1,
        "the 20-block gap is the hit the window must expire"
    );
    assert_eq!(
        delta.converted, 1,
        "the windowed leg answered it with a round"
    );
    assert_eq!(
        delta.extra_members, 2,
        "the measured cost of the conversion"
    );
    assert_eq!(delta.lost_hits, 1);
    assert_eq!(delta.live, (2, 1));

    let re_read_inside = vec![read(10, 1, false), read(11, 2, true)];
    let inside = retention_delta(
        &re_read_inside,
        &re_read_inside,
        &AnchorReuse::default(),
        &AnchorReuse::default(),
        8,
    );
    assert_eq!(
        inside.over_window, 0,
        "a gap inside the window converts nothing"
    );
    assert_eq!(inside.converted, 0);

    assert_eq!(
        re_reads(&[read(3, 1, true)]).len(),
        0,
        "a first read is not a re-read"
    );
    assert_eq!(
        re_reads(&[read(3, 1, true), read(5, 2, false)]),
        vec![relevance_pairs::ReRead {
            block: 5,
            gap: 2,
            hit: false
        }]
    );
}

/// The gap percentiles are nearest-rank, so the report's p95 is an observed gap
/// rather than an interpolation between two.
#[test]
fn the_gap_percentiles_are_nearest_rank() {
    let stats = summarize((1..=20).collect());
    assert_eq!(stats.count, 20);
    assert_eq!(
        (stats.p50, stats.p95, stats.p99, stats.max),
        (10, 19, 20, 20)
    );
    assert_eq!(
        summarize(vec![7]).p95,
        7,
        "one sample pins every percentile"
    );
    assert_eq!(summarize(Vec::new()), relevance_pairs::GapStats::default());
}

/// The committed production capture spans exactly the manager's stretch: the
/// loader's own gap/linkage checks hold, the tracked address count is the
/// corpus's, and the write identities the replay will apply are the capture's.
#[test]
fn the_committed_production_capture_spans_the_manager_range_with_its_write_identities() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/bal_real_coverage_production.json"
    );
    let real = RealCoverage::load(path).expect("the committed production capture parses");
    assert_eq!((real.first(), real.last()), (26_382_028, 26_382_051));
    for number in real.first()..=real.last() {
        assert!(
            real.block(number).is_some(),
            "block {number} of the stretch is captured"
        );
    }
    assert_eq!(
        real.write_count(),
        3_434,
        "the capture's write identities over the corpus's addresses"
    );
    let addresses: std::collections::BTreeSet<Address> = (real.first()..=real.last())
        .flat_map(|number| real.writes_at(number).iter().map(|(address, _)| *address))
        .collect();
    assert!(
        addresses.len() <= 89,
        "writes stay inside the corpus's {} addresses, saw {}",
        89,
        addresses.len()
    );
}
