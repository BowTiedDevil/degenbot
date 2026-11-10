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
//! same deployment and the same zero-valued storage chain, so the lanes differ
//! only in how candidate storage is represented — the legacy per-anchor memo
//! plus its ONE prefetch batch, or an already-settled BAL delta generation the
//! session's canonical-head pipeline prepared off the fan-out's path.
//!
//! # The legs
//!
//! - `off-legacy` — the shipped posture (a session cache without the opt-in).
//! - `on-ready-current` / `on-ready-previous` — the delta opt-in with the
//!   current (resp. previous) head settled before the fan-out.
//! - `on-cold-unprepared` / `on-cold-undelivered` — the controls: the opt-in is
//!   on, the head is not applied (resp. never delivered), so the fan-out must
//!   fall back to the legacy lane, pay the same batch, and fetch no BAL.
//!
//! The generation's list carries exactly the identities the PRODUCTION prefetch
//! derives on the legacy lane (learned by one discovery fan-out that is never
//! timed), so "the ready lane's dynamic-fallback exposure for this corpus is
//! zero" is checked per point instead of assumed.
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
//! missing a prefetch identity, or any candidate verdict differing between
//! lanes. Regeneration notes live beside it in
//! `logs/bal_dispatch_scaling_result.md`.

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
use scaling::{Bench, Leg};

/// The default fan-out sizes: one candidate through a burst fan-out. The
/// production fan-out truncates at `MAX_SIMULATE_CONCURRENT`, so the points
/// above that cap measure the pre-filter cost over a longer input list, not a
/// larger simulated set — the report says which.
const DEFAULT_COUNTS: [usize; 7] = [1, 4, 16, 50, 100, 500, 1000];

fn main() {
    let counts = env_counts();
    let reps = env_reps();
    let models = [StorageLatency::ZERO, StorageLatency::STORAGE_400US];
    let bench = scaling::run(&counts, &models, reps)
        .expect("the scripted chain settles every head it is asked for");

    println!("# Controlled BAL dispatch scaling\n");
    println!(
        "Production dispatch entry over one scripted head / candidate list / deployment / \
         provider state. Repetitions per leg: {reps}. Counts: {counts:?}. Latency models: {}\n",
        models
            .iter()
            .map(StorageLatency::label)
            .collect::<Vec<_>>()
            .join(" | ")
    );
    println!("{}", scaling::render(&bench));

    let failures = fixture_failures(&bench);
    if failures.is_empty() {
        println!(
            "\nfixture gates: every leg on its lane, cold controls BAL-free, ready \
                  generations covering the prefetch set, verdicts identical."
        );
    } else {
        println!("\nfixture gates FAILED:");
        for failure in &failures {
            println!("- {failure}");
        }
        std::process::exit(1);
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
/// controls' empty dispatch wire, the ready generation's coverage, and verdict
/// parity. Timing is never a gate.
fn fixture_failures(bench: &Bench) -> Vec<String> {
    let mut failures = Vec::new();
    for point in &bench.points {
        let at = format!("count {} / {}us", point.count, point.model.batch_round);
        for leg in Leg::ALL {
            let Some(summary) = point.summary(leg) else {
                failures.push(format!("{at}: leg {} did not run", leg.label()));
                continue;
            };
            let expected = match leg {
                Leg::OffLegacy => summary.lane == "legacy",
                Leg::ReadyCurrent | Leg::ReadyPrevious => summary.lane == "delta",
                Leg::ColdUnprepared => summary.lane == "legacy/unprepared",
                Leg::ColdUndelivered => summary.lane == "legacy/undelivered",
            };
            if !expected {
                failures.push(format!(
                    "{at}: leg {} resolved lane `{}`",
                    leg.label(),
                    summary.lane
                ));
            }
            match leg {
                Leg::OffLegacy | Leg::ColdUnprepared | Leg::ColdUndelivered => {
                    if summary.bal != 0 {
                        failures.push(format!(
                            "{at}: leg {} fetched {} BAL lists on the dispatch leg",
                            leg.label(),
                            summary.bal
                        ));
                    }
                    if summary.batches != 1
                        || summary.members
                            != u64::try_from(point.prefetch_identities).unwrap_or(u64::MAX)
                    {
                        failures.push(format!(
                            "{at}: leg {} paid {} batches / {} members, not the legacy 1 / {}",
                            leg.label(),
                            summary.batches,
                            summary.members,
                            point.prefetch_identities
                        ));
                    }
                }
                Leg::ReadyCurrent | Leg::ReadyPrevious => {
                    if summary.batches != 0 || summary.members != 0 {
                        failures.push(format!(
                            "{at}: ready leg {} paid {} batches / {} members",
                            leg.label(),
                            summary.batches,
                            summary.members
                        ));
                    }
                }
            }
        }
        if !point.parity() {
            failures.push(format!("{at}: candidate verdicts differ between lanes"));
        }
        for run in &point.runs {
            if let Some(generation) = run.generation.as_ref() {
                if generation.identities_covered != generation.identities_checked {
                    failures.push(format!(
                        "{at}: ready generation covers {} of {} prefetch identities",
                        generation.identities_covered, generation.identities_checked
                    ));
                }
            }
        }
    }
    failures
}
