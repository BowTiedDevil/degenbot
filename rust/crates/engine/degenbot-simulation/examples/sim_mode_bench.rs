#![expect(
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr
)]
//! The ADR-089 phase-6 offline engine benchmark: one corpus, four engine
//! modes, first-run and steady-state latency, plus the JIT backend's
//! compile telemetry.
//!
//! # Corpus
//!
//! The fixture spine mirrors the executor grammar harness (`crate::harness`,
//! revm-based): the REAL `cmd_executor` creation artifact plus the
//! `Token`/`Pair`/`PoolManager` stubs from `tier3-oracle/artifacts/`,
//! deployed as CREATE frames through the production-policy evm2 EVM and
//! committed. The workload is one encoded V2 two-hop cycle
//! (`encode_cmd_stream` → `execute()` calldata) replayed with the `discard`
//! verb, so every iteration re-executes identical real bytecode over an
//! identical warm read cache — the executor's 15KB of runtime code
//! dominates each run, which is exactly the code the JIT/AOT modes compile.
//!
//! # Modes
//!
//! - `interp`    — no interpreter runner (the stock evm2 interpreter).
//! - `jit-block` — `JitInterpreterRunner`, `blocking: true`: the first
//!   execution of each contract compiles synchronously (iteration 0 absorbs
//!   the full compile cost; the parity-gate-friendly mode).
//! - `jit-pool`  — the runner with `blocking: false`: frames interpret
//!   until the worker pool delivers the compiled program, then upgrade.
//! - `aot`       — the runner over a backend backed by the DURABLE artifact
//!   store (a scratch path here, the store the production JIT holder opens
//!   in place of temp dirs), pre-populated via `prepare_aot_sync` for every
//!   contract the corpus deployed; the first timed iteration already runs
//!   compiled code. The prep wall time is reported separately (the
//!   build-time cost a production AOT lane would absorb once).
//!
//! Every mode asserts a run-to-run identical verdict (status, gas, output
//! hash) and the report folds a cross-mode verdict identity check — a
//! compiled mode that diverges from the interpreter is a correctness bug,
//! not a performance result.
//!
//! # Usage
//!
//! ```sh
//! cargo run --release -p degenbot-simulation --example sim_mode_bench \
//!   --features evm2-jit -- [all|interp|jit-block|jit-pool|aot]
//! ```
//!
//! `SIM_BENCH_ITERS` sets the iteration count (default 100).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use alloy::primitives::{keccak256, B256};

use ::evm2::ethereum::RecoveredTxEnvelope;
use ::evm2::BaseEvmTypes;
use ::evm2::Evm;

use ::evm2_jit::evm2_evm::JitInterpreterRunner;
use ::evm2_jit::runtime::{AotRequest, JitBackend, ProgramKind, RuntimeConfig};

use degenbot_simulation::sim::evm2::aot_store::DurableArtifactStore;
use degenbot_simulation::sim::evm2::handle::EVM2_PRODUCTION_SPEC;

#[path = "shared/bench_corpus.rs"]
mod bench_corpus;
use bench_corpus::{bench_evm, build_corpus, Deployed};

/// The four engine modes under measurement.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Interp,
    JitBlock,
    JitPool,
    Aot,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Interp => "interp",
            Mode::JitBlock => "jit-block",
            Mode::JitPool => "jit-pool",
            Mode::Aot => "aot",
        }
    }
}

/// The per-mode verdict: one `execute()` run's observable identity.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
struct Verdict {
    status: bool,
    gas: u64,
    output: B256,
}

struct ModeReport {
    mode: &'static str,
    first_us: u128,
    p50_us: u128,
    p99_us: u128,
    /// `jit-pool` only: steady-state p50 AFTER the worker pool delivered
    /// every corpus program, plus how long the upgrade took from loop end.
    post_p50_us: Option<u128>,
    upgrade_wait_ms: Option<u128>,
    verdict: Verdict,
    lookup_hits: u64,
    lookup_misses: u64,
    compilations: u64,
    aot_prep_ms: Option<u128>,
}

/// A unique scratch directory removed on drop — the `aot` mode's durable
/// store root (the production store's shape, at a path a benchmark run may
/// throw away).
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(label: &str) -> Self {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "degenbot-sim-bench-{}-{sequence}-{label}",
            std::process::id()
        )))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The per-mode backend: none for the interpreter; blocking for the
/// synchronous mode; the worker pool for `jit-pool`; the pool plus the
/// durable artifact store for `aot`.
fn make_backend(mode: Mode, store_dir: &Path) -> Option<JitBackend> {
    let config = |blocking: bool| RuntimeConfig {
        enabled: true,
        blocking,
        ..Default::default()
    };
    match mode {
        Mode::Interp => None,
        Mode::JitBlock => Some(JitBackend::new(config(true)).expect("jit backend init")),
        Mode::JitPool => Some(JitBackend::new(config(false)).expect("jit backend init")),
        Mode::Aot => Some(
            JitBackend::new(RuntimeConfig {
                store: Some(Arc::new(
                    DurableArtifactStore::new(store_dir.join("aot-artifacts"))
                        .expect("artifact store"),
                )),
                ..config(false)
            })
            .expect("jit backend init"),
        ),
    }
}

/// `aot` mode: pre-compile every corpus contract into the store (timed and
/// reported — the build-time cost a production AOT lane absorbs once), and
/// prove each produced a `ProgramKind::Aot` program (the key-identity
/// check, in executable form).
fn aot_prepare(backend: &JitBackend, contracts: &[Deployed]) -> Result<u128, String> {
    let prep = Instant::now();
    for contract in contracts {
        backend
            .prepare_aot_sync(AotRequest {
                code_hash: contract.code_hash,
                code: contract.code.clone(),
                spec_id: EVM2_PRODUCTION_SPEC,
            })
            .map_err(|e| format!("prepare_aot_sync {:x}: {e}", contract.code_hash))?;
        let compiled = backend
            .get_compiled(contract.code_hash, EVM2_PRODUCTION_SPEC)
            .ok_or_else(|| {
                format!(
                    "aot prep {:x} did not produce a program",
                    contract.code_hash
                )
            })?;
        if compiled.kind != ProgramKind::Aot {
            return Err(format!(
                "aot prep {:x} produced {:?}",
                contract.code_hash, compiled.kind
            ));
        }
    }
    Ok(prep.elapsed().as_millis())
}

/// The timed discard loop: `iters` runs of the corpus frame, every verdict
/// identical to the first.
fn timed_loop(
    evm: &mut Evm<'_, BaseEvmTypes>,
    frame: &RecoveredTxEnvelope,
    iters: usize,
) -> Result<(Vec<u128>, Verdict), String> {
    let mut samples: Vec<u128> = Vec::with_capacity(iters);
    let mut verdict: Option<Verdict> = None;
    for _ in 0..iters {
        let t = Instant::now();
        let executed = evm
            .transact(frame)
            .map_err(|e| format!("timed frame: {e:?}"))?;
        let result = executed.discard();
        samples.push(t.elapsed().as_micros());
        let v = Verdict {
            status: result.status,
            gas: result.total_gas_spent,
            output: keccak256(&result.output),
        };
        match verdict {
            None => verdict = Some(v),
            Some(prev) if prev != v => {
                return Err(format!("run-to-run verdict drift: {prev:?} → {v:?}"));
            }
            _ => {}
        }
    }
    Ok((samples, verdict.expect("one iteration ran")))
}

/// `jit-pool`: the timed loop usually ends before the worker pool delivers
/// (the compiles cost hundreds of ms; the loop costs a few). Wait for the
/// upgrade, then measure the post-upgrade steady state — the honest
/// async-mode number. Returns `(upgrade wait ms, post-upgrade p50 µs)`.
fn post_upgrade_measure(
    backend: &JitBackend,
    contracts: &[Deployed],
    evm: &mut Evm<'_, BaseEvmTypes>,
    frame: &RecoveredTxEnvelope,
) -> Result<(u128, u128), String> {
    let wait = Instant::now();
    let deadline = std::time::Duration::from_secs(30);
    for contract in contracts {
        while backend
            .get_compiled(contract.code_hash, EVM2_PRODUCTION_SPEC)
            .is_none()
        {
            if wait.elapsed() > deadline {
                return Err(format!(
                    "jit-pool upgrade never landed for {:x}",
                    contract.code_hash
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    let wait_ms = wait.elapsed().as_millis();
    let mut post: Vec<u128> = Vec::with_capacity(20);
    for _ in 0..20 {
        let t = Instant::now();
        let executed = evm
            .transact(frame)
            .map_err(|e| format!("post-upgrade frame: {e:?}"))?;
        let _ = executed.discard();
        post.push(t.elapsed().as_micros());
    }
    post.sort_unstable();
    Ok((wait_ms, post[post.len() / 2]))
}

/// Run one mode end to end: fresh EVM, corpus build (untimed), optional AOT
/// prep (timed, reported separately), then the timed discard loop.
fn run_mode(mode: Mode, iters: usize, scratch: &ScratchDir) -> Result<ModeReport, String> {
    let mut evm = bench_evm(false);
    let corpus = build_corpus(&mut evm)?;
    let backend = make_backend(mode, scratch.path());
    let mut aot_prep_ms = None;
    if let Some(backend) = &backend {
        if mode == Mode::Aot {
            aot_prep_ms = Some(aot_prepare(backend, &corpus.contracts)?);
        }
        evm.set_interpreter_runner(JitInterpreterRunner::new(backend.clone()));
    }

    let stats_before = backend.as_ref().map(JitBackend::stats);
    let (samples, verdict) = timed_loop(&mut evm, &corpus.frame, iters)?;
    let (upgrade_wait_ms, post_p50_us) = if mode == Mode::JitPool {
        match &backend {
            Some(b) => {
                let (wait, p50) =
                    post_upgrade_measure(b, &corpus.contracts, &mut evm, &corpus.frame)?;
                (Some(wait), Some(p50))
            }
            None => (None, None),
        }
    } else {
        (None, None)
    };
    let stats_after = backend.as_ref().map(JitBackend::stats);
    let count = |f: fn(&::evm2_jit::runtime::RuntimeStatsSnapshot) -> u64| {
        stats_after.as_ref().map_or(0, &f) - stats_before.as_ref().map_or(0, &f)
    };

    let mut steady: Vec<u128> = samples.iter().copied().skip(1).collect();
    steady.sort_unstable();
    let p = |q: usize| steady[(steady.len() * q / 100).min(steady.len() - 1)];
    Ok(ModeReport {
        mode: mode.name(),
        first_us: samples[0],
        p50_us: p(50),
        p99_us: p(99),
        post_p50_us,
        upgrade_wait_ms,
        verdict,
        lookup_hits: count(|s| s.lookup_hits),
        lookup_misses: count(|s| s.lookup_misses),
        compilations: count(|s| s.compilations_succeeded),
        aot_prep_ms,
    })
}

fn fmt_opt(v: Option<u128>) -> String {
    v.map_or_else(|| "-".to_string(), |n| n.to_string())
}

fn main() {
    let filter = std::env::args().nth(1).unwrap_or_else(|| "all".to_string());
    let iters: usize = std::env::var("SIM_BENCH_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let modes = match filter.as_str() {
        "all" => vec![Mode::Interp, Mode::JitBlock, Mode::JitPool, Mode::Aot],
        "interp" => vec![Mode::Interp],
        "jit-block" => vec![Mode::JitBlock],
        "jit-pool" => vec![Mode::JitPool],
        "aot" => vec![Mode::Aot],
        other => {
            eprintln!("unknown mode {other:?} — want all|interp|jit-block|jit-pool|aot");
            std::process::exit(2);
        }
    };

    println!("sim_mode_bench: {iters} iterations per mode (steady-state excludes the first)");
    println!(
        "{:<10} {:>12} {:>12} {:>12} {:>12} {:>12} {:>8} {:>8} {:>8} {:>12}",
        "mode",
        "first µs",
        "p50 µs",
        "p99 µs",
        "post p50 µs",
        "upgrade ms",
        "hits*",
        "misses*",
        "compiled",
        "aot prep ms"
    );
    let scratch = ScratchDir::new("modes");
    let mut reports = Vec::new();
    for mode in modes {
        match run_mode(mode, iters, &scratch) {
            Ok(report) => {
                println!(
                    "{:<10} {:>12} {:>12} {:>12} {:>12} {:>12} {:>8} {:>8} {:>8} {:>12}",
                    report.mode,
                    report.first_us,
                    report.p50_us,
                    report.p99_us,
                    fmt_opt(report.post_p50_us),
                    fmt_opt(report.upgrade_wait_ms),
                    report.lookup_hits,
                    report.lookup_misses,
                    report.compilations,
                    fmt_opt(report.aot_prep_ms),
                );
                reports.push(report);
            }
            Err(e) => {
                println!("{:<10} FAILED: {e}", mode.name());
                std::process::exit(1);
            }
        }
    }
    if let Some((first, rest)) = reports.split_first() {
        let base = first.verdict;
        for report in rest {
            if report.verdict != base {
                eprintln!(
                    "VERDICT DIVERGENCE: {} {base:?} vs {} {:?}",
                    first.mode, report.mode, report.verdict
                );
                std::process::exit(1);
            }
        }
        println!("verdict identity across modes: OK ({base:?})");
        println!(
            "* hits/misses count the blocking lookup path only; async modes record events, not counters."
        );
    }
}
