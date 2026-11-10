//! The controlled BAL dispatch-scaling probe: the production dispatch entry
//! point over ONE scripted head, ONE candidate list, ONE deployment declaration
//! and ONE provider state, from a single candidate to a burst fan-out, with the
//! storage representation the only variable.
//!
//! # What this adds over the live OFF/ON pair
//!
//! `logs/bal_live_ab_v2_result.md` proved readiness and a small-fan-out
//! DIRECTION, and said outright what it could not do: price the mechanism. Its
//! two windows ran at disjoint heads over different candidate mixes, so no
//! fan-out had a counterpart and the delta lane's benefit at scale stayed
//! unmeasured. This probe removes the confound by scripting the chain and the
//! market: every leg of a point runs the same head, the same candidates, the
//! same deployment and the same scripted storage chain, so the lanes differ
//! only in how candidate storage is represented — the legacy per-anchor memo
//! plus its ONE prefetch batch, or an already-settled BAL delta generation the
//! session's canonical-head pipeline prepared off the fan-out's path.
//!
//! # The corpus reads, and the word is a variable
//!
//! Every simulated candidate executes a minimal value-sensitive storage reader
//! ([`scaling::read_runtime`]): one SLOAD of the executor's own slot
//! ([`scaling::READ_SLOT`]) and a two-word `execute` receipt whose
//! `gross_profit` is the frame's answer to that word. Every point runs BOTH
//! words — zero and nonzero — over the same candidates, head, deployment and
//! provider state, so a lane serving a wrong word moves the candidate's outcome
//! category. The read is asserted from the candidate's post-execution access
//! list, not from code inspection.
//!
//! # The legs
//!
//! - `off-legacy` — the shipped posture (a session cache without the opt-in).
//! - `on-ready-current` / `on-ready-previous` — the delta opt-in with the
//!   current (resp. previous) head settled before the fan-out, its list
//!   CARRYING the candidate's read identity: the covered-read control.
//! - `on-ready-unknown` — the same ready head settled from a list that
//!   deliberately OMITS that identity: the unknown-read control, which measures
//!   the dynamic serial fallback.
//! - `on-cold-failed` / `on-cold-undelivered` — the controls: the opt-in is
//!   on, the head settled COLD (resp. was never delivered), so the fan-out must
//!   fall back to the legacy lane, pay the same batch, and fetch no BAL.
//!
//! The matrix is followed by the same-block terminal-wait shapes
//! ([`scaling::wait_shapes`]): the exact in-flight head, the previous anchor,
//! and an explicit terminal failure, each against a deliberately slow
//! preparation, with the measured walls reported.
//!
//! The generation's list carries exactly the identities the PRODUCTION prefetch
//! derives on the legacy lane (learned by one discovery fan-out that is never
//! timed) plus, for the covered legs, the candidate's own read key, so "the
//! ready lane's dynamic-fallback exposure for this corpus is zero" is checked
//! per point instead of assumed.
//!
//! # Knobs
//!
//! `BAL_DISPATCH_SCALING_COUNTS` (default `1,4,16,50,100,500,1000`) the fan-out
//! sizes; `BAL_DISPATCH_SCALING_REPS` (default `3`) the interleaved repetitions
//! per leg. Every point runs under TWO latency models — zero added latency (the
//! CPU/scheduler cost alone) and a deterministic 400 us storage round trip with
//! a 20 us per-member charge (synthetic, NOT a measurement of reth).
//!
//! The probe prints a Markdown report on stdout and never asserts timing: the
//! numbers are the report. It DOES fail closed (exit 1) on a broken fixture — a
//! leg on the wrong lane, a cold control fetching a BAL, a ready generation
//! missing a prefetch identity, a covered leg missing the read key, an unknown
//! leg holding it, a candidate whose access list never names the read identity,
//! or any candidate verdict differing between lanes. Regeneration notes live
//! beside it in `logs/bal_dispatch_scaling_result.md`.

#![expect(
    clippy::expect_used,
    clippy::print_stdout,
    reason = "a benchmark probe prints its report and fails loudly on a broken fixture"
)]

#[path = "shared/dispatch_probe.rs"]
#[expect(dead_code)]
mod probe;
#[path = "shared/dispatch_scaling.rs"]
mod scaling;

use probe::StorageLatency;
use scaling::{Bench, Leg, LegSummary, Point, RunMeasure, ScriptedWord};

/// The default fan-out sizes: one candidate through a burst fan-out. The
/// production fan-out truncates at `MAX_SIMULATE_CONCURRENT`, so the points
/// above that cap measure the pre-filter cost over a longer input list, not a
/// larger simulated set — the report says which.
const DEFAULT_COUNTS: [usize; 7] = [1, 4, 16, 50, 100, 500, 1000];

fn main() {
    let counts = env_counts();
    let reps = env_reps();
    let words = ScriptedWord::ALL;
    let models = [StorageLatency::ZERO, StorageLatency::STORAGE_400US];
    let bench = scaling::run(&counts, &words, &models, reps)
        .expect("the scripted chain settles every head it is asked for");

    println!("# Controlled BAL dispatch scaling\n");
    println!(
        "Production dispatch entry over one scripted head / candidate list / deployment / \
         provider state. Repetitions per leg: {reps}. Counts: {counts:?}. Words: {}. \
         Latency models: {}\n",
        words
            .iter()
            .map(|word| word.label())
            .collect::<Vec<_>>()
            .join(" | "),
        models
            .iter()
            .map(StorageLatency::label)
            .collect::<Vec<_>>()
            .join(" | ")
    );
    println!("{}", scaling::render(&bench));

    let wait_shapes =
        scaling::wait_shapes().expect("the scripted chain settles the wait shapes' heads");
    println!("{}", scaling::render_wait_shapes(&wait_shapes));

    let mut failures = fixture_failures(&bench);
    wait_shape_failures(&wait_shapes, &mut failures);
    if failures.is_empty() {
        println!(
            "\nfixture gates: every leg on its lane, cold controls BAL-free, ready \
                  generations covering the prefetch set, covered legs serving the read \
                  key, unknown legs omitting it, every simulated candidate's access \
                  list naming the read, verdicts identical, and every same-block wait \
                  shape on its lane."
        );
    } else {
        println!("\nfixture gates FAILED:");
        for failure in &failures {
            println!("- {failure}");
        }
        std::process::exit(1);
    }
}

/// The wait shapes' own gates: each shape resolved the lane its name promises —
/// the in-flight shape joined the preparation and resolved delta, the previous
/// anchor did not wait and resolved delta, and the explicit terminal failure
/// fell back with its real reason. The walls are the report's, never gates.
fn wait_shape_failures(shapes: &[scaling::WaitShape], failures: &mut Vec<String>) {
    let expected = [
        ("same-block-in-flight", "delta"),
        ("previous-block", "delta"),
        ("explicit-failure", "legacy/bal-unreadable"),
    ];
    for (shape, lane) in expected {
        match shapes.iter().find(|row| row.shape == shape) {
            Some(row) if row.lane == lane => {}
            Some(row) => failures.push(format!(
                "wait shape {shape} resolved lane `{}` (expected `{lane}`)",
                row.lane
            )),
            None => failures.push(format!("wait shape {shape} did not run")),
        }
    }
}

/// Every fan-out size the run covers.
fn env_counts() -> Vec<usize> {
    match std::env::var("BAL_DISPATCH_SCALING_COUNTS") {
        Ok(raw) => raw
            .split(',')
            .map(|part| {
                part.trim()
                    .parse::<usize>()
                    .expect("a comma-separated fan-out size list")
            })
            .collect(),
        Err(_) => DEFAULT_COUNTS.to_vec(),
    }
}

/// How many interleaved repetitions each leg runs.
fn env_reps() -> usize {
    std::env::var("BAL_DISPATCH_SCALING_REPS").map_or(3, |raw| {
        raw.trim().parse::<usize>().expect("a repetition count")
    })
}

/// The fixture-identity gates every point must pass: the lanes' own labels, the
/// controls' empty dispatch wire, the ready generation's coverage, the covered
/// and unknown read controls, the read witness on every candidate, and verdict
/// parity. Timing is never a gate.
fn fixture_failures(bench: &Bench) -> Vec<String> {
    let mut failures = Vec::new();
    for point in &bench.points {
        let at = format!(
            "count {} / word {} / {}us",
            point.count,
            point.word.label(),
            point.model.batch_round
        );
        for leg in Leg::ALL {
            let Some(summary) = point.summary(leg) else {
                failures.push(format!("{at}: leg {} did not run", leg.label()));
                continue;
            };
            lane_failures(leg, summary.lane.as_str(), &at, &mut failures);
            read_failures(point, leg, &summary, &at, &mut failures);
            wire_failures(point, leg, &summary, &at, &mut failures);
        }
        if !point.parity() {
            failures.push(format!("{at}: candidate verdicts differ between lanes"));
        }
        for run in &point.runs {
            generation_failures(point, run, &at, &mut failures);
        }
    }
    failures
}

/// The lane each leg must resolve, in the production selector's own vocabulary.
fn lane_failures(leg: Leg, lane: &str, at: &str, failures: &mut Vec<String>) {
    let expected = match leg {
        Leg::OffLegacy => lane == "legacy",
        Leg::ReadyCurrent | Leg::ReadyPrevious | Leg::ReadyCurrentUnknown => lane == "delta",
        Leg::ColdFailed => lane == "legacy/lineage-refused",
        Leg::ColdUndelivered => lane == "legacy/undelivered",
    };
    if !expected {
        failures.push(format!("{at}: leg {} resolved lane `{lane}`", leg.label()));
    }
}

/// Every simulated candidate must have returned a result row whose
/// post-execution access list names the read identity.
fn read_failures(
    point: &Point,
    leg: Leg,
    summary: &LegSummary,
    at: &str,
    failures: &mut Vec<String>,
) {
    let simulated = u64::try_from(point.simulated).unwrap_or(u64::MAX);
    if summary.result_rows != simulated {
        failures.push(format!(
            "{at}: leg {} returned {} result rows for {} simulated candidates",
            leg.label(),
            summary.result_rows,
            point.simulated
        ));
    }
    if summary.read_rows != summary.result_rows {
        failures.push(format!(
            "{at}: leg {} witnesses the read in {} of {} result rows",
            leg.label(),
            summary.read_rows,
            summary.result_rows
        ));
    }
}

/// The wire each leg must dispatch: the non-ready legs pay the legacy batch and
/// ONE dynamic read for the candidate's own slot (no path derivation knows it);
/// a covered ready leg pays no batch and no dynamic read; the unknown ready leg
/// pays no batch and exactly the one deliberately omitted key.
fn wire_failures(
    point: &Point,
    leg: Leg,
    summary: &LegSummary,
    at: &str,
    failures: &mut Vec<String>,
) {
    match leg {
        Leg::OffLegacy | Leg::ColdFailed | Leg::ColdUndelivered => {
            if summary.bal != 0 {
                failures.push(format!(
                    "{at}: leg {} fetched {} BAL lists on the dispatch leg",
                    leg.label(),
                    summary.bal
                ));
            }
            let members = u64::try_from(point.prefetch_identities).unwrap_or(u64::MAX);
            if summary.batches != 1 || summary.members != members {
                failures.push(format!(
                    "{at}: leg {} paid {} batches / {} members, not the legacy 1 / {}",
                    leg.label(),
                    summary.batches,
                    summary.members,
                    point.prefetch_identities
                ));
            }
            if summary.singles != 1 {
                failures.push(format!(
                    "{at}: leg {} paid {} dynamic serial reads, not the one the \
                     derivation cannot cover",
                    leg.label(),
                    summary.singles
                ));
            }
        }
        Leg::ReadyCurrent | Leg::ReadyPrevious | Leg::ReadyCurrentUnknown => {
            if summary.batches != 0 || summary.members != 0 {
                failures.push(format!(
                    "{at}: ready leg {} paid {} batches / {} members",
                    leg.label(),
                    summary.batches,
                    summary.members
                ));
            }
            let expected = u64::from(matches!(leg, Leg::ReadyCurrentUnknown));
            if summary.singles != expected {
                failures.push(format!(
                    "{at}: leg {} paid {} dynamic serial reads, not {expected}",
                    leg.label(),
                    summary.singles
                ));
            }
        }
    }
}

/// The background generation's own gates: the prefetch coverage every ready leg
/// must keep, and the read key's presence on a covered list (and absence on the
/// unknown one).
fn generation_failures(point: &Point, run: &RunMeasure, at: &str, failures: &mut Vec<String>) {
    let Some(generation) = run.generation.as_ref() else {
        return;
    };
    if generation.identities_covered != generation.identities_checked {
        failures.push(format!(
            "{at}: ready generation covers {} of {} prefetch identities",
            generation.identities_covered, generation.identities_checked
        ));
    }
    let covered = generation.read_value == Some(point.word.value());
    match run.leg {
        Leg::ReadyCurrent | Leg::ReadyPrevious if !covered => {
            failures.push(format!(
                "{at}: covered leg {} does not serve the scripted read word",
                run.leg.label()
            ));
        }
        Leg::ReadyCurrentUnknown if generation.read_value.is_some() => {
            failures.push(format!(
                "{at}: unknown leg {} still holds the read key",
                run.leg.label()
            ));
        }
        _ => {}
    }
}
