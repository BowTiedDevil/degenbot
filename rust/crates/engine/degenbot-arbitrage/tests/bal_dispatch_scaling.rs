//! The dispatch-scaling harness's own pins: corpus identity, cross-lane verdict
//! parity, the OFF/legacy no-BAL posture, the cold controls' fallback shape, the
//! ready generation's coverage, and the latency model's arithmetic.
//!
//! The probe (`examples/bal_dispatch_scaling_probe.rs`) prints the matrix; these
//! tests pin the properties the matrix would be meaningless without, on a small
//! slice of it (two sizes, zero added latency, one repetition). No test here
//! asserts a wall: timing belongs to the report, never to a gate.
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
use scaling::{corpus, run, Leg};

/// Two fan-out sizes: one candidate, and one larger than the production cap so
/// the cap's own effect is inside the slice.
const COUNTS: [usize; 2] = [1, 4];

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

    // The relevance set the head applies through names the corpus's own pools.
    for pool in &corpus.pools {
        assert!(
            corpus.relevance.contains(pool),
            "the production relevance set names every pool the corpus routes\n"
        );
    }
}

#[test]
fn every_leg_of_a_point_produces_the_same_candidate_verdicts() {
    let bench = run(&COUNTS, &[StorageLatency::ZERO], 1).expect("the chain settles every head");
    assert_eq!(bench.points.len(), COUNTS.len());
    for point in &bench.points {
        assert!(
            point.parity(),
            "count {}: every leg's verdict surface must be the legacy lane's",
            point.count
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
    let bench = run(&COUNTS, &[StorageLatency::ZERO], 1).expect("the chain settles every head");
    for point in &bench.points {
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
                assert_eq!(run.census.storage_singles, 0);
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
                assert_eq!(run.census.storage_singles, 0, "no candidate read a slot");
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
fn the_latency_model_is_the_recorded_wire_s_own_arithmetic() {
    let model = StorageLatency::STORAGE_400US;
    let bench = run(&[4], &[model], 1).expect("the chain settles every head");
    let point = bench.points.first().expect("one point");
    let identities = u64::try_from(point.prefetch_identities).expect("a member count");

    for run in point.leg(Leg::OffLegacy) {
        // One batch round trip plus one charge per member: nothing else in this
        // corpus is a storage packet.
        let expected = model.batch_round + identities * model.batch_member;
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
                "a ready fan-out dispatches no storage packet"
            );
        }
    }

    // The zero point charges nothing, whatever the wire did.
    let bench = run(&[4], &[StorageLatency::ZERO], 1).expect("the chain settles every head");
    let point = bench.points.first().expect("one point");
    for run in &point.runs {
        assert_eq!(run.modeled_us, 0, "{} models no wall", run.leg.label());
    }
}
