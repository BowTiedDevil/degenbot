//! Behavior pins for the analytical BAL candidate-scaling model
//! (`examples/shared/candidate_scaling.rs` and its development probe
//! `examples/bal_candidate_scaling_probe.rs`): the two-corpus calibration
//! closure against the committed real-coverage accounting and ceilings, the
//! carry-hit identities, the scenario aggregates and their novelty axis, the
//! edge behaviors, and the red calibration probe's nonzero disposition.
//!
//! Every test name states the behavior it pins. The numbers under test are the
//! committed corpus accounting (`bal_wall_ceiling_probe`'s live profiles,
//! `bal_carry_probe`'s set analyses, and the measured real-coverage result
//! tables) and the model's aggregate arithmetic — never a measured wall and
//! never a per-candidate replay.
//!
//! Invocation: `just bal-candidate-scaling-probe` for the report (its red
//! calibration probe: `BAL_SCALING_PLANT_MISMATCH=1 just
//! bal-candidate-scaling-probe` exits nonzero); these pins run under `just
//! test-rust-nextest -p degenbot-simulation`.
//!
//! Migration: none for consumers — the model is example/test surface only, no
//! production `src` and no wire/ABI/FFI contract.

#![expect(
    clippy::expect_used,
    reason = "the committed corpus facts are literals this suite trusts"
)]

// The shared scaling module carries more surface than one consumer drives (the
// probe and the pins divide it); the pins below read the parts this suite owns.
#[path = "../examples/shared/candidate_scaling.rs"]
#[expect(dead_code)]
mod candidate_scaling;

use candidate_scaling::{
    calibrate, carry_accounting, measured_ceiling_pm, probe_exit_code, scenario_report, PricePoint,
    Scenario, ScenarioFamily, CORPORA, ORIGINAL,
};

/// The original corpus's committed real-coverage carry accounting at W=2/F=8:
/// 5,938 anchored keys, 3,261 carried, 136 final-write, 2,541 dynamic.
#[test]
fn calibration_reproduces_the_original_corpus_members_and_carry_hit_structure_exactly() {
    let report = calibrate(&CORPORA).expect("the committed corpora calibrate");
    let row = &report.aggregates[0];
    assert_eq!(
        (
            row.modeled.keys,
            row.modeled.carried,
            row.modeled.final_write,
            row.modeled.dynamic
        ),
        (5_938, 3_261, 136, 2_541),
        "the model reproduces the live probe's counts exactly: {row:?}"
    );
    assert_eq!(
        (
            row.modeled.keys,
            row.modeled.carried,
            row.modeled.final_write,
            row.modeled.dynamic
        ),
        (
            ORIGINAL.anchored_keys,
            ORIGINAL.carried,
            ORIGINAL.final_write,
            ORIGINAL.dynamic
        ),
        "the modeled members are the committed real accounting: {row:?}"
    );
}

/// The broader corpus's committed real-coverage carry accounting at W=2/F=8:
/// 6,478 anchored keys, 3,711 carried, 164 final-write, 2,603 dynamic.
#[test]
fn calibration_reproduces_the_broader_corpus_members_and_carry_hit_structure_exactly() {
    let report = calibrate(&CORPORA).expect("the committed corpora calibrate");
    let row = &report.aggregates[1];
    assert_eq!(
        (
            row.modeled.keys,
            row.modeled.carried,
            row.modeled.final_write,
            row.modeled.dynamic
        ),
        (6_478, 3_711, 164, 2_603),
        "the model reproduces the live probe's counts exactly: {row:?}"
    );
}

/// The measured eliminable storage-wait ceilings (11.7%/26.9% original, 8.9%/
/// 22.0% broader) are what the calibration gate closes against, and the
/// model's composed estimate must land within the documented 10% relative
/// tolerance at all four calibration points.
#[test]
fn calibration_reproduces_all_four_measured_storage_wait_ceilings_within_ten_percent() {
    let report = calibrate(&CORPORA).expect("the committed corpora calibrate");
    for row in &report.rows {
        assert_eq!(
            row.measured_ceiling_pm, row.log_ceiling_pm,
            "the committed measured ceiling reproduces the result log's figure: {row:?}"
        );
        assert!(
            row.error_pm.abs() <= 100,
            "the modeled ceiling must land within 10% relative of the measured one: {row:?}"
        );
    }
    assert_eq!(
        report
            .rows
            .iter()
            .map(|r| r.log_ceiling_pm)
            .collect::<Vec<_>>(),
        vec![117, 269, 89, 220],
        "the four measured ceilings are the logs' 11.7%/26.9% and 8.9%/22.0%"
    );
}

/// The logical round structure (eliminated rounds and the cold/carry totals)
/// is reproduced from the calibrated coalescing widths within the documented
/// 10% band, so the scenario round estimates rest on a closed round model.
#[test]
fn calibration_reproduces_the_logical_round_structure_within_the_documented_band() {
    let report = calibrate(&CORPORA).expect("the committed corpora calibrate");
    for row in &report.rows {
        assert!(
            row.eliminated_rounds_error_pm.abs() <= 100,
            "eliminated rounds close within 10%: {row:?}"
        );
        assert!(
            row.carry_rounds_error_pm.abs() <= 100,
            "carry rounds close within 10%: {row:?}"
        );
        if let Some(error_pm) = row.cold_rounds_error_pm {
            assert!(
                error_pm.abs() <= 100,
                "cold rounds close within 10%: {row:?}"
            );
        }
    }
}

/// The calibration gate is a real gate: green on the committed corpora, and a
/// planted calibration mismatch fails it with a named discrepancy whose
/// disposition is the nonzero exit the probe's red path takes.
#[test]
fn the_calibration_gate_is_green_on_the_committed_corpora_and_red_with_a_nonzero_disposition_on_a_planted_mismatch(
) {
    let green = calibrate(&CORPORA);
    assert_eq!(probe_exit_code(&green), 0, "a green gate exits zero");
    let red = candidate_scaling::calibrate_planted();
    assert_eq!(
        probe_exit_code(&red),
        1,
        "a failed calibration gate is the probe's nonzero exit"
    );
    let error = red.expect_err("the planted mismatch fails the gate");
    assert!(
        error.contains("planted"),
        "the failure names the planted mismatch: {error}"
    );
}

/// The measured ceiling a calibration row closes against is the eliminable
/// storage wait over the cold wall, in per-mille of the cold wall.
#[test]
fn the_measured_ceiling_is_the_eliminated_storage_wait_over_the_cold_wall() {
    assert_eq!(
        measured_ceiling_pm(&ORIGINAL, PricePoint::Optimistic),
        117,
        "407,682us of eliminated wait over the 3,474,810us cold wall"
    );
}

/// The carry accounting identity partitions the anchored keys: a key the
/// anchor's own final writes carry is a final-write hit, a key the snapshot
/// already held is carried, and the rest are the dynamic fetch members.
#[test]
fn the_carry_accounting_identity_partitions_the_anchored_keys() {
    let accounting = carry_accounting(5_938, 2_543, 136.0 / 5_938.0, 2.0 / 2_543.0);
    assert_eq!(
        (
            accounting.keys,
            accounting.carried,
            accounting.final_write,
            accounting.dynamic
        ),
        (5_938, 3_261, 136, 2_541),
        "the precedence rules land on the live probe's split: {accounting:?}"
    );
    assert_eq!(
        accounting.carried + accounting.final_write + accounting.dynamic,
        accounting.keys,
        "the three classes partition the keys: {accounting:?}"
    );
}

/// A scenario at the model's default shape.
fn scenario(candidates: u64, family: ScenarioFamily, novelty: f64, point: PricePoint) -> Scenario {
    Scenario {
        candidates,
        family,
        novelty,
        point,
        lanes: 2,
        slots: 8,
    }
}

/// Zero candidates model zero work and a vacuous ceiling — the empty fan-out
/// is a zero report, never a division by an empty schedule.
#[test]
fn zero_candidates_model_zero_work_and_a_vacuous_ceiling() {
    let report = scenario_report(&scenario(
        0,
        ScenarioFamily::SteadyState,
        0.13,
        PricePoint::Optimistic,
    ))
    .expect("zero candidates is a valid, empty scenario");
    assert_eq!(
        (
            report.cold_fetches,
            report.carried,
            report.final_write,
            report.dynamic,
            report.cold_rounds,
            report.snapshot_entries,
            report.ceiling_pm,
        ),
        (0, 0, 0, 0, 0, 0, 0),
        "the empty scenario models no fetches, no rounds, no working set, and a vacuous ceiling: {report:?}"
    );
}

/// Full repetition (novelty zero): every candidate reads the same set, so one
/// anchor's fetches answer the whole run and everything later is carried —
/// the universe and the dynamic members stay constant while the carried hits
/// grow with the candidate count.
#[test]
fn full_repetition_fetches_one_anchor_and_carries_the_rest() {
    let small = scenario_report(&scenario(
        1_000,
        ScenarioFamily::Repeated,
        0.0,
        PricePoint::Optimistic,
    ))
    .expect("a valid scenario");
    let large = scenario_report(&scenario(
        1_000_000,
        ScenarioFamily::Repeated,
        0.0,
        PricePoint::Optimistic,
    ))
    .expect("a valid scenario");
    assert_eq!(
        small.universe_keys, large.universe_keys,
        "exact repetition never grows the key universe: {small:?} vs {large:?}"
    );
    assert_eq!(
        small.dynamic, large.dynamic,
        "exact repetition fetches the first anchor's keys exactly once at any candidate count"
    );
    assert!(
        large.carried > small.carried * 900,
        "the carried hits grow with the candidate count under exact repetition: {small:?} vs {large:?}"
    );
    assert_eq!(
        large.carried + large.final_write + large.dynamic,
        large.cold_fetches,
        "the classes still partition the keys at one million candidates: {large:?}"
    );
}

/// Full novelty (novelty one): every anchor's keys are first appearances, so
/// the carry answers nothing and every key is a dynamic fetch.
#[test]
fn full_novelty_fetches_every_key_and_carries_nothing() {
    let report = scenario_report(&scenario(
        10_000,
        ScenarioFamily::Discovery,
        1.0,
        PricePoint::Optimistic,
    ))
    .expect("a valid scenario");
    assert_eq!(
        report.carried, 0,
        "no key survives to a later anchor under full novelty: {report:?}"
    );
    assert_eq!(
        report.dynamic,
        report.cold_fetches - report.final_write,
        "every key the anchor's own writes do not answer is fetched: {report:?}"
    );
    assert_eq!(
        report.universe_keys, report.cold_fetches,
        "the universe is the whole anchored key set under full novelty: {report:?}"
    );
}

/// Novelty is an independent scenario axis, not a function of the candidate
/// count: the same novelty models the same class shares at one thousand and at
/// one million candidates, and the families' novelty derives from the corpora's
/// measured segment structure, not from their size.
#[test]
fn novelty_is_an_independent_scenario_axis_not_a_function_of_candidate_count() {
    let share = |candidates: u64| -> f64 {
        let report = scenario_report(&scenario(
            candidates,
            ScenarioFamily::SteadyState,
            0.5,
            PricePoint::Optimistic,
        ))
        .expect("a valid scenario");
        candidate_scaling::to_f64(report.dynamic) / candidate_scaling::to_f64(report.cold_fetches)
    };
    let (small_share, large_share) = (share(1_000), share(1_000_000));
    assert!(
        (small_share - large_share).abs() < 0.02,
        "the dynamic share depends on novelty, not on the candidate count ({small_share} vs {large_share})"
    );
    assert_eq!(
        (ScenarioFamily::Repeated.novelty(),
            candidate_scaling::nearest(ScenarioFamily::SteadyState.novelty() * 1_000.0),
            candidate_scaling::nearest(ScenarioFamily::Discovery.novelty() * 1_000.0)),
        (0.0, 131, 852),
        "the families' novelties are the segments' first-appearance shares: exact repetition 0, steady state 13.1% (528/4,032 steady-segment anchored pairs), discovery 85.2% (2,083/2,446 burst pairs)"
    );
}

/// A million-candidate scenario is deterministic aggregate arithmetic: the
/// same inputs produce the same report and the aggregates scale with the
/// candidate count — no candidate is ever executed or slept through.
#[test]
fn a_million_candidate_scenario_is_deterministic_aggregate_arithmetic() {
    let first = scenario_report(&scenario(
        1_000_000,
        ScenarioFamily::SteadyState,
        0.13,
        PricePoint::Pessimistic,
    ))
    .expect("a valid scenario");
    let second = scenario_report(&scenario(
        1_000_000,
        ScenarioFamily::SteadyState,
        0.13,
        PricePoint::Pessimistic,
    ))
    .expect("a valid scenario");
    assert_eq!(
        first, second,
        "the scenario report is a pure function of its inputs"
    );
    let thousand = scenario_report(&scenario(
        1_000,
        ScenarioFamily::SteadyState,
        0.13,
        PricePoint::Pessimistic,
    ))
    .expect("a valid scenario");
    let ratio = candidate_scaling::to_f64(first.cold_fetches)
        / candidate_scaling::to_f64(thousand.cold_fetches);
    assert!(
        (900.0..1_100.0).contains(&ratio),
        "the anchored fetches scale linearly with the candidate count ({ratio})"
    );
}

/// The price-point sensitivity: the same scenario at 700/1200us carries a
/// larger modeled storage wait and a larger eliminable ceiling than at
/// 200/400us, because the same rounds cost more per trip.
#[test]
fn the_storage_wait_ceiling_rises_with_the_slower_price_point() {
    let optimistic = scenario_report(&scenario(
        100_000,
        ScenarioFamily::SteadyState,
        0.13,
        PricePoint::Optimistic,
    ))
    .expect("a valid scenario");
    let pessimistic = scenario_report(&scenario(
        100_000,
        ScenarioFamily::SteadyState,
        0.13,
        PricePoint::Pessimistic,
    ))
    .expect("a valid scenario");
    assert!(
        pessimistic.eliminated_wait_us > optimistic.eliminated_wait_us,
        "the eliminated wait grows with the storage price: {optimistic:?} vs {pessimistic:?}"
    );
    assert!(
        pessimistic.ceiling_pm > optimistic.ceiling_pm,
        "the eliminable share grows when storage wait is a larger wall share: {optimistic:?} vs {pessimistic:?}"
    );
}

/// The W/F sensitivity (the documented rule, not a measurement): a wider
/// lane-slot bound coalesces more members per round, so the modeled rounds
/// shrink and the eliminable ceiling falls or holds.
#[test]
fn wider_lane_slot_bounds_widen_coalescing_and_shrink_modeled_rounds() {
    let mut baseline = scenario(
        100_000,
        ScenarioFamily::SteadyState,
        0.13,
        PricePoint::Pessimistic,
    );
    let mut doubled_slots = baseline;
    doubled_slots.slots = 16;
    let mut doubled_lanes = baseline;
    doubled_lanes.lanes = 4;
    baseline.lanes = 2;
    let report = scenario_report(&baseline).expect("a valid scenario");
    let slots = scenario_report(&doubled_slots).expect("a valid scenario");
    let lanes = scenario_report(&doubled_lanes).expect("a valid scenario");
    assert!(
        slots.cold_rounds < report.cold_rounds && lanes.cold_rounds < report.cold_rounds,
        "a wider bound models fewer, fatter rounds: {report:?} vs {slots:?} vs {lanes:?}"
    );
    assert!(
        slots.ceiling_pm <= report.ceiling_pm && lanes.ceiling_pm <= report.ceiling_pm,
        "a wider bound never models a larger eliminable share: {report:?} vs {slots:?} vs {lanes:?}"
    );
}

/// The snapshot working set grows with the key universe and the measured
/// write-only rate (the broader corpus's 1,711 BAL-only entries over its
/// 2,611-key working set), and exact repetition keeps it flat in the
/// candidate count.
#[test]
fn the_snapshot_working_set_grows_with_the_key_universe_and_the_write_only_rate() {
    let report = scenario_report(&scenario(
        10_000,
        ScenarioFamily::Discovery,
        1.0,
        PricePoint::Optimistic,
    ))
    .expect("a valid scenario");
    let expected = candidate_scaling::nearest(
        candidate_scaling::to_f64(report.universe_keys) * (1.0 + 1_711.0 / 2_611.0),
    );
    assert_eq!(
        report.snapshot_entries, expected,
        "the snapshot is the working set plus the measured write-only rate: {report:?}"
    );
    let small = scenario_report(&scenario(
        1_000,
        ScenarioFamily::Repeated,
        0.0,
        PricePoint::Optimistic,
    ))
    .expect("a valid scenario");
    let large = scenario_report(&scenario(
        1_000_000,
        ScenarioFamily::Repeated,
        0.0,
        PricePoint::Optimistic,
    ))
    .expect("a valid scenario");
    assert_eq!(
        small.snapshot_entries, large.snapshot_entries,
        "exact repetition holds the working set at one anchor's keys: {small:?} vs {large:?}"
    );
}

/// A scenario outside the model's declared domain fails loud instead of
/// modeling nonsense: novelty outside [0, 1], or a lane/slot bound of zero.
#[test]
fn invalid_scenarios_fail_loud_instead_of_modeling_nonsense() {
    for novelty in [-0.1, 1.1] {
        let invalid = scenario(
            1_000,
            ScenarioFamily::SteadyState,
            novelty,
            PricePoint::Optimistic,
        );
        assert!(
            scenario_report(&invalid).is_err(),
            "novelty {novelty} is outside the model's domain"
        );
    }
    let mut no_lanes = scenario(
        1_000,
        ScenarioFamily::SteadyState,
        0.13,
        PricePoint::Optimistic,
    );
    no_lanes.lanes = 0;
    assert!(
        scenario_report(&no_lanes).is_err(),
        "zero lanes is a broken schedule, not a scenario"
    );
    let mut no_slots = scenario(
        1_000,
        ScenarioFamily::SteadyState,
        0.13,
        PricePoint::Optimistic,
    );
    no_slots.slots = 0;
    assert!(
        scenario_report(&no_slots).is_err(),
        "zero slots is a broken schedule, not a scenario"
    );
}
