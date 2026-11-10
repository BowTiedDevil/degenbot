//! The dispatch-scaling harness's own pins: corpus identity, real candidate
//! storage reads and their value sensitivity, cross-lane verdict parity, the
//! OFF/legacy no-BAL posture with its dynamic read, the covered and unknown read
//! controls, the cold controls' fallback shape, the ready generation's coverage,
//! and the latency model's arithmetic.
//!
//! The probe (`examples/bal_dispatch_scaling_probe.rs`) prints the matrix; these
//! tests pin the properties the matrix would be meaningless without, on a small
//! slice of it (two sizes, both words, zero added latency, one repetition). No
//! test here asserts a wall: timing belongs to the report, never to a gate.
//!
//! Regeneration and interpretation notes live beside the probe in
//! `logs/bal_dispatch_scaling_result.md`.

#![expect(
    clippy::expect_used,
    reason = "the scripted chain and its assertions fail loudly on a broken fixture"
)]

use degenbot_arbitrage::MAX_SIMULATE_CONCURRENT;

#[path = "../examples/shared/dispatch_probe.rs"]
#[expect(dead_code)]
mod probe;
#[path = "../examples/shared/dispatch_scaling.rs"]
#[expect(dead_code)]
mod scaling;

use probe::{StorageLatency, BLOCK};
use scaling::{
    corpus, read_identity, run, Leg, Point, ScriptedWord, EXECUTOR, NONZERO_WORD, READ_SLOT,
};

/// Two fan-out sizes: one candidate, and one larger than the production cap so
/// the cap's own effect is inside the slice.
const COUNTS: [usize; 2] = [1, 4];

/// The zero-latency model: the CPU/scheduler/wire-construction cost alone.
const ZERO: StorageLatency = StorageLatency::ZERO;

fn slice(words: &[ScriptedWord]) -> Vec<Point> {
    run(&COUNTS, words, &[ZERO], 1)
        .expect("the chain settles every head")
        .points
}

fn point(points: &[Point], count: usize, word: ScriptedWord) -> &Point {
    points
        .iter()
        .find(|point| point.count == count && point.word == word)
        .expect("every count and word is in the slice")
}

#[test]
fn the_corpus_is_one_route_shape_over_distinct_pool_identities() {
    let corpus = corpus(4);
    assert_eq!(corpus.count, 4);
    assert_eq!(corpus.candidates.len(), 4);
    assert_eq!(corpus.pools.len(), 8, "two distinct pools per candidate");

    let distinct: std::collections::HashSet<_> = corpus.pools.iter().collect();
    assert_eq!(distinct.len(), 8, "no candidate shares a pool identity");

    let ids: Vec<u64> = corpus
        .candidates
        .iter()
        .map(|candidate| candidate.candidate_id.path_id())
        .collect();
    assert_eq!(ids, vec![0, 1, 2, 3], "the caller's order is the id order");
    for candidate in &corpus.candidates {
        assert_eq!(candidate.solve_block, BLOCK);
        assert_eq!(candidate.path_info.hops.len(), 2);
    }

    // The lagging leg moves the anchor and nothing else.
    let lagged = corpus.lagged();
    assert_eq!(lagged.len(), corpus.candidates.len());
    for (lagged, original) in lagged.iter().zip(&corpus.candidates) {
        assert_eq!(lagged.solve_block, BLOCK - 1);
        assert_eq!(lagged.candidate_id, original.candidate_id);
        assert_eq!(
            format!("{:?}", lagged.path_info),
            format!("{:?}", original.path_info),
            "the lagging leg moves the anchor, never the path"
        );
    }

    // The relevance set the head applies through names the corpus's own pools
    // plus the one declared fixture fact: the executor whose own storage the
    // scripted frame reads.
    for pool in &corpus.pools {
        assert!(
            corpus.relevance.contains(pool),
            "the production relevance set names every pool the corpus routes\n"
        );
    }
    assert!(
        corpus.relevance.contains(&EXECUTOR),
        "the reader's own account is the corpus's declared read subject"
    );
    assert_eq!(read_identity(), (EXECUTOR, READ_SLOT));
}

#[test]
fn every_simulated_candidate_executes_the_scripted_read() {
    let points = slice(&ScriptedWord::ALL);
    for point in &points {
        for leg in Leg::ALL {
            for run in point.leg(leg) {
                assert_eq!(
                    run.result_rows,
                    run.simulated,
                    "count {} word {} {}: every simulated candidate returns a result row",
                    point.count,
                    point.word.label(),
                    leg.label()
                );
                assert_eq!(
                    run.read_rows,
                    run.simulated,
                    "count {} word {} {}: every result row's post-execution access list \
                     names the executor's read slot",
                    point.count,
                    point.word.label(),
                    leg.label()
                );
                assert_eq!(
                    run.fail_count,
                    0,
                    "count {} word {} {}: the reading frame returns a receipt, it does \
                     not revert",
                    point.count,
                    point.word.label(),
                    leg.label()
                );
                let read = format!("{EXECUTOR}:0x{READ_SLOT:064x}");
                assert_eq!(run.verdicts.len(), run.simulated);
                for (_, row) in &run.verdicts {
                    assert!(
                        row.contains(&format!("reads=[{read}]")),
                        "count {} word {} {}: row `{row}` carries the read witness",
                        point.count,
                        point.word.label(),
                        leg.label()
                    );
                }
            }
        }
    }
}

#[test]
fn the_stored_word_decides_the_candidate_outcome_category() {
    let points = slice(&ScriptedWord::ALL);
    let zero = point(&points, 1, ScriptedWord::Zero);
    let nonzero = point(&points, 1, ScriptedWord::Nonzero);

    for leg in Leg::ALL {
        for run in zero.leg(leg) {
            assert_eq!(
                run.verdicts.len(),
                1,
                "one result row per simulated candidate"
            );
            let row = &run.verdicts[0].1;
            assert!(
                row.starts_with("unprofitable gross=1 net=0 "),
                "{}: the zero word takes the minimal-retained arm, got `{row}`",
                leg.label()
            );
        }
        for run in nonzero.leg(leg) {
            let row = &run.verdicts[0].1;
            assert!(
                row.starts_with(&format!("profitable gross={NONZERO_WORD} ")),
                "{}: the nonzero word returns itself, got `{row}`",
                leg.label()
            );
            // The fixture is value-sensitive: the SAME candidate, head, state
            // and lane produce a different category and figure per word.
            assert_ne!(
                run.verdicts,
                zero.leg(leg)[0].verdicts,
                "{}: the two words must not produce the same verdict rows",
                leg.label()
            );
        }
    }
}

#[test]
fn every_leg_of_a_point_produces_the_same_candidate_verdicts() {
    let points = slice(&ScriptedWord::ALL);
    assert_eq!(points.len(), COUNTS.len() * ScriptedWord::ALL.len());
    for point in &points {
        assert!(
            point.parity(),
            "count {} word {}: every leg's verdict surface, read witness included, \
             must be the legacy lane's",
            point.count,
            point.word.label()
        );
        let simulated = point.count.min(MAX_SIMULATE_CONCURRENT);
        assert_eq!(
            point.simulated, simulated,
            "count {}: the production cap decides what runs",
            point.count
        );
        let reference = point.leg(Leg::OffLegacy).first().copied().expect("a run");
        assert_eq!(
            reference.verdicts.len(),
            simulated,
            "one verdict row per simulated candidate"
        );
        for run in &point.runs {
            assert_eq!(run.simulated, simulated);
            assert_eq!(run.candidate_count, simulated);
            assert_eq!(run.fail_count, reference.fail_count);
            assert_eq!(run.exception_count, reference.exception_count);
            assert_eq!(run.fail_buckets, reference.fail_buckets);
            assert_eq!(run.verdicts, reference.verdicts);
        }
    }
}

#[test]
fn the_off_and_cold_legs_request_no_bal_and_pay_the_legacy_batch() {
    let points = slice(&ScriptedWord::ALL);
    for point in &points {
        let identities = u64::try_from(point.prefetch_identities).expect("a member count");
        assert!(
            identities > 0,
            "count {}: the corpus derives slots",
            point.count
        );

        for leg in [Leg::OffLegacy, Leg::ColdUnprepared, Leg::ColdUndelivered] {
            for run in point.leg(leg) {
                assert_eq!(
                    run.census.bal_fetches,
                    0,
                    "count {}: {} fetched a BAL on the dispatch leg",
                    point.count,
                    leg.label()
                );
                assert_eq!(
                    run.census.storage_batches,
                    1,
                    "count {}: {} pays the ONE legacy prefetch batch",
                    point.count,
                    leg.label()
                );
                assert_eq!(run.census.storage_members, identities);
                assert_eq!(
                    run.census.storage_singles,
                    1,
                    "count {}: {} pays ONE dynamic read for the candidate's own \
                     slot, which no path derivation knows",
                    point.count,
                    leg.label()
                );
            }
        }

        for leg in [Leg::ReadyCurrent, Leg::ReadyPrevious] {
            for run in point.leg(leg) {
                assert_eq!(
                    run.lane,
                    "delta",
                    "{} reads the settled generation",
                    leg.label()
                );
                assert_eq!(
                    run.census.storage_batches, 0,
                    "a ready head prefetches nothing"
                );
                assert_eq!(run.census.storage_members, 0);
                assert_eq!(
                    run.census.storage_singles, 0,
                    "the covered read costs no dynamic serial read"
                );
                assert_eq!(
                    run.census.bal_fetches, 0,
                    "the pipeline fetched it, not the fan-out"
                );
                let generation = run.generation.as_ref().expect("a delta leg settles heads");
                assert_eq!(generation.anchors.len(), 3, "three linked heads settled");
                assert_eq!(
                    generation.identities_covered, generation.identities_checked,
                    "the settled generation answers every identity the legacy batch fetched"
                );
                assert!(generation.identities_checked > 0);
            }
        }

        // The controls report the closed legacy reason, never a delta lane.
        for run in point.leg(Leg::ColdUnprepared) {
            assert_eq!(run.lane, "legacy/unprepared");
        }
        for run in point.leg(Leg::ColdUndelivered) {
            assert_eq!(run.lane, "legacy/undelivered");
        }
    }
}

#[test]
fn the_covered_read_is_served_by_the_generation_with_no_dynamic_read() {
    let points = slice(&ScriptedWord::ALL);
    for point in &points {
        for leg in [Leg::ReadyCurrent, Leg::ReadyPrevious] {
            for run in point.leg(leg) {
                assert_eq!(run.read_rows, run.simulated);
                assert_eq!(
                    run.census.storage_singles,
                    0,
                    "count {} word {} {}: a covered key is served from the generation",
                    point.count,
                    point.word.label(),
                    leg.label()
                );
                let generation = run.generation.as_ref().expect("a ready generation");
                assert_eq!(
                    generation.read_value,
                    Some(point.word.value()),
                    "count {} word {}: the generation serves the scripted word itself",
                    point.count,
                    point.word.label()
                );
            }
        }
    }
}

#[test]
fn the_unknown_read_falls_back_to_one_dynamic_read_with_the_right_value() {
    let points = slice(&ScriptedWord::ALL);
    for point in &points {
        let covered = point
            .leg(Leg::ReadyCurrent)
            .first()
            .copied()
            .expect("the covered control");
        for run in point.leg(Leg::ReadyCurrentUnknown) {
            assert_eq!(run.lane, "delta", "the head still settled ready");
            assert_eq!(
                run.census.storage_batches, 0,
                "the ready lane still prefetches nothing"
            );
            assert_eq!(
                run.census.storage_singles,
                1,
                "count {} word {}: the deliberately omitted key costs exactly ONE \
                 dynamic serial read (the candidates run serially on one hub)",
                point.count,
                point.word.label()
            );
            assert_eq!(run.read_rows, run.simulated);
            let generation = run.generation.as_ref().expect("a ready generation");
            assert_eq!(
                generation.read_value, None,
                "the unknown list never carried the read identity"
            );
            assert_eq!(
                run.verdicts,
                covered.verdicts,
                "count {} word {}: the fallback served the same word, so the rows \
                 are the covered control's",
                point.count,
                point.word.label()
            );
            assert_eq!(run.fail_count, 0);
        }
    }
}

#[test]
fn the_latency_model_is_the_recorded_wire_s_own_arithmetic() {
    let model = StorageLatency::STORAGE_400US;
    let bench = run(&[4], &ScriptedWord::ALL, &[model], 1).expect("the chain settles every head");
    for point in &bench.points {
        let identities = u64::try_from(point.prefetch_identities).expect("a member count");

        for run in point.leg(Leg::OffLegacy) {
            // One batch round trip plus one charge per member, then the one
            // dynamic read the derivation cannot cover.
            let expected = model.batch_round + identities * model.batch_member + model.single;
            assert_eq!(run.modeled_us, expected);
            assert_eq!(run.census.modeled_us(&model), expected);
            assert!(
                run.wall_us >= run.modeled_us,
                "the modeled wall is inside the measured one ({}us < {}us)",
                run.wall_us,
                run.modeled_us
            );
        }

        for leg in [Leg::ReadyCurrent, Leg::ReadyPrevious] {
            for run in point.leg(leg) {
                assert_eq!(
                    run.modeled_us, 0,
                    "a covered ready fan-out dispatches no storage packet"
                );
            }
        }

        // The unknown control prices exactly the fallback it measured.
        for run in point.leg(Leg::ReadyCurrentUnknown) {
            assert_eq!(run.census.storage_singles, 1);
            assert_eq!(run.modeled_us, model.single);
            assert!(
                run.wall_us >= run.modeled_us,
                "the serial read's modeled wall is inside the measured one"
            );
        }
    }

    // The zero point charges nothing, whatever the wire did.
    let bench = run(&[4], &ScriptedWord::ALL, &[ZERO], 1).expect("the chain settles every head");
    for point in &bench.points {
        for run in &point.runs {
            assert_eq!(run.modeled_us, 0, "{} models no wall", run.leg.label());
        }
    }
}
