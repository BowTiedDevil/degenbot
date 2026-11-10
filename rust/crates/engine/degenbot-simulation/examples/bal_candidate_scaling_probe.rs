#![expect(
    clippy::expect_used,
    clippy::print_stderr,
    clippy::print_stdout,
    reason = "a development probe prints its report and fails loudly on a failed calibration"
)]

//! The analytical BAL candidate-scaling report: the two-corpus calibration
//! closure of the shared scaling model (`shared/candidate_scaling.rs` — model,
//! assumptions, and what it does NOT claim live there), the scenario matrix
//! over candidate count x family x independent novelty at both calibrated
//! price points, and the W/F sensitivity.
//!
//! Every number is a formula over the committed corpus aggregates — no
//! candidate is executed, nothing sleeps, and a million candidates cost the
//! same arithmetic as one. `BAL_SCALING_PLANT_MISMATCH=1` takes the red
//! calibration path: the gate fails and the process exits nonzero.
//!
//! Invocation: `just bal-candidate-scaling-probe` (red probe:
//! `BAL_SCALING_PLANT_MISMATCH=1 just bal-candidate-scaling-probe`). Migration:
//! none for consumers — development analysis surface only.

#[path = "shared/candidate_scaling.rs"]
#[expect(dead_code)]
mod candidate_scaling;

use candidate_scaling::{
    calibrate, calibrate_planted, probe_exit_code, scenario_report, CalibrationReport, PricePoint,
    Scenario, ScenarioFamily, CORPORA, PLANT_ENV,
};

/// The scenario matrix's candidate counts.
const CANDIDATE_COUNTS: [u64; 4] = [1_000, 10_000, 100_000, 1_000_000];

/// The novelty sweep's values — the axis the matrix varies independently of
/// the candidate count.
const NOVELTIES: [f64; 7] = [0.0, 0.05, 0.131, 0.35, 0.6, 0.852, 1.0];

/// The W/F sensitivity's bounds.
const BOUNDS: [(u64, u64); 4] = [(1, 8), (2, 8), (2, 16), (4, 8)];

fn main() {
    let planted = std::env::var(PLANT_ENV).is_ok_and(|value| value == "1");
    let outcome = if planted {
        calibrate_planted()
    } else {
        calibrate(&CORPORA)
    };
    match &outcome {
        Ok(report) => print_report(report),
        Err(error) => eprintln!("calibration gate FAILED: {error}"),
    }
    std::process::exit(probe_exit_code(&outcome));
}

fn print_report(report: &CalibrationReport) {
    println!("# BAL analytical candidate-scaling model — calibration closure");
    println!();
    println!(
        "Calibrated against BOTH real-coverage corpora at W=2/F=8 (tolerance \
         10% relative on the ceilings, 10% on the logical rounds)."
    );
    println!();
    print_closure(report);
    print_round_closure(report);
    print_aggregates(report);
    print_coefficients(report);
    print_matrix();
    print_sweep();
    print_sensitivity();
    print_not_modeled();
}

fn print_closure(report: &CalibrationReport) {
    println!(
        "| corpus | point | ceiling measured | ceiling modeled | error | eliminated wait measured | modeled | cold wall measured | modeled |"
    );
    println!("| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |");
    for row in &report.rows {
        println!(
            "| {} | {} | {} | {} | {:+.1}% | {} | {} | {} | {} |",
            row.corpus,
            row.point.label(),
            percent(row.measured_ceiling_pm),
            percent(row.modeled_ceiling_pm),
            relative_percent(row.error_pm),
            row.measured_eliminated_us,
            row.modeled_eliminated_us,
            row.measured_cold_wall_us,
            row.modeled_cold_wall_us,
        );
    }
    println!();
}

fn print_round_closure(report: &CalibrationReport) {
    println!("## Logical round closure");
    println!();
    println!(
        "| corpus | point | eliminated rounds measured | modeled | error | carry rounds measured | modeled | error | cold rounds measured | modeled | error |"
    );
    println!("| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |");
    for row in &report.rows {
        println!(
            "| {} | {} | {} | {} | {:+.1}% | {} | {} | {:+.1}% | {} | {} | {} |",
            row.corpus,
            row.point.label(),
            row.measured_eliminated_rounds,
            row.modeled_eliminated_rounds,
            relative_percent(row.eliminated_rounds_error_pm),
            row.measured_carry_rounds,
            row.modeled_carry_rounds,
            relative_percent(row.carry_rounds_error_pm),
            optional(row.measured_cold_rounds),
            row.modeled_cold_rounds,
            optional_signed(row.cold_rounds_error_pm),
        );
    }
    println!();
}

fn print_aggregates(report: &CalibrationReport) {
    println!("## Carry-hit reproduction (exact)");
    println!();
    println!("| corpus | keys | carried | final-write | dynamic |");
    println!("| --- | ---: | ---: | ---: | ---: |");
    for row in &report.aggregates {
        println!(
            "| {} | {} | {} | {} | {} |",
            row.corpus,
            row.modeled.keys,
            row.modeled.carried,
            row.modeled.final_write,
            row.modeled.dynamic
        );
    }
    println!();
}

fn print_coefficients(report: &CalibrationReport) {
    println!("## Calibrated coefficients");
    println!();
    let rates = &report.coefficients;
    println!("| coefficient | value | provenance |");
    println!("| --- | ---: | --- |");
    println!(
        "| eliminated coalescing width (keys/round) | {:.4} original / {:.4} broader / {:.4} pooled | eliminated gross spans over the storage price |",
        rates.eliminated_width_original,
        rates.eliminated_width_broader,
        rates.eliminated_width_pooled
    );
    println!(
        "| residual coalescing width (members/round) | {:.4} | carry-round structure, pooled over both corpora |",
        rates.residual_width
    );
    println!(
        "| eliminated partition survival | {:.4} (200/400) / {:.4} (700/1200) | eliminated bucket over its gross spans, pooled |",
        rates.eliminated_survival[0],
        rates.eliminated_survival[1]
    );
    println!(
        "| residual partition survival | {:.4} (200/400) / {:.4} (700/1200) | carry-leg storage bucket over its modeled rounds, pooled |",
        rates.residual_survival[0],
        rates.residual_survival[1]
    );
    println!();
}

fn print_matrix() {
    println!("## Scenario matrix — candidate count x family (novelty varied independently)");
    println!();
    for point in PricePoint::ALL {
        println!("### {}", point.label());
        println!();
        println!(
            "| candidates | family | novelty | universe keys | cold fetches | carried | final-write | dynamic | rounds cold/carry | eliminated wait us | ceiling | snapshot entries | clone est us |"
        );
        println!("| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |");
        for candidates in CANDIDATE_COUNTS {
            for family in [
                ScenarioFamily::Repeated,
                ScenarioFamily::SteadyState,
                ScenarioFamily::Discovery,
            ] {
                let scenario = Scenario {
                    candidates,
                    family,
                    novelty: family.novelty(),
                    point,
                    lanes: 2,
                    slots: 8,
                };
                let row = scenario_report(&scenario).expect("a matrix scenario is in domain");
                println!(
                    "| {} | {} | {:.3} | {} | {} | {} | {} | {} | {}/{} | {} | {} | {} | {} |",
                    candidates,
                    family.label(),
                    scenario.novelty,
                    row.universe_keys,
                    row.cold_fetches,
                    row.carried,
                    row.final_write,
                    row.dynamic,
                    row.cold_rounds,
                    row.carry_rounds,
                    row.eliminated_wait_us,
                    percent(row.ceiling_pm),
                    row.snapshot_entries,
                    row.clone_estimate_us,
                );
            }
        }
        println!();
    }
}

fn print_sweep() {
    println!("## Novelty sweep — 100,000 candidates, steady-state shape, both points");
    println!();
    println!(
        "| novelty | universe keys | cold fetches | carried | dynamic | ceiling 200/400 | ceiling 700/1200 | snapshot entries |"
    );
    println!("| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |");
    for novelty in NOVELTIES {
        let optimistic = scenario_report(&Scenario {
            candidates: 100_000,
            family: ScenarioFamily::SteadyState,
            novelty,
            point: PricePoint::Optimistic,
            lanes: 2,
            slots: 8,
        })
        .expect("a sweep scenario is in domain");
        let pessimistic = scenario_report(&Scenario {
            candidates: 100_000,
            family: ScenarioFamily::SteadyState,
            novelty,
            point: PricePoint::Pessimistic,
            lanes: 2,
            slots: 8,
        })
        .expect("a sweep scenario is in domain");
        println!(
            "| {:.3} | {} | {} | {} | {} | {} | {} | {} |",
            novelty,
            optimistic.universe_keys,
            optimistic.cold_fetches,
            optimistic.carried,
            optimistic.dynamic,
            percent(optimistic.ceiling_pm),
            percent(pessimistic.ceiling_pm),
            optimistic.snapshot_entries,
        );
    }
    println!();
}

fn print_sensitivity() {
    println!(
        "## W/F sensitivity — 100,000 candidates, steady-state, novelty 0.131, 700/1200us (documented rule, NOT measured)"
    );
    println!();
    println!(
        "| W/F | cold rounds | carry rounds | eliminated wait us | ceiling | snapshot entries |"
    );
    println!("| --- | ---: | ---: | ---: | ---: | ---: |");
    for (lanes, slots) in BOUNDS {
        let row = scenario_report(&Scenario {
            candidates: 100_000,
            family: ScenarioFamily::SteadyState,
            novelty: 0.131,
            point: PricePoint::Pessimistic,
            lanes,
            slots,
        })
        .expect("a sensitivity scenario is in domain");
        println!(
            "| {}/{} | {} | {} | {} | {} | {} |",
            lanes,
            slots,
            row.cold_rounds,
            row.carry_rounds,
            row.eliminated_wait_us,
            percent(row.ceiling_pm),
            row.snapshot_entries,
        );
    }
    println!();
}

fn print_not_modeled() {
    println!("## NOT MODELED");
    println!();
    println!(
        "- actual p95 wall: no completion percentiles and no wall-clock parity; the scheduler's occupancy and overlap live only in the calibrated partition survivals."
    );
    println!(
        "- lock contention: unmodeled and unmeasured — nothing here speaks to scheduler lock behavior."
    );
    println!(
        "- allocation cost: the snapshot is an entry count plus a linear clone extrapolation of the measured 100,000-entry leg, never an allocator measurement."
    );
    println!("- economic outcomes: no candidate verdicts and no value capture are modeled.");
    println!();
    println!(
        "W/F beyond the measured W=2/F=8 is the documented coalescing-width scaling rule (widths scale with the lane-slot count, capped at one anchor's keys) — a sensitivity, not a measurement."
    );
}

/// A per-mille share as percent, one decimal.
fn percent(pm: u64) -> String {
    format!("{}.{:01}%", pm / 10, pm % 10)
}

/// A per-mille relative error as signed percent, one decimal.
fn relative_percent(pm: i64) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "closure errors stay far below 2^53"
    )]
    let scaled = pm as f64 / 10.0;
    scaled
}

/// An optional count as a table cell.
fn optional(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_string(), |count| count.to_string())
}

/// An optional signed per-mille error as a table cell.
fn optional_signed(value: Option<i64>) -> String {
    value.map_or_else(
        || "-".to_string(),
        |pm| format!("{:+.1}%", relative_percent(pm)),
    )
}
