//! Acceptance gate for ADR-062 D7: `degenbot-config` is the one env-reading
//! owner, on both sides of the language seam. `std::env::var` / `env::var_os`
//! may appear only inside its loader, and `os.environ` / `os.getenv` /
//! `os.environb` may appear in the Python companion only at enumerated sites.
//!
//! This file scans the Rust half. The companion half is scanned by the shared
//! stdlib implementation in `scripts/env_surface.py`: this test invokes it as a
//! subprocess, `schema_completeness.rs` invokes it for the `DEGENBOT_*` key
//! inventory, and the default pytest suite (`tests/test_env_surface.py`) calls
//! it directly. One implementation, one report format (`path:line: NAME`), one
//! `<computed>` convention — so a key cannot be migrated out of the Rust schema
//! and into Python unnoticed: the migration lands a finding in one half or the
//! other. A stray re-introduced read fails loudly with the file and the variable
//! name (Q6 fail-closed posture: those keys must load through `BotConfig`
//! instead).
//!
//! The allowlist is the ratchet: a deletion has to keep this test green, so a
//! read that comes back cannot stay.
//!
//! `python3` is a declared test-tool dependency of the companion half, so its
//! absence is a named refusal, never a skip and never a stale snapshot.
//!
//! The lexer this file used to carry was replaced after a measured comparison
//! (aliases and unparseable sources were invisible to it); the evidence and the
//! decision are recorded in [ADR-110].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Files outside degenbot-config where env reads are permitted, with the
/// exact names allowed per file. A NEW env read in a library file must add
/// an entry here (with justification) or be migrated onto `BotConfig`.
///
/// The map holds the Rust tree's entries, keyed relative to `rust/` (the
/// workspace root the Rust scan normalizes to). The companion's sanctioned
/// reads live beside the scanner that reads them
/// (`PYTHON_ALLOWLIST` in `scripts/env_surface.py`), so each sanction sits with
/// the implementation that applies it and neither half carries a copy.
///
/// A name is reported as written at the call site, so a read whose name is
/// computed is enumerated by the identifier it computes from -- the loader's
/// `env::var(name)` seam and the companion's retired-key identifiers alike.
/// An expression no name is reducible from is reported `<computed>`, which no
/// file may be enumerated for by accident.
///
/// # Entry classes
///
/// Every entry belongs to one of four classes, and the class rationale lives
/// HERE so a per-entry line carries only what is true of that file:
///
/// 1. **Infra** -- OS or tooling signal variables (a shell's `HOME`, the
///    tracing convention's `RUST_LOG`) no static schema can own.
/// 2. **Offline tool knob** -- a per-run control of an offline probe,
///    benchmark, replay, or capture generator. The readout is a measurement or
///    a corpus selection, so the knob carries no operator-config authority, and
///    routing it through the loader would hand a standalone tool the config
///    crate's dependency graph for no behavioral gain.
/// 3. **Test-only stance** -- a name a parent test sets to drive a CHILD test
///    binary, or a dev/feature-gated stance inside `src`. Presence coordinates
///    a harness; it never configures a production boot.
/// 4. **Refusal or plumbing** -- a presence-only read of a retired name (no
///    value is consumed) or console-level plumbing the logging stack owns.
fn allowed() -> &'static BTreeMap<&'static str, &'static [&'static str]> {
    static MAP: OnceLock<BTreeMap<&'static str, &'static [&'static str]>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = BTreeMap::new();
        // Infra: shell state, the tracing-convention filter, and the
        // companion's exporter endpoint. (TOKIO_WORKER_THREADS is refused: the
        // ambient runtime is sized from the cgroup CPU budget via the typed
        // `runtime.io_workers` key.)
        m.insert(
            "crates/shells/degenbot-python/src/python_log_layer.rs",
            &[
                "HOME",
                "RUST_LOG",
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                // The stderr fmt-layer mirror gate: output plumbing, not config.
                "DEGENBOT_LOG_FMT",
            ][..],
        );
        // Offline tool knobs in library code.
        m.insert(
            "crates/engine/degenbot-bot/src/profiling.rs",
            &["HOTPATH_SHUTDOWN_MS"][..],
        );
        // The probe-fixture corpus path, `cfg(test)` only: the
        // concentrated-liquidity-hop clamp margin rides the typed config
        // (`solve.clamp_margin_wei`), so this is the one remaining env read on
        // that path.
        m.insert(
            "crates/engine/degenbot-bot/src/arb_engine/executor_ab_probe.rs",
            &["DEGENBOT_PROBE_FIXTURE"][..],
        );
        m.insert(
            "crates/engine/degenbot-bot/src/arb_engine/epoch_delta_parity.rs",
            &["DBENCH_CAPTURES"][..], // offline parity fixture dir
        );
        m.insert(
            "crates/foundation/degenbot-executor/src/grammar_walker/shapes/two_hop_v4_led.rs",
            &["T1_CAPTURE"][..], // executor shape dev-capture gate
        );
        m.insert(
            "crates/foundation/degenbot-executor/src/grammar_walker/shapes/two_hop_seed_v4.rs",
            &["T1_CAPTURE"][..], // executor shape dev-capture gate
        );

        // The two Aave window recorders read the chain-1 endpoint as their
        // documented invocation default, consulted only when the explicit
        // `--node` flag and its value are absent: a standalone offline tool's
        // shell contract, not a second operator-config authority.
        m.insert(
            "tools/fixture-tools/src/bin/generate_aave_upgrade_windows/main.rs",
            &["DEGENBOT_RPC_HTTP_CHAINID_1"][..],
        );
        m.insert(
            "tools/fixture-tools/src/bin/generate_aave_config_windows/main.rs",
            &["DEGENBOT_RPC_HTTP_CHAINID_1"][..],
        );

        insert_engine_lab_entries(&mut m);

        insert_solver_lab_entries(&mut m);

        // The Python oracle corpus recorder reads each tier's documented
        // endpoint default, consulted only when that tier's repeatable
        // `--ethereum-node` / `--base-node` flag is absent; the base read keeps
        // the keyless endpoint the aerodrome/pancakeswap parity tests fork. It
        // opens no operator DB and reads no typed config key.
        m.insert(
            "tools/fixture-tools/src/bin/record_py_oracle_corpus/main.rs",
            &[
                "TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI",
                "TEST_BASE_ARCHIVE_NODE_HTTP_URI",
            ][..],
        );

        // The concentrated-liquidity capture generator's shell contract: the
        // required endpoint, the pinned block, the per-pool tick-word fetch cap,
        // and the per-block path cap, each read once at start-up. It opens no
        // operator DB and reads no typed config key.
        m.insert(
            "tools/fixture-tools/src/bin/concentrated_liquidity_capture_gen/main.rs",
            &[
                "DEGENBOT_CLCAP_RPC",
                "DEGENBOT_CLCAP_BLOCK",
                "DEGENBOT_CLCAP_MAX_FETCHES",
                "DEGENBOT_CLCAP_PATH_CAP",
            ][..],
        );
        m.insert(
            // The reset-lock flock test spawns a child test binary that takes
            // the real lock before the parent kills it -- cross-process
            // coordination, not operator config.
            "crates/shells/degenbot-cli-core/src/aave.rs",
            &["RESET_LOCK_TEST_DB", "RESET_LOCK_TEST_READY"][..],
        );
        // Test-only stances inside src: the parent test sets these to drive
        // the CHILD test binary (no config loader in the harness).
        m.insert(
            "crates/engine/degenbot-bot/src/bot_core/block_pump.rs",
            &[
                "DEGENBOT_SELF_ABORT_TEST",
                "DEGENBOT_DESYNC_TEST_STANCE",
                // DEGENBOT_RPC_WS_CHAINID_<chain_id> is INTENTIONALLY dynamic
                // (non-schema); the suffix is the numeric chain id. Documented
                // next to SCHEMA.
                "DEGENBOT_RPC_WS_CHAINID_1",
            ][..],
        );
        m.insert(
            "crates/shells/degenbot-python/src/diagnostics/thread_registry.rs",
            &["DEGENBOT_STATE_LOCK_DIAG"][..], // test stance (feature-gated)
        );
        // Refusal and plumbing. The ADR-043 §5 retired-name boot detection
        // reads the env by iterating a closed list (`RETIRED_ENV_NAMES`), so
        // the name is computed at the call site; detection only -- no alias is
        // honored. The ADR-043 §4 filter branch rides the same file: `RUST_LOG`
        // is the tracing-convention override (it wins verbatim on every sink;
        // the typed `telemetry.log_level`/`telemetry.diag` knobs apply
        // otherwise). Output plumbing, not config.
        m.insert(
            "crates/foundation/degenbot-core/src/telemetry.rs",
            &["name", "RUST_LOG"][..],
        );

        m
    })
}

/// The offline engine-lab instrument entries. A separate builder keeps this
/// registry readable: the fan-out probes below, each with its own per-run knob
/// set, would otherwise bury the library-code entries above.
fn insert_engine_lab_entries(map: &mut BTreeMap<&'static str, &'static [&'static str]>) {
    // The cold-miss fan-out probe's two per-run knobs: the modeled RPC
    // round-trip latency and the frame count N, read once at start-up
    // (defaults 200 us, 8).
    map.insert(
        "tools/engine-lab/src/bin/cold_miss_fanout_probe/main.rs",
        &["COLD_MISS_LATENCY_US", "COLD_MISS_FRAMES"][..],
    );

    // The per-worker fan-out probe's ten per-run knobs: the two modeled
    // round-trip prices, the candidate and worker counts, the corpus shape, the
    // recorded-trace path and its chunk size, and the three diagnostic
    // switches, read once at start-up (defaults 700 us, 1200 us, 21 candidates,
    // 2 workers, `production`).
    map.insert(
        "tools/engine-lab/src/bin/worker_fanout_probe/main.rs",
        &[
            "COLD_MISS_LATENCY_US",
            "WORKER_FANOUT_ACCOUNT_US",
            "WORKER_FANOUT_CANDIDATES",
            "WORKER_FANOUT_WORKERS",
            "WORKER_FANOUT_SHAPE",
            "WORKER_FANOUT_TRACE",
            "WORKER_FANOUT_DEBUG",
            "WORKER_FANOUT_PLANT_DIVERGENCE",
            "WORKER_FANOUT_TRACE_CHUNK",
            "WORKER_FANOUT_EXEC_SLEEP",
        ][..],
    );

    // The sim-mode benchmark's iteration count per mode, read once at start-up
    // (default 100).
    map.insert(
        "tools/engine-lab/src/bin/sim_mode_bench/main.rs",
        &["SIM_BENCH_ITERS"][..],
    );

    // The hotpath Prometheus exporter probe's invocation contract: the
    // `DEGENBOT_HOTPATH=1` guard/runtime gate the bot's
    // `profiling::hotpath_guard` itself consults (the probe refuses to run
    // without it), and the exporter port override (`HOTPATH_PROMETHEUS_PORT`,
    // default 6772) read once at scrape time.
    map.insert(
        "tools/engine-lab/src/bin/hotpath_prometheus_probe/main.rs",
        &["DEGENBOT_HOTPATH", "HOTPATH_PROMETHEUS_PORT"][..],
    );

    insert_bal_engine_lab_entries(map);

    // The session-scheduler fan-out probe's seven per-run knobs: the two
    // modeled round-trip prices, the lane and slot counts, the lane-progress
    // diagnostic, the planted-divergence negative control, and the recorded
    // trace path, read once at start-up (defaults: the optimistic 200/400 us
    // and pessimistic 700/1200 us points, W=2, F=8, `/tmp/fanout_trace.log`).
    map.insert(
        "tools/engine-lab/src/bin/session_fanout_probe/main.rs",
        &[
            "SESSION_FANOUT_DEBUG",
            "SESSION_FANOUT_STORAGE_US",
            "SESSION_FANOUT_ACCOUNT_US",
            "SESSION_FANOUT_W",
            "SESSION_FANOUT_F",
            "SESSION_FANOUT_PLANT_DIVERGENCE",
            "SESSION_FANOUT_TRACE",
        ][..],
    );
}

/// The BAL engine-lab instrument entries. A separate builder because the nine
/// BAL instruments share one `BAL_*` key-prefix family; holding them apart
/// keeps each registry readable.
fn insert_bal_engine_lab_entries(map: &mut BTreeMap<&'static str, &'static [&'static str]>) {
    insert_bal_offline_probe_entries(map);

    insert_bal_live_probe_entries(map);
}

/// The offline BAL engine-lab entries: the six instruments that never dial a
/// node. Split from the live half so each registry helper stays under the
/// line-count lint with every per-entry justification carried in place.
fn insert_bal_offline_probe_entries(map: &mut BTreeMap<&'static str, &'static [&'static str]>) {
    // The analytical candidate-scaling report's planted-mismatch calibration
    // path, read once at start-up with its `== "1"` gate.
    map.insert(
        "tools/engine-lab/src/bin/bal_candidate_scaling_probe/main.rs",
        &["BAL_SCALING_PLANT_MISMATCH"][..],
    );

    // The offline BAL delta-snapshot probe's five per-run knobs: the
    // planted-divergence negative control and the working-set matrix's four
    // sizes/reps/lookups/depth bounds, read once at start-up (defaults
    // 2500..1000000 sizes, 11 reps, 10000 lookups, depth 2).
    map.insert(
        "tools/engine-lab/src/bin/bal_delta_snapshot_probe/main.rs",
        &[
            "BAL_DELTA_PLANT_DIVERGENCE",
            "BAL_DELTA_SIZES",
            "BAL_DELTA_REPS",
            "BAL_DELTA_LOOKUPS",
            "BAL_DELTA_MAX_DEPTH",
        ][..],
    );

    // The offline BAL retention probe's five per-run knobs: the threshold
    // matrix, the synthetic long run's three sizes, and the planted-divergence
    // negative control, read once at start-up (defaults thresholds 1/2/4/8,
    // 300 blocks, 2000 keys, wave 4).
    map.insert(
        "tools/engine-lab/src/bin/bal_retention_probe/main.rs",
        &[
            "BAL_RETENTION_PLANT_DIVERGENCE",
            "BAL_RETENTION_THRESHOLDS",
            "BAL_RETENTION_SYNTH_BLOCKS",
            "BAL_RETENTION_SYNTH_KEYS",
            "BAL_RETENTION_SYNTH_WAVE",
        ][..],
    );

    // The offline BAL round-coalesced replay probe's eight per-run knobs: the
    // two recorded-trace paths and their matching real-coverage captures, the
    // two modeled round-trip prices, the worker and slot counts, the idle-block
    // window, and the planted-divergence negative control, read once at
    // start-up (defaults: the two `/tmp` corpora, the 200/400 us and 700/1200
    // us points, W=2, F=8, idle 8).
    map.insert(
        "tools/engine-lab/src/bin/bal_delta_rounds_probe/main.rs",
        &[
            "BAL_DELTA_ROUNDS_TRACES",
            "BAL_DELTA_ROUNDS_FIXTURES",
            "BAL_DELTA_ROUNDS_STORAGE_US",
            "BAL_DELTA_ROUNDS_ACCOUNT_US",
            "BAL_DELTA_ROUNDS_W",
            "BAL_DELTA_ROUNDS_F",
            "BAL_DELTA_ROUNDS_IDLE",
            "BAL_DELTA_ROUNDS_PLANT_DIVERGENCE",
        ][..],
    );

    // The paired production-relevance replay probe's eight per-run knobs: the
    // recorded-trace path and its matching real-coverage capture, the two
    // modeled round-trip prices, the worker and slot counts, the idle-block
    // window, and the planted-divergence negative control, read once at
    // start-up (defaults: `/tmp/bal_production_trace.log`, the committed
    // production capture, the 200/400 us and 700/1200 us points, W=2, F=8,
    // idle 8).
    map.insert(
        "tools/engine-lab/src/bin/bal_production_relevance_probe/main.rs",
        &[
            "BAL_PROD_TRACE",
            "BAL_PROD_FIXTURE",
            "BAL_PROD_STORAGE_US",
            "BAL_PROD_ACCOUNT_US",
            "BAL_PROD_W",
            "BAL_PROD_F",
            "BAL_PROD_IDLE",
            "BAL_PROD_PLANT_DIVERGENCE",
        ][..],
    );

    // The offline BAL dispatch-scaling probe's two per-run knobs: the fan-out
    // size list and the interleaved repetitions per leg, read once at start-up
    // (defaults: counts 1,4,16,50,100,500,1000 and 3 repetitions).
    map.insert(
        "tools/engine-lab/src/bin/bal_dispatch_scaling_probe/main.rs",
        &["BAL_DISPATCH_SCALING_COUNTS", "BAL_DISPATCH_SCALING_REPS"][..],
    );
}

/// The live BAL engine-lab entries: the five instruments that dial a
/// declared live endpoint, each carrying the two endpoint names beside its
/// own controls.
fn insert_bal_live_probe_entries(map: &mut BTreeMap<&'static str, &'static [&'static str]>) {
    // The live BAL carry-forward probe's invocation contract: the
    // recorded-trace path (`/tmp/fanout_trace.log`), the first/last
    // corpus-block overrides, and the two endpoint names the live legs dial,
    // read once at start-up (defaults 26375033..=26375053).
    map.insert(
        "tools/engine-lab/src/bin/bal_carry_probe/main.rs",
        &[
            "BAL_CARRY_TRACE",
            "BAL_CARRY_FIRST",
            "BAL_CARRY_LAST",
            "DEGENBOT_RPC_HTTP_CHAINID_1",
            "TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI",
        ][..],
    );

    // The live BAL head-lifecycle probe's invocation contract: the
    // historical/fresh-head/waiter sample knobs, the parked window each waiter
    // sample holds, the planted-divergence negative control, the summary-out
    // path, and the two endpoint names the live legs dial, read once at
    // start-up (defaults 26375033..=26375053, 6 fresh heads, 21 waiter
    // samples, 20 ms hold; the two zero sample sizes disable their legs).
    map.insert(
        "tools/engine-lab/src/bin/bal_head_lifecycle_probe/main.rs",
        &[
            "BAL_HEAD_FIRST",
            "BAL_HEAD_LAST",
            "BAL_HEAD_FRESH",
            "BAL_HEAD_WAIT_SAMPLES",
            "BAL_HEAD_WAIT_HOLD_MS",
            "BAL_HEAD_PLANT_DIVERGENCE",
            "BAL_HEAD_SUMMARY_OUT",
            "DEGENBOT_RPC_HTTP_CHAINID_1",
            "TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI",
        ][..],
    );

    // The live real-read-oracle probe's invocation contract: the soak-trace
    // path (the recipe exports `logs/bal_postfix_soak_trace.log`), the anchor
    // and row-count selectors, the planted one-wei negative control, and the
    // two endpoint names the live legs dial, read once at start-up (defaults:
    // 6 anchors x 2 rows, the plant unarmed).
    map.insert(
        "tools/engine-lab/src/bin/bal_real_read_oracle_probe/main.rs",
        &[
            "BAL_READ_ORACLE_TRACE",
            "BAL_READ_ORACLE_ANCHORS",
            "BAL_READ_ORACLE_ROWS",
            "BAL_READ_ORACLE_PLANT_WEI",
            "DEGENBOT_RPC_HTTP_CHAINID_1",
            "TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI",
        ][..],
    );

    // The wall-clock ceiling probe's invocation contract: the recorded-trace
    // path (`/tmp/fanout_trace.log`), the real-coverage fixture path and its
    // capture flag, the coverage-mode selector, the leg selector, the two
    // modeled round-trip prices, the worker and slot counts, the two negative
    // controls, the lane-progress diagnostic, and the two endpoint names the
    // dev-only fixture-capture phase dials (the offline replay itself never
    // touches the network), read once at start-up (defaults: both coverage
    // modes, all three legs, the 200/400 us and 700/1200 us points, W=2, F=8,
    // the committed capture, no plant armed).
    map.insert(
        "tools/engine-lab/src/bin/bal_wall_ceiling_probe/main.rs",
        &[
            "BAL_WALL_LEGS",
            "BAL_WALL_STORAGE_US",
            "BAL_WALL_ACCOUNT_US",
            "BAL_WALL_COVERAGE",
            "BAL_WALL_TRACE",
            "BAL_WALL_FIXTURE",
            "BAL_WALL_FIXTURE_CAPTURE",
            "BAL_WALL_W",
            "BAL_WALL_F",
            "BAL_WALL_PLANT_STORAGE_DELAY_US",
            "BAL_WALL_PLANT_DIVERGENCE",
            "BAL_WALL_DEBUG",
            "DEGENBOT_RPC_HTTP_CHAINID_1",
            "TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI",
        ][..],
    );

    // The live BAL acquisition probe's invocation contract: the anchor block,
    // the interleaved repetitions, the corpus width list, and the two endpoint
    // names the live legs dial (the chain-1 endpoint family first, the conftest
    // archive endpoint as the fallback), read once at start-up (defaults anchor
    // 26375043, 7 repetitions, widths 1,4,16,50,100,500,1000).
    map.insert(
        "tools/engine-lab/src/bin/bal_live_acquisition_probe/main.rs",
        &[
            "BAL_ACQ_ANCHOR",
            "BAL_ACQ_REPS",
            "BAL_ACQ_CORPUS_COUNTS",
            "DEGENBOT_RPC_HTTP_CHAINID_1",
            "TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI",
        ][..],
    );
}

/// The solver-lab entries, inserted into the same map the Rust scan reads. A
/// separate builder keeps the four offline replay binaries together.
fn insert_solver_lab_entries(map: &mut BTreeMap<&'static str, &'static [&'static str]>) {
    // The two solver replay binaries' per-run knobs for a local corpus replay:
    // the iteration count, and the golden regeneration flag for the
    // concentrated-liquidity program.
    map.insert(
        "tools/solver-lab/src/bin/concentrated_liquidity_solve_replay/main.rs",
        &["DR_REPLAY_ITERS", "DR_REPLAY_REGEN"][..],
    );
    map.insert(
        "tools/solver-lab/src/bin/mixed_solve_replay/main.rs",
        &["DR_REPLAY_ITERS"][..],
    );

    // The entry A/B readout's two per-run knobs: the rep count per path per
    // arm and the memo stance, read once at start-up (defaults 25 reps, stance
    // off).
    map.insert(
        "tools/solver-lab/src/bin/entry_ab_readout/main.rs",
        &["DR_AB_ITERS", "DR_AB_MEMO"][..],
    );

    // The cache-lab replay's four per-run knobs, read once at start-up: the
    // path cap and the transitions per path shorten a local sweep,
    // `DRCLAB_DIGEST` echoes each reference solve, and `DRCLAB_MICRO` adds the
    // micro-bench readout.
    map.insert(
        "tools/solver-lab/src/bin/concentrated_liquidity_cache_lab/main.rs",
        &[
            "DRCLAB_MAX_PATHS",
            "DRCLAB_TRANS",
            "DRCLAB_DIGEST",
            "DRCLAB_MICRO",
        ][..],
    );
}

#[expect(
    clippy::expect_used,
    reason = "test helper: CARGO_MANIFEST_DIR depth is fixed at compile time; loud failure beats a dummy root"
)]
fn workspace_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        // CARGO_MANIFEST_DIR = rust/crates/foundation/degenbot-config
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .expect("role-grouped manifest is under the rust/ workspace")
    })
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Skip non-source trees so a scan of the repo root sees only this
            // checkout's crate sources:
            //   - `target/` carries vendored crate copies that mirror crate
            //     sources and double-report;
            //   - hidden dirs are tooling/worktree state (`.git/`, and `.pi/`
            //     per-branch checkouts of this same repo, pre-cutover surfaces
            //     included);
            //   - `autoresearch/` is the gitignored agent-harness scratch tree
            //     holding full repo snapshots (same sources at an older
            //     revision), which would otherwise double-report every hit;
            //   - `vendor/` is the vendored upstream tree (rust/vendor,
            //     ADR-096): upstream's code with upstream's env reads, not
            //     this project's env discipline.
            let fname = entry.file_name();
            let name = fname.to_str().unwrap_or("");
            if name == "target"
                || name == "autoresearch"
                || name == "node_modules"
                || name == "vendor"
                || name.starts_with('.')
            {
                continue;
            }
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Extract the env variable name from a call like `std::env::var("NAME")`.
#[must_use]
fn env_var_name(line: &str) -> Option<String> {
    // Anchor the scan AFTER the env::var call token so a preceding
    // initializer like `if let Ok(raw) = ...` cannot shadow the parse.
    let pos = line.find("env::var")?;
    let rest = &line[pos + "env::var".len()..];
    let rest = rest.trim_start_matches(|c: char| c == '_' || c.is_ascii_alphanumeric());
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('(')?;
    let rest = rest.trim_start();
    if let Some(q) = rest.strip_prefix('"') {
        let name: String = q.chars().take_while(|c| *c != '"').collect();
        return (!name.is_empty()).then_some(name);
    }
    // Computed name (a const), e.g. env::var(ENV_VAR): report the identifier.
    let ident: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if ident == "name" {
        // The loader's EnvVars seam (env::var(name)) is sanctioned.
        return Some("name".into());
    }
    (!ident.is_empty()).then_some(ident)
}

/// live-invariant gate (ADR-087 D2): watches every live Rust source outside `degenbot-config` for env reads not in the enumerated allowlist; not tripwire precedent.
#[test]
fn no_stray_env_reads_outside_the_config_loader() {
    let root = workspace_root();
    let mut files = Vec::new();
    collect_rs_files(root, &mut files);
    assert!(
        files
            .iter()
            .any(|p| p.ends_with("degenbot-config/src/loader.rs")),
        "workspace scan must include the config crate itself"
    );

    let mut violations: Vec<String> = Vec::new();
    for path in &files {
        let rel = path
            .strip_prefix(root)
            .map(|p| p.display().to_string())
            .unwrap_or_default()
            // When cargo runs from rust/ the manifest parents land on the
            // repo root; normalize to a crate-relative path either way.
            .strip_prefix("rust/")
            .map_or_else(
                || {
                    path.strip_prefix(root)
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                },
                str::to_string,
            );
        // The one sanctioned env-reading crate.
        if rel.starts_with("crates/foundation/degenbot-config/") {
            continue;
        }
        // Test-only stances: every tests/, examples/, and benches/
        // directory file, wherever it sits in the tree - a crate-nested
        // `crates/*/tests/` path OR a workspace-level
        // `examples/`/`tests/` leading component (e.g.
        // `examples/settlement_bot/`). Matching on path components rather
        // than a `/tests/` substring keeps the leading-component case from
        // slipping through.
        let is_test_or_example = Path::new(rel.as_str()).components().any(|c| {
            matches!(
                c.as_os_str().to_str(),
                Some("tests" | "examples" | "benches")
            )
        });
        if is_test_or_example {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let allow = allowed().get(rel.as_str());
        for (idx, line) in text.lines().enumerate() {
            if !line.contains("env::var") {
                continue;
            }
            // Whole-line comments documenting historical reads are fine.
            if line.trim_start().starts_with("//") {
                continue;
            }
            let name = env_var_name(line)
                .or_else(|| {
                    // Multi-line call: the name may sit on one of the next
                    // two lines (fn arg on its own line).
                    text.lines().skip(idx + 1).take(2).find_map(|l| {
                        let t = l.trim_start().trim_end_matches(',').trim();
                        let inner = t
                            .strip_prefix('"')
                            .and_then(|r| r.chars().position(|c| c == '"'))
                            .map(|p| &t[1..p]);
                        inner.filter(|s| !s.is_empty()).map(str::to_string)
                    })
                })
                .unwrap_or_else(|| "<computed>".into());
            let permitted = allow.is_some_and(|names| names.contains(&name.as_str()));
            if !permitted {
                violations.push(format!("{rel}:{}: {name}", idx + 1));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "stray env reads outside degenbot-config (route through BotConfig or add
         an enumerated, justified entry in this test):\n{}",
        violations.join("\n")
    );
}

/// The loader is the ONLY env-reading surface inside degenbot-config.
///
/// live-invariant gate (ADR-087 D2): watches the live `degenbot-config` production sources, where `loader.rs` is the one env-reading surface; not tripwire precedent.
#[test]
fn config_crate_env_reads_confined_to_the_loader() {
    let crate_dir = workspace_root().join("crates/foundation/degenbot-config");
    let mut files = Vec::new();
    collect_rs_files(&crate_dir, &mut files);
    assert!(
        files.iter().any(|path| path.ends_with("src/loader.rs")),
        "config production scan must include src/loader.rs"
    );
    for path in files {
        // Test harnesses may read their own regeneration controls. This gate
        // protects production configuration surfaces, not test fixtures.
        if path.components().any(|component| {
            matches!(
                component.as_os_str().to_str(),
                Some("tests" | "examples" | "benches")
            )
        }) {
            continue;
        }
        let rel = path
            .strip_prefix(workspace_root())
            .map(|p| p.display().to_string())
            .unwrap_or_default()
            .strip_prefix("rust/")
            .map_or_else(|| path.display().to_string(), str::to_string);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (idx, line) in text.lines().enumerate() {
            if line.contains("env::var") && !line.trim_start().starts_with("//") {
                assert!(
                    rel.ends_with("loader.rs"),
                    "degenbot-config env read outside loader.rs: {rel}:{}",
                    idx + 1
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The Python companion half of the same rule
// ---------------------------------------------------------------------------
//
// The companion is scanned by the shared implementation in
// `scripts/env_surface.py`, shared with the schema-inventory adapter in
// `schema_completeness.rs` and with the default pytest suite. The scan is a
// syntax tree, not a token walk: an import alias resolves to the name it binds,
// and a source that does not parse is a refusal rather than silence. The
// companion's sanctioned reads live beside that scanner (`PYTHON_ALLOWLIST`),
// so a new read fails there and its repair is either routing the value through
// `degenbot-config` or an enumerated, justified row in the scanner.
//
// The comparison that retired the in-tree lexer, and the decision behind the
// shared module, are recorded in
// [ADR-110](../../../../../docs/adr/ADR-110-env-surface-scan-shared-python-ast.md).

mod support;

/// The repository root: `workspace_root()` is `<repo>/rust`, and the Python
/// companion is a sibling tree of the workspace, not a crate inside it.
fn repo_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let repo = workspace_root()
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        assert!(
            repo.join("src").join("degenbot").is_dir(),
            "the repository root is the parent of the rust workspace: {}",
            repo.display()
        );
        repo
    })
}

/// live-invariant gate (ADR-087 D2): watches the live `src/degenbot` companion through the shared scan; not tripwire precedent.
#[test]
fn no_stray_python_env_reads() {
    let output = support::run("python3", repo_root(), &["reads", "--root", "src/degenbot"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stray Python env reads in the companion (route them through degenbot-config or add an
         enumerated, justified entry in scripts/env_surface.py):\n{stdout}{stderr}"
    );
}

/// The negative control for the companion half: a canary tree the shared scan
/// must fail on, driven through the SAME adapter the gate above uses. A scan that
/// quietly stopped reading would leave the gate green, so the control answers
/// with a real child process and a real finding. The scanner's own limits are
/// stated where it is implemented (`scripts/env_surface.py`).
/// live-invariant gate (ADR-087 D2): watches the same companion scan, driven over a canary tree so a silent scan cannot pass; not tripwire precedent.
#[test]
#[expect(
    clippy::expect_used,
    reason = "test helper: the canary tree is this test's own fixture and a write failure invalidates the control"
)]
fn the_companion_scan_fails_on_an_unlisted_read() {
    const CANARY_SOURCE: &str =
        "import os\n\nDEGENBOT_GATE_CANARY = os.environ.get(\"DEGENBOT_GATE_CANARY\")\n";

    let repo = std::env::temp_dir().join(format!("degenbot-env-surface-{}", std::process::id()));
    let canary = repo.join("src").join("degenbot").join("gate_canary.py");
    std::fs::create_dir_all(canary.parent().expect("canary has a parent directory"))
        .expect("canary fixture tree is creatable");
    std::fs::write(&canary, CANARY_SOURCE).expect("canary fixture is writable");

    let output = support::run(
        "python3",
        repo_root(),
        &[
            "reads",
            "--root",
            repo.to_str().expect("the temp path is utf-8"),
        ],
    );
    let _ = std::fs::remove_dir_all(&repo);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let expected = format!(
        "{}/src/degenbot/gate_canary.py:3: DEGENBOT_GATE_CANARY",
        repo.display()
    );
    assert!(
        stdout.contains(&expected),
        "an unlisted read must be reported with its path and name; expected {expected:?} in {stdout:?} (stderr {stderr:?})"
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "a finding is a failure, never a skip: {stdout}{stderr}"
    );
}

/// `python3` is a declared test-tool dependency, so its absence is a named
/// refusal: the gates above fail closed instead of skipping or reading a stale
/// snapshot.
#[test]
#[should_panic(expected = "python3 is a declared test-tool dependency")]
fn a_missing_python3_refuses_instead_of_skipping() {
    let _ = support::run("degenbot-test-no-such-python3", repo_root(), &["reads"]);
}
