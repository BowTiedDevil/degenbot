#![expect(
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr
)]
//! The JIT-composition probe from the phase-6 spine work: does the
//! production capture (access-list slots + swap events) survive OFF the
//! `Inspector` hook?
//!
//! evm2's frame dispatch is inspector XOR interpreter-runner, so the
//! production inspector suppresses the JIT path. The seam under test:
//!
//! - **slots** come from the executed transaction's `account_changes()` —
//!   the state-layer storage overlay records every slot *loaded or
//!   written*, populated by the host for interpreted and compiled frames
//!   alike (the step hook's SLOAD/SSTORE coverage, without the hook).
//! - **swaps** decode from the result's `logs` — the journal already
//!   scopes logs to committed frames, the same set the swap buffer's
//!   LIFO scoping reproduces incrementally.
//!
//! # Configurations
//!
//! - `A` — interpreter + the production composite inspector (reference).
//! - `B` — JIT blocking runner + the post-hoc seam (the target).
//! - `C` — plain interpreter + the post-hoc seam (seam correctness with
//!   no JIT in the picture).
//!
//! PASS: B's and C's slot/swap sets equal A's, and B's backend reports
//! compiled execution (lookup hits), proving capture rides compiled code.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use alloy::primitives::{Address, B256};

use ::evm2_jit::evm2_evm::JitInterpreterRunner;
use ::evm2_jit::runtime::{JitBackend, RuntimeConfig};

use degenbot_simulation::sim::capture::CapturedSwap;
use degenbot_simulation::sim::evm2::inspectors::Evm2SimInspector;

#[path = "shared/bench_corpus.rs"]
mod bench_corpus;
use bench_corpus::{bench_evm, build_corpus};

const ITERS: usize = 30;

type SlotSet = BTreeMap<Address, BTreeSet<B256>>;

struct ConfigReport {
    name: &'static str,
    slots: SlotSet,
    swaps: Vec<String>,
    p50_us: u128,
    lookup_hits: u64,
}

/// The normalized swap comparison currency (fields, not ordering).
fn swap_lines(swaps: &[CapturedSwap]) -> Vec<String> {
    let mut lines: Vec<String> = swaps.iter().map(|s| format!("{s:?}")).collect();
    lines.sort();
    lines
}

/// Reference configuration: the production inspector on the interpreter.
fn run_inspector_config() -> Result<ConfigReport, String> {
    let mut evm = bench_evm(true);
    let corpus = build_corpus(&mut evm)?;
    // The corpus build runs through the inspector too — flush its capture
    // so the reference holds only the timed runs' sets.
    let inspector = evm
        .inspector_mut()
        .and_then(|i| i.downcast_mut::<Evm2SimInspector>())
        .ok_or("inspector downcast")?;
    let _ = inspector.take_access_list();
    let _ = inspector.take_swaps();
    let _ = inspector.take_trace();
    let mut samples = Vec::with_capacity(ITERS);
    let mut reference: Option<(SlotSet, Vec<String>)> = None;
    for _ in 0..ITERS {
        let t = Instant::now();
        let executed = evm
            .transact(&corpus.frame)
            .map_err(|e| format!("inspector frame: {e:?}"))?;
        let result = executed.discard();
        samples.push(t.elapsed().as_micros());
        if !result.status {
            return Err("inspector frame reverted".to_string());
        }
        let inspector = evm
            .inspector_mut()
            .and_then(|i| i.downcast_mut::<Evm2SimInspector>())
            .ok_or("inspector downcast")?;
        let access_list = inspector.take_access_list();
        let mut slots = SlotSet::new();
        for item in access_list.0 {
            slots.insert(item.address, item.storage_keys.into_iter().collect());
        }
        let swaps = swap_lines(&inspector.take_swaps());
        let current = (slots, swaps);
        match &reference {
            None => reference = Some(current),
            Some(prev) if *prev != current => {
                let (ps, pw) = prev;
                eprintln!(
                    "drift: prev slots {} cur slots {} prev swaps {} cur swaps {}",
                    ps.values().map(BTreeSet::len).sum::<usize>(),
                    current.0.values().map(BTreeSet::len).sum::<usize>(),
                    pw.len(),
                    current.1.len()
                );
                eprint!("{}", diff_slots(ps, &current.0));
                return Err("inspector capture drifted run-to-run".to_string());
            }
            _ => {}
        }
    }
    samples.sort_unstable();
    let (slots, swaps) = reference.expect("one iteration ran");
    Ok(ConfigReport {
        name: "A:interp+inspector",
        slots,
        swaps,
        p50_us: samples[ITERS / 2],
        lookup_hits: 0,
    })
}

/// The seam configuration: no inspector; slots from `account_changes`,
/// swaps from result logs. `jit` installs the blocking runner.
fn run_seam_config(name: &'static str, jit: bool) -> Result<ConfigReport, String> {
    let mut evm = bench_evm(false);
    let corpus = build_corpus(&mut evm)?;
    if corpus.contracts.is_empty() {
        return Err("corpus deploys no contracts".to_string());
    }
    let backend = jit.then(|| {
        JitBackend::new(RuntimeConfig {
            enabled: true,
            blocking: true,
            ..Default::default()
        })
        .expect("jit backend init")
    });
    if let Some(backend) = &backend {
        evm.set_interpreter_runner(JitInterpreterRunner::new(backend.clone()));
    }
    let mut samples = Vec::with_capacity(ITERS);
    let mut reference: Option<(SlotSet, Vec<String>)> = None;
    for _ in 0..ITERS {
        let t = Instant::now();
        let executed = evm
            .transact(&corpus.frame)
            .map_err(|e| format!("seam frame: {e:?}"))?;
        let mut slots = SlotSet::new();
        for changes in executed.account_changes() {
            // The step-hook domain: accounts with actual storage access.
            // `account_changes` also carries accounts merely touched (the
            // caller, value recipients) whose slot map is empty.
            if changes.storage.slots.is_empty() {
                continue;
            }
            let entry = slots.entry(changes.address).or_default();
            for key in changes.storage.slots.keys() {
                entry.insert(B256::from(*key));
            }
        }
        let result = executed.discard();
        samples.push(t.elapsed().as_micros());
        if !result.status {
            return Err("seam frame reverted".to_string());
        }
        let swaps: Vec<CapturedSwap> = result
            .logs
            .iter()
            .filter_map(CapturedSwap::from_log)
            .collect();
        let current = (slots, swap_lines(&swaps));
        match &reference {
            None => reference = Some(current),
            Some(prev) if *prev != current => {
                return Err("seam capture drifted run-to-run".to_string());
            }
            _ => {}
        }
    }
    samples.sort_unstable();
    let (slots, swaps) = reference.expect("one iteration ran");
    let lookup_hits = backend.map_or(0, |b| b.stats().lookup_hits);
    Ok(ConfigReport {
        name,
        slots,
        swaps,
        p50_us: samples[ITERS / 2],
        lookup_hits,
    })
}

fn diff_slots(a: &SlotSet, b: &SlotSet) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for (address, slots) in a {
        match b.get(address) {
            None => {
                let _ = writeln!(out, "  only-A {address} (+{} slots)", slots.len());
            }
            Some(theirs) => {
                for slot in slots.difference(theirs) {
                    let _ = writeln!(out, "  only-A {address} slot {slot}");
                }
                for slot in theirs.difference(slots) {
                    let _ = writeln!(out, "  only-B {address} slot {slot}");
                }
            }
        }
    }
    for address in b.keys().filter(|addr| !a.contains_key(*addr)) {
        let _ = writeln!(out, "  only-B {address}");
    }
    out
}

fn compare(reference: &ConfigReport, candidate: &ConfigReport) -> bool {
    let slots_ok = reference.slots == candidate.slots;
    let swaps_ok = reference.swaps == candidate.swaps;
    println!(
        "{:<24} slots {} (A:{} B:{}) swaps {} (A:{} B:{})",
        candidate.name,
        if slots_ok { "==" } else { "!=" },
        reference.slots.values().map(BTreeSet::len).sum::<usize>(),
        candidate.slots.values().map(BTreeSet::len).sum::<usize>(),
        if swaps_ok { "==" } else { "!=" },
        reference.swaps.len(),
        candidate.swaps.len(),
    );
    if !slots_ok {
        eprint!("{}", diff_slots(&reference.slots, &candidate.slots));
    }
    if !swaps_ok {
        eprintln!(
            "  A swaps: {:?}\n  B swaps: {:?}",
            reference.swaps, candidate.swaps
        );
    }
    slots_ok && swaps_ok
}

fn main() {
    let a = run_inspector_config().unwrap_or_else(|e| {
        eprintln!("A failed: {e}");
        std::process::exit(1);
    });
    let b = run_seam_config("B:jit+seam", true).unwrap_or_else(|e| {
        eprintln!("B failed: {e}");
        std::process::exit(1);
    });
    let c = run_seam_config("C:interp+seam", false).unwrap_or_else(|e| {
        eprintln!("C failed: {e}");
        std::process::exit(1);
    });

    println!("capture_seam_probe: {ITERS} iterations per configuration");
    println!("{:<24} {:>10} {:>12}", "config", "p50 µs", "lookup hits");
    for report in [&a, &b, &c] {
        println!(
            "{:<24} {:>10} {:>12}",
            report.name, report.p50_us, report.lookup_hits
        );
    }

    let mut ok = true;
    ok &= compare(&a, &b);
    ok &= compare(&a, &c);
    if b.lookup_hits == 0 {
        eprintln!("B ran WITHOUT compiled execution (0 lookup hits) — the probe proves nothing");
        ok = false;
    }
    if ok {
        println!("PASS: the post-hoc seam reproduces the inspector capture, including under compiled execution");
    } else {
        eprintln!("FAIL: seam/inspector capture mismatch");
        std::process::exit(1);
    }
}
