//! Behavior pins for the broader-corpus segmentation and per-segment report
//! arithmetic (`examples/shared/recorded_corpus.rs`'s corpus shape and density
//! segments, `examples/shared/probe_report.rs`'s per-segment write overlap,
//! span attribution, ceiling, and live cross-check).
//!
//! Every test name states the behavior it pins. The inputs are synthetic rows,
//! synthetic captured blocks, and synthetic spans: the numbers under test are
//! the recorded counts and modeled spans the report is computed from, never a
//! measured wall.

#![expect(
    clippy::expect_used,
    reason = "the synthetic fixtures fail loudly on a broken account"
)]

use std::collections::BTreeSet;

use alloy::primitives::{Address, B256, U256};

// The shared benchmark modules carry more surface than one consumer drives
// (the probes divide it); the pins below read the parts this suite owns.
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

use probe_report::{
    attribute_segment_spans, cross_check_live, segment_ceiling, write_overlap, StorageAccounting,
    WorkUnit,
};
use real_coverage::{RealBlock, RealCoverage};
use recorded_corpus::{
    anchor_profiles, classify_density, segments_by_density, shape, DensityRegime, Recorded,
};
use wall_ledger::{Span, WallBucket};

/// The identity a `tag`/`slot` pair names in these fixtures.
fn key(tag: u8, slot: u64) -> (Address, U256) {
    (Address::new([tag; 20]), U256::from(slot))
}

/// One recorded candidate at `block`, its read walk grouped by address.
fn row(block: u64, reads: &[(u8, &[u64])]) -> Recorded {
    Recorded {
        block,
        sim_ns: 1,
        reads: reads
            .iter()
            .map(|(tag, slots)| {
                (
                    key(*tag, 0).0,
                    slots.iter().map(|slot| U256::from(*slot)).collect(),
                )
            })
            .collect(),
        relevance: None,
    }
}

#[test]
fn the_corpus_shape_counts_candidates_anchors_and_keys_in_the_manager_shapes() {
    let rows = vec![
        row(7, &[(0x11, &[1, 2])]),
        row(7, &[(0x11, &[1])]),
        row(9, &[(0x22, &[3])]),
    ];
    let shape = shape(&rows);
    assert_eq!(shape.candidates, 3, "three recorded candidates: {shape:?}");
    assert_eq!(
        shape.candidate_anchors, 2,
        "two blocks carry candidates: {shape:?}"
    );
    assert_eq!(
        shape.candidate_anchor_keys, 4,
        "every recorded slot read counts once, repeats included: {shape:?}"
    );
    assert_eq!(
        shape.anchored_keys, 3,
        "the cold leg reads each anchor's distinct identities: {shape:?}"
    );
    assert_eq!(
        shape.global_keys, 3,
        "the global key set dedups across anchors: {shape:?}"
    );
    assert_eq!(
        shape.contracts, 2,
        "distinct addresses, not slots: {shape:?}"
    );
}

#[test]
fn a_density_regime_is_the_anchor_fan_out_size_at_the_declared_thresholds() {
    assert_eq!(
        classify_density(1),
        DensityRegime::Sparse,
        "a trickle anchor is sparse"
    );
    assert_eq!(
        classify_density(14),
        DensityRegime::Sparse,
        "the sparse regime reaches to fourteen candidates"
    );
    assert_eq!(
        classify_density(15),
        DensityRegime::Moderate,
        "fifteen candidates is already moderate"
    );
    assert_eq!(
        classify_density(50),
        DensityRegime::Moderate,
        "the moderate regime reaches to fifty candidates"
    );
    assert_eq!(
        classify_density(51),
        DensityRegime::Burst,
        "fifty-one candidates is already a burst"
    );
}

/// The four-fan-out corpus the segmentation pins read: two sparse anchors
/// (blocks 10 and 13), one moderate (11), one burst (12), over three keys.
fn four_fanouts() -> Vec<Recorded> {
    let mut rows = Vec::new();
    for _ in 0..2 {
        rows.push(row(10, &[(0x11, &[1, 2])]));
    }
    for _ in 0..15 {
        rows.push(row(11, &[(0x11, &[1]), (0x22, &[1])]));
    }
    for _ in 0..51 {
        rows.push(row(12, &[(0x11, &[1]), (0x22, &[1])]));
    }
    for _ in 0..3 {
        rows.push(row(13, &[(0x11, &[2])]));
    }
    rows
}

#[test]
fn the_corpus_segments_into_one_segment_per_density_regime_it_actually_records() {
    let segments = segments_by_density(&four_fanouts());
    assert_eq!(
        segments.iter().map(|s| s.regime).collect::<Vec<_>>(),
        vec![
            DensityRegime::Sparse,
            DensityRegime::Moderate,
            DensityRegime::Burst
        ],
        "one segment per recorded regime, in first-appearance order: {segments:?}"
    );
    let sparse = &segments[0];
    assert_eq!(
        sparse.anchors,
        vec![10, 13],
        "both sparse anchors share one segment whatever blocks sit between them"
    );
    assert_eq!(
        sparse.candidates, 5,
        "the segment counts its candidates: {sparse:?}"
    );
    assert_eq!(
        sparse.read_calls, 7,
        "every recorded read counts: {sparse:?}"
    );
    assert_eq!(sparse.keys.len(), 2, "two distinct identities: {sparse:?}");
    assert_eq!(
        segments[1].candidates, 15,
        "the moderate segment counts its own fan-out: {:?}",
        segments[1]
    );
    assert_eq!(
        segments[2].candidates, 51,
        "the burst segment counts its own fan-out: {:?}",
        segments[2]
    );
    let without_burst: Vec<Recorded> = four_fanouts()
        .into_iter()
        .filter(|row| row.block != 12)
        .collect();
    let segments = segments_by_density(&without_burst);
    assert_eq!(
        segments.len(),
        2,
        "a regime the corpus never recorded gets no fabricated segment: {segments:?}"
    );
}

#[test]
fn a_key_is_new_where_it_first_appears_and_repeated_in_every_later_segment() {
    let segments = segments_by_density(&four_fanouts());
    let counts: Vec<(usize, usize)> = segments
        .iter()
        .map(|s| (s.new_keys, s.repeated_keys))
        .collect();
    assert_eq!(
        counts,
        vec![(2, 0), (1, 1), (0, 2)],
        "the three corpus keys are new exactly at their first anchor's segment and repeated later"
    );
    for segment in &segments {
        assert_eq!(
            segment.new_keys + segment.repeated_keys,
            segment.keys.len(),
            "new and repeated partition the segment's distinct keys: {segment:?}"
        );
    }
}

#[test]
fn key_age_counts_blocks_since_first_appearance_and_survival_counts_the_anchors_that_read_the_key()
{
    let segments = segments_by_density(&four_fanouts());
    let sparse = &segments[0];
    assert_eq!(
        (
            sparse.key_age_sum_blocks,
            sparse.key_age_pairs,
            sparse.key_age_max_blocks
        ),
        (3, 3, 3),
        "block 10 reads its two keys at age zero and block 13 re-reads one at age three: {sparse:?}"
    );
    assert_eq!(
        sparse.survival_anchor_sum, 5,
        "one key survives three anchors and one survives two: {sparse:?}"
    );
    assert_eq!(
        sparse.recurred_keys, 2,
        "both sparse keys more than one anchor reads: {sparse:?}"
    );
    assert_eq!(
        sparse.surviving_keys, 0,
        "nothing reads past the corpus's last anchor: {sparse:?}"
    );
    let moderate = &segments[1];
    assert_eq!(
        (
            moderate.key_age_sum_blocks,
            moderate.key_age_pairs,
            moderate.key_age_max_blocks
        ),
        (1, 2, 1),
        "the moderate anchor reads one key at age one and one new at age zero: {moderate:?}"
    );
    assert_eq!(
        moderate.surviving_keys, 2,
        "the burst anchor past it re-reads both moderate keys: {moderate:?}"
    );
}

#[test]
fn each_anchor_profile_carries_its_fan_out_size_and_read_set_novelty() {
    let profiles = anchor_profiles(&four_fanouts());
    let rows: Vec<(u64, DensityRegime, usize, usize, usize, usize)> = profiles
        .iter()
        .map(|p| {
            (
                p.block,
                p.regime,
                p.candidates,
                p.read_calls,
                p.keys,
                p.new_keys,
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (10, DensityRegime::Sparse, 2, 4, 2, 2),
            (11, DensityRegime::Moderate, 15, 30, 2, 1),
            (12, DensityRegime::Burst, 51, 102, 2, 0),
            (13, DensityRegime::Sparse, 3, 3, 1, 0),
        ],
        "every anchor carries its regime, fan-out, reads, distinct keys, and novel keys"
    );
}

/// One synthetic span on a leg's timeline.
fn span(bucket: WallBucket, start_ns: u64, end_ns: u64) -> Span {
    Span {
        bucket,
        start_ns,
        end_ns,
    }
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
    RealCoverage::from_blocks(vec![], blocks).expect("a linked captured stretch")
}

#[test]
fn every_recorded_span_is_owned_by_exactly_one_work_unit_and_folds_per_segment() {
    let spans = vec![
        span(WallBucket::StorageWait, 0, 100),
        span(WallBucket::RecordedExec, 100, 150),
        span(WallBucket::StorageWait, 150, 250),
        span(WallBucket::BalFetchDecode, 250, 260),
        span(WallBucket::StorageWait, 300, 320),
    ];
    let units = vec![
        WorkUnit {
            first_span: 0,
            end_span: 2,
            segment: 0,
        },
        WorkUnit {
            first_span: 2,
            end_span: 4,
            segment: 1,
        },
        WorkUnit {
            first_span: 4,
            end_span: 5,
            segment: 0,
        },
    ];
    let gross = attribute_segment_spans(&units, &spans, 2).expect("the units own every span");
    assert_eq!(
        gross[0][WallBucket::StorageWait.index()],
        120,
        "a segment's gross storage wait sums its units' spans: {gross:?}"
    );
    assert_eq!(
        gross[0][WallBucket::RecordedExec.index()],
        50,
        "the buckets fold per segment, not per unit: {gross:?}"
    );
    assert_eq!(
        gross[1][WallBucket::BalFetchDecode.index()],
        10,
        "the second segment owns its own units' spans: {gross:?}"
    );
    let gap = vec![
        WorkUnit {
            first_span: 0,
            end_span: 2,
            segment: 0,
        },
        WorkUnit {
            first_span: 3,
            end_span: 5,
            segment: 1,
        },
    ];
    let error = attribute_segment_spans(&gap, &spans, 2)
        .expect_err("a span no work unit owns is a broken instrumentation record");
    assert!(
        error.contains("span 2"),
        "the failure names the unowned span: {error}"
    );
    let tail = vec![WorkUnit {
        first_span: 0,
        end_span: 2,
        segment: 0,
    }];
    assert!(
        attribute_segment_spans(&tail, &spans, 2).is_err(),
        "spans past the last unit are never silently dropped"
    );
    let stray = vec![
        WorkUnit {
            first_span: 0,
            end_span: 2,
            segment: 0,
        },
        WorkUnit {
            first_span: 2,
            end_span: 5,
            segment: 2,
        },
    ];
    assert!(
        attribute_segment_spans(&stray, &spans, 2).is_err(),
        "a unit naming a segment the run never built is a broken attribution"
    );
}

#[test]
fn the_segment_ceiling_is_the_gross_storage_wait_the_carry_removes_over_the_segment_cold_wall() {
    let ceiling = segment_ceiling(600, 100, 1_000).expect("a measured segment");
    assert_eq!(
        ceiling.eliminated_gross_ns, 500,
        "cold minus carry removes the difference: {ceiling:?}"
    );
    assert_eq!(
        ceiling.share_thousandths, 500,
        "the share is over the segment's cold wall: {ceiling:?}"
    );
    assert_eq!(
        segment_ceiling(100, 150, 1_000)
            .expect("a measured segment")
            .eliminated_gross_ns,
        0,
        "a carry that pays more storage wait than cold removes nothing (never negative)"
    );
    assert!(
        segment_ceiling(100, 50, 0).is_err(),
        "a segment with no cold wall has no share to report"
    );
}

#[test]
fn the_segment_write_overlap_counts_the_read_set_keys_the_real_lists_write_in_the_span() {
    let writes = [(10, key(0x22, 1)), (11, key(0x11, 1)), (12, key(0x22, 1))];
    let real = fixture(10, 12, &writes);
    let keys: BTreeSet<(Address, U256)> = [key(0x11, 1), key(0x22, 1), key(0x33, 1)]
        .into_iter()
        .collect();
    let overlap = write_overlap(&keys, &real, 10, 12).expect("the span is captured");
    assert_eq!(overlap.keys, 3, "the read set's size: {overlap:?}");
    assert_eq!(
        overlap.written_in_span, 2,
        "two of the read set's keys the span's lists write: {overlap:?}"
    );
    assert_eq!(
        overlap.span_writes, 3,
        "every write identity over the read set counts, repeats across blocks included: {overlap:?}"
    );
    let late = write_overlap(&keys, &real, 12, 12).expect("the span is captured");
    assert_eq!(
        (late.written_in_span, late.span_writes),
        (1, 1),
        "a narrower span sees only its own block's writes: {late:?}"
    );
}

#[test]
fn the_live_cross_check_is_green_only_on_exact_accounting_and_names_the_discrepancy() {
    let live = StorageAccounting {
        keys: 4,
        carried: 1,
        final_write: 2,
        dynamic: 1,
    };
    assert!(
        cross_check_live(&live, &live).is_ok(),
        "the replay's accounting matching the live probe's is green"
    );
    let off = StorageAccounting { dynamic: 2, ..live };
    let error = cross_check_live(&off, &live).expect_err("a discrepancy is never hidden");
    assert!(
        error.contains("dynamic") && error.contains('2') && error.contains('1'),
        "the failure names the field and both counts: {error}"
    );
}
