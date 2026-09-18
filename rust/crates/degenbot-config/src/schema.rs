//! The typed `BotConfig` schema — THE declaration site.
//!
//! [`SCHEMA`]'s `config_schema!` invocation declares every configuration key
//! exactly once. Each line yields: the typed field on the generated section
//! struct, the `DEGENBOT_*` env name, the dotted TOML path, the typed
//! default, and the generated key-reference doc entry (see `doc`).
//!
//! # TOML / env raw-value conventions
//!
//! - `bool`: a flag word (`1`/`true`/`yes`/`on`/`y` vs empty/`0`/`false`/
//!   `off`/`no`/`n`); `bool_not` inverts the parse (for vars whose `0`
//!   historically ENABLED a legacy behavior).
//! - `ms`: a decimal count of milliseconds (validated `> 0` at the call
//!   sites historically; the schema states the default).
//! - `u128` (wei) is a decimal integer in env; in TOML quote it as a string
//!   (TOML integers are i64, wei values exceed that).
//! - `path`: literal path text; defaults containing `{pid}` are expanded by
//!   the reader at use time.
//! - `string`: raw text.

use std::fmt;

/// The scalar base kind of a declared key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseKind {
    /// Boolean flag (truthy/falsey word lists; see `parse_bool_flag`).
    Bool,
    /// Inverted boolean (legacy `=0`-enables keys).
    BoolInverted,
    /// Milliseconds duration (decimal `u64`).
    Ms,
    /// Free-form text.
    Str,
    /// Filesystem path.
    Path,
    /// Unsigned machine-word size (counts, caps, worker numbers).
    Usize,
    /// Unsigned 64-bit integer.
    U64,
    /// Signed 64-bit integer.
    I64,
    /// Signed 32-bit integer.
    I32,
    /// Decimal integer rendered as text (TOML: quoted) — wei amounts.
    U128,
    /// Floating point.
    F64,
    /// Small closed variant set (generated enum type).
    Enum(&'static str, &'static [&'static str]),
    /// A validated domain -> level map (the generated key type is
    /// `BTreeMap<String, E>`; the element enum name is carried for docs).
    Map(&'static str),
}

impl fmt::Display for BaseKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool => f.write_str("bool"),
            Self::BoolInverted => f.write_str("bool (inverted: `0` enables)"),
            Self::Ms => f.write_str("duration-ms (u64)"),
            Self::Str => f.write_str("string"),
            Self::Path => f.write_str("path"),
            Self::Usize => f.write_str("usize"),
            Self::U64 => f.write_str("u64"),
            Self::I64 => f.write_str("i64"),
            Self::I32 => f.write_str("i32"),
            Self::U128 => f.write_str("u128 (decimal text)"),
            Self::F64 => f.write_str("f64"),
            Self::Enum(name, variants) => write!(f, "{name}({})", variants.join("|")),
            Self::Map(name) => write!(f, "map<string, {name}>"),
        }
    }
}

/// Kind of a declared key: base scalar kind + whether the typed field is
/// `Option<_>` (unset-able).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ValueKind {
    /// Scalar base kind.
    pub base: BaseKind,
    /// `true` when the typed field is `Option<_>` and unset by default.
    pub optional: bool,
}

impl fmt::Display for ValueKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.optional {
            write!(f, "Option<{}>", self.base)
        } else {
            write!(f, "{}", self.base)
        }
    }
}

/// One declared configuration key: the machine-checkable registry entry
/// produced by the single declaration in [`SCHEMA`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyDecl {
    /// Section path segment (TOML table name).
    pub section: &'static str,
    /// Field name within the section (TOML leaf key, `snake_case`).
    pub field: &'static str,
    /// `DEGENBOT_*` environment variable name (operator continuity).
    pub env: &'static str,
    /// Dotted TOML path (`section.field`).
    pub toml_path: &'static str,
    /// Declared kind (drives typed parsing).
    pub kind: ValueKind,
    /// Default rendered for docs (matches `Default` impl).
    pub default_repr: &'static str,
    /// Operator-facing description.
    pub description: &'static str,
}

impl KeyDecl {
    /// Fully qualified key label used in error messages.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{} ({} / {})", self.toml_path, self.env, self.kind)
    }
}

// ⚠ ONE DECLARATION SITE PER KEY BELOW. Do NOT add parallel env/TOML const
// lists anywhere in the workspace; extend this list and regenerate the doc
// (`REGEN_CONFIG_DOCS=1 cargo test -p degenbot-config`).
//
// Ordering is stable (defines doc + SCHEMA iteration order).

crate::config_schema! {

    // the ambient I/O runtime of the two-runtime contract (solve
    // bins on one side, shared I/O runtime on the other) is sized from the
    // cgroup CPU budget; this section carries the explicit operator
    // override for that sizing. Declared once — the typed field, env name,
    // TOML path, and doc entry all come from this line.
    runtime RuntimeConfig {
        io_workers [opt usize] = None, env = "DEGENBOT_IO_WORKERS", def = "(unset; derived from the cgroup CPU budget)",
            doc = "Ambient I/O runtime worker count; when unset it is derived from the cgroup CPU budget after the solve bins take theirs (see solve.solve_cpus / solve.solve_headroom).";
        fleet_profile [enum FleetProfile Auto Pinned Serial] = FleetProfile::Auto, env = "DEGENBOT_FLEET_PROFILE", def = "auto",
            doc = "Fleet host-binding profile (FLEETFLOOR FF-T2): auto resolves the host tier from the CPU budget (pinned at/above the pinned-role floor, serial on 2-5 cores, refused below 2); pinned/serial force a binding — a forced pinned binding below the floor runs marked oversubscribed, and forced bindings still need 2 or more cores.";
    }

    telemetry TelemetryConfig {
        log_level [opt enum LogLevel Off Error Warn Info Debug Trace] = None, env = "DEGENBOT_LOG_LEVEL", def = "(unset: wiring default)",
            doc = "Console wiring default level, used only when RUST_LOG is absent (off|error|warn|info|debug|trace). The standalone Rust bot wiring defaults to warn; the Python driver to info.";
        diag [map LogLevel] = ::std::collections::BTreeMap::new(), env = "DEGENBOT_TELEMETRY_DIAG", def = "(empty)",
            doc = "Per-domain console escalation from a validated map: [telemetry.diag] with sim = \"debug\", or the env form sim=debug,solver=trace. A typo'd domain is a boot error. Ignored with one WARN when RUST_LOG is set.";
        otel [bool] = true, env = "DEGENBOT_OTEL", def = "true",
            doc = "Enable the OTel OTLP span layer and the Prometheus metrics endpoint; `0`/empty opts out.";
        metrics_addr [string] = String::from("127.0.0.1:9464"), env = "DEGENBOT_METRICS_ADDR", def = "127.0.0.1:9464",
            doc = "Prometheus scrape endpoint bind address (only active when otel is on).";
        jaeger_endpoint [string] = String::from("http://127.0.0.1:4318"), env = "DEGENBOT_JAEGER_ENDPOINT", def = "http://127.0.0.1:4318",
            doc = "OTLP endpoint used by the opt-in Jaeger E2E test.";
        jaeger_e2e [bool] = false, env = "DEGENBOT_JAEGER_E2E", def = "false",
            doc = "Gate for the network-accessible Jaeger E2E test (Jaeger must be reachable at jaeger_endpoint).";
    }

    // Per-session run artifacts: one directory per process lifetime holding
    // the captured stdout log and JSONL trace. The root is a typed key so the
    // location is file/env settable like every other runtime path; a leading
    // `~` resolves against HOME at use (degenbot-runs).
    logging LoggingConfig {
        runs_dir [path] = std::path::PathBuf::from("~/.local/state/degenbot/logs"), env = "DEGENBOT_RUNS_DIR", def = "~/.local/state/degenbot/logs",
            doc = "Root directory for per-session run artifacts: each session lands in `<runs_dir>/<engine>/<UTC-stamp>-<pid>/` holding stdout.log and trace.jsonl, with a best-effort `latest` symlink beside it. The default is the XDG state home (`$XDG_STATE_HOME` when absolute, else `$HOME/.local/state`); a leading `~` expands against HOME. There is deliberately no rotation, compression, or size cap.";
        log_stderr [bool] = false, env = "DEGENBOT_LOG_STDERR", def = "false",
            doc = "Also mirror the session stdout log to stderr (interactive runs); the file-only posture is the default. A bad value fails the load rather than silently picking a sink.";
        trace_jsonl [opt path] = None, env = "DEGENBOT_TRACE_JSONL", def = "(unset; the session trace.jsonl under logging.runs_dir)",
            doc = "Explicit offline-review JSONL capture path. When unset, the trace helpers append to the session's `trace.jsonl` under logging.runs_dir.";
        dry_run_jsonl [opt path] = None, env = "DEGENBOT_DRY_RUN_JSONL", def = "(unset; live feed)",
            doc = "Dry-run fixture frames path: a captured frame JSONL replaces the live feed and is processed once, in order. Unset keeps the live feed.";
    }

    // Durable state that OUTLIVES a process lifetime: unlike per-session run
    // artifacts, this is the restart-survival surface. The root is a typed key
    // so the location is file/env settable like every other runtime path; a
    // leading `~` resolves against HOME at use (degenbot-runs).
    persistence PersistenceConfig {
        state_dir [path] = std::path::PathBuf::from("~/.local/state/degenbot/state"), env = "DEGENBOT_STATE_DIR", def = "~/.local/state/degenbot/state",
            doc = "Root directory for durable, process-lifetime-independent bot state (e.g. the backrun sidecar's gap-quarantine journal). State here OUTLIVES sessions and is deliberately NOT nested under a per-session run directory. The default is the XDG state home (`$XDG_STATE_HOME` when absolute, else `$HOME/.local/state`); a leading `~` expands against HOME.";
    }

    allocator AllocatorConfig {
        mimalloc_purge_delay_ms [opt i64] = None, env = "DEGENBOT_MIMALLOC_PURGE_DELAY_MS", def = "(unset)",
            doc = "Fixed mimalloc purge delay in ms overriding cadence discovery entirely (clamped 1_000..=600_000).";
        mimalloc_auto_purge [bool] = true, env = "DEGENBOT_MIMALLOC_AUTO_PURGE", def = "true",
            doc = "Whether mimalloc cadence discovery may re-apply the purge option; `0` disables.";
        mimalloc_purge_delay_mult [f64] = 2.0, env = "DEGENBOT_MIMALLOC_PURGE_DELAY_MULT", def = "2.0",
            doc = "Purge-delay multiplier over the mean block interval (clamped 1.0..=20.0).";
        mimalloc_purge_decommits [bool] = false, env = "DEGENBOT_MIMALLOC_PURGE_DECOMMITS", def = "false",
            doc = "Purge with MADV_DONTNEED instead of MADV_FREE (`1`/`true` enables aggressive decommit).";
    }

    state_lock StateLockConfig {
        trace [bool] = false, env = "DEGENBOT_LOCK_TRACE", def = "false",
            doc = "Capture full backtraces at lock acquire (diagnostics).";
        warn_ms [ms] = 500, env = "DEGENBOT_LOCK_WARN_MS", def = "500",
            doc = "Warn threshold (ms) for read-hold age and write-acquire block (clamped >= 1).";
        diag [bool] = false, env = "DEGENBOT_STATE_LOCK_DIAG", def = "false",
            doc = "Enable hold-tracking diagnostics for soak/incident forensics.";
        thread_registry_path [path] = std::path::PathBuf::from("/tmp/degenbot-thread-registry-{pid}.json"), env = "DEGENBOT_THREAD_REGISTRY_PATH", def = "/tmp/degenbot-thread-registry-{pid}.json",
            doc = "Watchdog thread-registry dump path; `{pid}` is substituted with the process id at use.";
    }

    pump PumpConfig {
        pump_debounce_ms [ms] = 50, env = "DEGENBOT_PUMP_DEBOUNCE_MS", def = "50",
            doc = "Publish-debounce settle window in ms (`> 0`; bad values historically fall back to 50 at the site).";
        early_slice_ms [ms] = 25, env = "DEGENBOT_EARLY_SLICE_MS", def = "25",
            doc = "Early-slice window in ms; `0` valid and disables the slice (settle-only parity).";
        streaming_delivery [bool] = true, env = "DEGENBOT_STREAMING_DELIVERY", def = "true",
            doc = "Stream solved arms immediately (T3 default); `0` opts out to the debounce sweep.";
        ws_completeness [bool] = true, env = "DEGENBOT_WS_COMPLETENESS", def = "true",
            doc = "WS completeness gating (newHeads + logs double-delivery check); `0` disables.";
        // the
        // adaptive trailing-quiesce estimator. Default flipped to `adaptive`
        // after the 2026-09-09 live A/B (the untracked `logs/` run artifact
        // logs/perf-after-20260909.md: settle
        // p50 25→10ms, publish p95 100→50ms, zero late-admit tripwires);
        // `fixed` retains the historical `pump_debounce_ms` behavior. W =
        // clamp(EWMA·quiesce_margin, floor, ceil), the EWMA tracking each
        // block's max intra-block silence gap.
        quiesce_mode [enum QuiesceMode Fixed Adaptive] = QuiesceMode::Adaptive, env = "DEGENBOT_PUMP_QUIESCE_MODE", def = "adaptive",
            doc = "Settle-window mode: `fixed` = constant pump.pump_debounce_ms debounce; `adaptive` = EWMA trailing-quiesce estimator (never below quiesce_floor_ms, never above quiesce_ceil_ms; late-admit budget overruns hold at the ceiling).";
        quiesce_floor_ms [ms] = 2, env = "DEGENBOT_PUMP_QUIESCE_FLOOR_MS", def = "2",
            doc = "Adaptive mode: lower bound (ms) of the trailing settle window (clamped >= 1; a 0/garbage value never collapses the window to zero).";
        quiesce_ceil_ms [ms] = 20, env = "DEGENBOT_PUMP_QUIESCE_CEIL_MS", def = "20",
            doc = "Adaptive mode: upper bound (ms) of the trailing settle window; the estimator never grows beyond it (inherits the debounce parse contract: unset/zero/invalid falls back, never 0).";
        quiesce_margin_ms [f64] = 3.0, env = "DEGENBOT_PUMP_QUIESCE_MARGIN_MS", def = "3.0",
            doc = "Adaptive mode: safety multiplier over the silence-gap EWMA (W = EWMA × margin, then floor/ceiling clamp).";
        quiesce_ewma_alpha [f64] = 0.1, env = "DEGENBOT_PUMP_QUIESCE_EWMA_ALPHA", def = "0.1",
            doc = "Adaptive mode: EWMA smoothing constant over per-block max silence gaps (≈10-block memory); clamped to (0, 1].";
        quiesce_late_budget [u64] = 120, env = "DEGENBOT_PUMP_QUIESCE_LATE_BUDGET", def = "120",
            doc = "Adaptive-mode runtime backstop: more than this many benign late-admit events in a sliding hour holds the window at quiesce_ceil_ms until the ledger drains.";
    }

    trace TraceConfig {
        hotpath [bool] = false, env = "DEGENBOT_HOTPATH", def = "false",
            doc = "Construct the hotpath profiling guard (default OFF; build must enable the profiling feature too).";
    }

    solve SolveConfig {
        solve_cpus [opt usize] = None, env = "DEGENBOT_SOLVE_CPUS", def = "(unset)",
            doc = "Override the detected solve CPU budget (worker bin count).";
        solve_headroom [opt usize] = None, env = "DEGENBOT_SOLVE_HEADROOM", def = "(unset)",
            doc = "Override the I/O headroom carved out of the CPU budget before solve bins.";
        solve_inline_sim [bool] = true, env = "DEGENBOT_SOLVE_INLINE_SIM", def = "true",
            doc = "Inline-sim stance (T2 worker-side clamp path); `0`/`false` disables.";
        solve_resolve_par [bool] = true, env = "DEGENBOT_SOLVE_RESOLVE_PAR", def = "true",
            doc = "Chunked parallel resolve stance; `0`/`off`/`false`/`disabled` disables.";
        inline_sim_workers [opt usize] = None, env = "DEGENBOT_INLINE_SIM_WORKERS", def = "(unset; derived from CPU budget)",
            doc = "Inline-sim worker count (clamped 1..=32; unparsable falls back to derived default at the site).";
        admission_shed [bool] = true, env = "DEGENBOT_SOLVE_ADMISSION", def = "true",
            doc = "QTZGFL capacity-modulated admission stance: unset/`1`/`true`/`on` (default) replaces the retired in-flight cap degrade with budget = max(0, admission_target_depth - in-flight) and SHEDS zero-budget cycles; `0`/`false`/`off` is the intentional take-all no-shed operator stance (A/B).";
        admission_target_depth [usize] = 8, env = "DEGENBOT_SOLVE_ADMISSION_TARGET_DEPTH", def = "8",
            doc = "QTZGFL: un-merged-result pipe depth target in KEYS (pools) — the draw budget headroom. Clamped at engine construction to 1..=DETACHED_INFLIGHT_CAP (DETACHED_INFLIGHT_CAP is only the default/clamp for this key now, no longer a runtime cap verdict); default 8 = DETACHED_INFLIGHT_CAP.";
        admission_retention_blocks [u64] = 50, env = "DEGENBOT_SOLVE_ADMISSION_RETENTION", def = "50",
            doc = "QTZGFL: retained (carried) key retention window W in blocks — on each block advance the ledger prunes buckets older than head - W and counts degenbot.detached.leads_expired. Default 50.";
        min_profit_wei [u128] = 0, env = "DEGENBOT_MIN_PROFIT_WEI", def = "0",
            doc = "Minimum path profit floor in wei (decimal text; TOML: quoted string).";
        walk_event_solver_legacy [bool_not] = false, env = "DEGENBOT_WALK_EVENT_SOLVER", def = "false",
            doc = "Legacy event-solver path is enabled by `DEGENBOT_WALK_EVENT_SOLVER=0` (inverted flag).";
        walk_event_census [bool] = false, env = "DEGENBOT_WALK_EVENT_CENSUS", def = "false",
            doc = "Loop-15 nested event census counters in the CL walker (`1` enables).";
        walk_anchor_sweep [enum AnchorSweep Off CenterOnly Full] = AnchorSweep::Full, env = "DEGENBOT_WALK_ANCHOR_SWEEP", def = "full",
            doc = "Walk anchor-sweep posture: `off` (0), `center-only` (2), or `full` (default/anything else).";
        envelope_max_tangent_lines [usize] = 32, env = "DEGENBOT_ENVELOPE_MAX_TANGENT_LINES", def = "32",
            doc = "Profit-envelope max tangent lines cap.";
        envelope_sampled_compose_lines [usize] = 48, env = "DEGENBOT_ENVELOPE_SAMPLED_COMPOSE_LINES", def = "48",
            doc = "Profit-envelope sampled-compose lines cap.";
        solver_walk_memo [bool] = false, env = "DEGENBOT_SOLVER_WALK_MEMO", def = "false",
            doc = "CL-solver walk memo (result caching) (`1` enables).";
        solver_walk_memo_stats [bool] = false, env = "DEGENBOT_SOLVER_WALK_MEMO_STATS", def = "false",
            doc = "Walk-memo recomposition census (`1` enables).";
        cl_projection_cache [bool] = true, env = "DEGENBOT_CL_PROJECTION_CACHE", def = "true",
            doc = "CL projection memo cache; `0`/`off`/`false`/`disabled` disables.";
    }

    fleet FleetConfig {
        quota_cpus [opt f64] = None, env = "DEGENBOT_FLEET_QUOTA_CPUS", def = "(unset; detected from the cgroup)",
            doc = "Terminal override of the fractional cgroup CPU quota (cores) feeding the fleet budget sum check; unset detects from the cgroup (ADR-042 §5).";
        reserve_cpus [opt usize] = None, env = "DEGENBOT_FLEET_RESERVE_CPUS", def = "(unset; default 1)",
            doc = "Fleet-budget reserve share H (Python bridge, pump, OTel, async GC) in cores; overrides the fixed default 1 (design doc §5).";
        solver_cpus [opt usize] = None, env = "DEGENBOT_FLEET_SOLVER_CPUS", def = "(unset; derived floor(Q)-H-A-R-M)",
            doc = "Fleet Solver CPU share S in cores; terminal when set and it participates in the same startup sum check (default: floor(quota) − H − A − R − M; S < 2 fails the boot).";
        sim_slot_cap [opt usize] = None, env = "DEGENBOT_FLEET_SIM_SLOT_CAP", def = "(unset; default 4)",
            doc = "SimDriver slot cap (duty-counted; spendable from the fractional-quota remainder); default 4, today's SimSlots cap.";
        pool_state_updater_slots [opt usize] = None, env = "DEGENBOT_FLEET_POOL_STATE_UPDATER_SLOTS", def = "(unset; default 4)",
            doc = "PoolStateUpdater slot cap (the registration intake station: duty-counted, spendable from the fractional-quota remainder, Deferrable cordon class); default 4.";
        cordon_enter_events [usize] = 2, env = "DEGENBOT_FLEET_CORDON_ENTER_EVENTS", def = "2",
            doc = "Throttle events within the enter window that cordon the fleet (design doc §6 enter trigger; Q5 amendment delivered: runtime-tunable via the operator channel — `degenbot fleet posture set --cordon-enter-events N` on a live process).";
        cordon_enter_window_ms [ms] = 1000, env = "DEGENBOT_FLEET_CORDON_ENTER_WINDOW_MS", def = "1000",
            doc = "Rolling window (ms) for the throttle-event burst enter trigger.";
        cordon_duty_percent [f64] = 2.0, env = "DEGENBOT_FLEET_CORDON_DUTY_PERCENT", def = "2.0",
            doc = "Throttled-time duty percent over the duty window that cordons the fleet (enter trigger; 2.0 = >2%).";
        cordon_duty_window_ms [ms] = 5000, env = "DEGENBOT_FLEET_CORDON_DUTY_WINDOW_MS", def = "5000",
            doc = "Trailing window (ms) over which throttled-time duty is evaluated.";
        cordon_exit_clean_ms [ms] = 10000, env = "DEGENBOT_FLEET_CORDON_EXIT_CLEAN_MS", def = "10000",
            doc = "Clean-window hysteresis (ms) required before cordon exits (design doc §6: 10 s of clean windows).";
        cordon_sim_intake_floor [opt usize] = None, env = "DEGENBOT_FLEET_CORDON_SIM_INTAKE_FLOOR", def = "(unset; half the slot cap)",
            doc = "SimDriver new-lease cap while cordoned; in-flight sims are never cancelled (default: half the slot cap).";
        intake_backstop_ms [ms] = 250, env = "DEGENBOT_FLEET_INTAKE_BACKSTOP_MS", def = "250",
            doc = "Backstop recv-timeout (ms) armed iff a host intake backlog is non-empty; on timeout the host re-runs the grant pump, so a posture lift with no further message still drains held units (TB4QGX T2). Clamped to >= 1 ms at the site so a garbage value cannot collapse into a busy-spin.";
        intake_no_progress_ticks [usize] = 8, env = "DEGENBOT_FLEET_INTAKE_NO_PROGRESS_TICKS", def = "8",
            doc = "Consecutive admitted-but-no-progress host intake passes (backlog non-empty, admission admits, yet no dequeue and no grant) before the host fails LOUDLY (TB4QGX T4). Legitimate WaitCap/WaitPosture holds never count, and only real progress resets the counter. Clamped to >= 1 at the site so a garbage value cannot trip on the first pass.";
    }

    capture CaptureConfig {
        gate_capture [bool] = false, env = "DEGENBOT_GATE_CAPTURE", def = "false",
            doc = "Gate degenerate-path capture (presence gates; loader parses a bool).";
        gate_capture_out [path] = std::path::PathBuf::from("/tmp/gate_degenerate.jsonl"), env = "DEGENBOT_GATE_CAPTURE_OUT", def = "/tmp/gate_degenerate.jsonl",
            doc = "Gate-capture output JSONL path.";
        gate_capture_cap [usize] = 50, env = "DEGENBOT_GATE_CAPTURE_CAP", def = "50",
            doc = "Max paths captured per gate-capture run.";
        solver_capture [bool] = false, env = "DEGENBOT_SOLVER_CAPTURE", def = "false",
            doc = "Heavy CL-path solve capture (presence gates; loader parses a bool).";
        solver_capture_cap [usize] = 16, env = "DEGENBOT_SOLVER_CAPTURE_CAP", def = "16",
            doc = "Max captures per run (deduped by path id).";
        solver_capture_min_sims [u64] = 2000, env = "DEGENBOT_SOLVER_CAPTURE_MIN_SIMS", def = "2000",
            doc = "Capture only paths with at least this many walk sims.";
        solver_capture_min_us [u64] = 50000, env = "DEGENBOT_SOLVER_CAPTURE_MIN_US", def = "50000",
            doc = "Capture only paths whose solve time is at least this many microseconds.";
        solver_capture_out [opt path] = None, env = "DEGENBOT_SOLVER_CAPTURE_OUT", def = "(unset; site picks a working-file default)",
            doc = "Solver-capture output JSONL path (site-specific fallback).";
        swap_capture_probe [bool] = false, env = "DEGENBOT_SWAP_CAPTURE_PROBE", def = "false",
            doc = "Swap-capture correctness example gate (`1` enables).";
    }

    verify VerifyConfig {
        verify_spotcheck_permyriad [u64] = 0, env = "DEGENBOT_VERIFY_SPOTCHECK_PERMYRIAD", def = "0",
            doc = "Per-myriad (1/10_000) sampling rate for verify spot-checks (0 = off).";
    }

    simulation SimulationConfig {
        sim_execute_gas [opt u64] = None, env = "DEGENBOT_SIM_EXECUTE_GAS", def = "(unset; EIP-7825 TX_GAS_LIMIT_CAP)",
            doc = "Override the execute() gas limit (decimal u64; garbage/0 falls back at the site while migrating).";
        inject_executor_code [bool] = false, env = "DEGENBOT_INJECT_EXECUTOR_CODE", def = "false",
            doc = "Simulate against executor bytecode injected into the evm overlay instead of a deployed contract. Injection mode also gates live submission off for safety; `1` opts in (dry-run/dev posture).";
        sim_exit_on_fail [bool] = false, env = "DEGENBOT_SIM_EXIT_ON_FAIL", def = "false",
            doc = "Abort the process when a sim fails (the live trap used to capture V3-hop fixtures).";
        sim_serve_engine_state [bool] = false, env = "DEGENBOT_SIM_SERVE_ENGINE_STATE", def = "false",
            doc = "Serve engine state to the sim/evm layer (`1` enables; behavior change, default off).";
        probe_fixture [opt path] = None, env = "DEGENBOT_PROBE_FIXTURE", def = "(unset)",
            doc = "Corpus fixture for the offline executor A/B probe (ignore-listed test).";
        probe_ns [string] = String::from("1,2,4,8,16"), env = "DEGENBOT_PROBE_NS", def = "1,2,4,8,16",
            doc = "Comma-separated thread-count arms for the offline executor A/B probe.";
        probe_passes [usize] = 3, env = "DEGENBOT_PROBE_PASSES", def = "3",
            doc = "Passes per arm for the offline executor A/B probe.";
    }

    // 4IOEVT: discovery delivery batching. The startup discovery sweep's
    // async wrapper delivers paths in batches (one event-loop hop per batch)
    // instead of one hop per path; this typed key makes the batch size
    // operator-settable without a code change.
    pathfinding PathfindingConfig {
        discovery_batch_size [usize] = 1000, env = "DEGENBOT_DISCOVERY_BATCH_SIZE", def = "1000",
            doc = "Discovery-sweep delivery batch size (paths per async batch): the worker thread collects this many paths before the async consumer yields them and gives the event loop one turn. A value <= 1 falls back to the legacy per-path delivery.";
    }

    // The typed strategy selector plus the per-arm facet namespaces. One
    // declaration site for the operator-facing choice; the settlement and
    // backrun readers migrate onto this key separately.
    strategy StrategyConfig {
        name [opt enum StrategyName Settlement Backrun] = None, env = "DEGENBOT_STRATEGY_NAME", def = "(unset; no explicit strategy selection)",
            doc = "Active strategy arm: `settlement` or `backrun`. Unset leaves strategy selection at the wiring default; the settlement/backrun readers consume this key in a later phase.";

        // Per-arm facet namespaces: the typed config home each strategy's own
        // knobs land in. The backrun sidecar's operator surface is declared
        // here (single declaration site per key); settlement is still empty
        // because its knobs have not migrated yet. ADR-055 Phase C adds
        // `.enabled` to both.
        settlement StrategySettlementConfig {}
        backrun StrategyBackrunConfig {
            bid_mode [bool] = false, env = "DEGENBOT_STRATEGY_BACKRUN_BID_MODE", def = "false",
                doc = "Explicit bid-mode flag; off is observe-only. Bid mode also requires a non-zero budget_wei (the legality gate reads both).";
            budget_wei [u128] = 0, env = "DEGENBOT_STRATEGY_BACKRUN_BUDGET_WEI", def = "0",
                doc = "Cumulative bid budget cap in wei (decimal text; TOML: quoted string). Zero makes bid mode illegal; the spent accumulator tracks the wallet's gas burn.";
            max_bundle_wei [u128] = 1_000_000_000_000_000u128, env = "DEGENBOT_STRATEGY_BACKRUN_MAX_BUNDLE_WEI", def = "1000000000000000",
                doc = "Hard per-bundle cap in wei (decimal text; TOML: quoted string); a decided bid is clamped to it.";
            bribe_bips [u64] = 9800, env = "DEGENBOT_STRATEGY_BACKRUN_BRIBE_BIPS", def = "9800",
                doc = "The builder's bribe share of true profit in bips of 10_000 — the competitiveness ceiling (clamped at the site to <= 10_000).";
            priority_fee_gwei [u64] = 2, env = "DEGENBOT_STRATEGY_BACKRUN_PRIORITY_FEE_GWEI", def = "2",
                doc = "The operator's priority fee in gwei, converted to wei when pricing the wallet's gas burn.";
            bundle_gas_est [u64] = 300_000, env = "DEGENBOT_STRATEGY_BACKRUN_BUNDLE_GAS_EST", def = "300000",
                doc = "Composed-bundle gas estimate priced into the net-of-gas bid gate until an exact in-scratch measurement replaces it.";
            dry_run [bool] = false, env = "DEGENBOT_STRATEGY_BACKRUN_DRY_RUN", def = "false",
                doc = "Sign-nothing dispatch: every candidate skips as DryRun. Plain bool words are accepted (the old `SIDECAR_DRY_RUN=1` spelling included).";
            key_file [opt path] = None, env = "DEGENBOT_STRATEGY_BACKRUN_KEY_FILE", def = "(unset)",
                doc = "Hex secp256k1 operator key file. Unset means no signing material is loaded (observe-only); the key never leaves TxSigner.";
            executor [string] = String::from("0x30b28ed8aa581fbc0191c3b532b0697773070e97"), env = "DEGENBOT_STRATEGY_BACKRUN_EXECUTOR", def = "0x30b28ed8aa581fbc0191c3b532b0697773070e97",
                doc = "Executor contract address the composed backrun calls; parsed and validated at the sidecar boot.";
            operator [opt string] = None, env = "DEGENBOT_STRATEGY_BACKRUN_OPERATOR", def = "(unset; falls back to EXECUTOR_OWNER_ADDRESS)",
                doc = "Executor owner / sim caller address. Unset falls back to the legacy EXECUTOR_OWNER_ADDRESS env name, then the built-in default; parsed at the sidecar boot.";
            sim_url [opt string] = None, env = "DEGENBOT_STRATEGY_BACKRUN_SIM_URL", def = "(unset; the chain node)",
                doc = "Bundle-sim endpoint serving eth_callMany. Unset reuses the chain node; MEVBlocker's /fast tier answers method-missing, so the node is the fallback.";
            stream_url [string] = String::new(), env = "DEGENBOT_STRATEGY_BACKRUN_STREAM_URL", def = "(empty: the MEVBlocker searcher WS default)",
                doc = "MEVBlocker searcher WebSocket for the private bundle broadcast. Empty defers to the feed crate's mainnet default so the endpoint lives in one place.";
            rank_evidence [bool] = false, env = "DEGENBOT_STRATEGY_BACKRUN_RANK_EVIDENCE", def = "false",
                doc = "Run the live deep-pair ranking sanity probe before any frame trusts the connector-depth truncation (diagnostic).";
            connectors [usize] = 8, env = "DEGENBOT_STRATEGY_BACKRUN_CONNECTORS", def = "8",
                doc = "Discovery fan-out cap (connectors per frame).";
            fixture_head [opt u64] = None, env = "DEGENBOT_STRATEGY_BACKRUN_FIXTURE_HEAD", def = "(unset)",
                doc = "Offline dry-run's pinned head block: replays captured frames against the chain view they were pending in instead of the live tip. Unset falls back to the fetched head.";
            stop_file [path] = std::path::PathBuf::from("/tmp/degenbot-sidecar-STOP"), env = "DEGENBOT_STRATEGY_BACKRUN_STOP_FILE", def = "/tmp/degenbot-sidecar-STOP",
                doc = "Kill-switch path: while the file exists the decision layer drops every candidate and the loop halts. Host-lifecycle debt tracked for Phase C; facet-owned until the host owns lifecycle.";
        }
    }

    aave AaveConfig {
        bridge_probe [bool] = false, env = "DEGENBOT_BRIDGE_PROBE", def = "false",
            doc = "In-tree bridge-probe observation surface in the arbitrage simulator (presence gates).";
    }

    offline OfflineConfig {
        clcap_rpc [opt string] = None, env = "DEGENBOT_CLCAP_RPC", def = "(unset; required by the capture examples)",
            doc = "RPC URL for the offline CL capture/boundary-scan examples (network-gated).";
        clcap_block [opt u64] = None, env = "DEGENBOT_CLCAP_BLOCK", def = "(unset)",
            doc = "Pinned block for the CL capture generator.";
        clcap_max_fetches [usize] = 320, env = "DEGENBOT_CLCAP_MAX_FETCHES", def = "320",
            doc = "Max active-set fetches backfilled by the CL capture generator.";
        clcap_path_cap [usize] = 12, env = "DEGENBOT_CLCAP_PATH_CAP", def = "12",
            doc = "Per-block path cap for the CL capture generator.";
        scan_block [opt u64] = None, env = "DEGENBOT_SCAN_BLOCK", def = "(unset)",
            doc = "Block for the CL boundary-scan examples.";
        fixture_db [opt path] = None, env = "DEGENBOT_FIXTURE_DB", def = "(unset)",
            doc = "Database path for the standalone-consumer fixture example.";
        v3_fixture_rpc [opt string] = None, env = "DEGENBOT_V3_FIXTURE_RPC", def = "(unset)",
            doc = "Network-gated V3 IIA fixture reproduction RPC endpoint (test-only).";
        v3_fixture_block [opt u64] = None, env = "DEGENBOT_V3_FIXTURE_BLOCK", def = "(unset)",
            doc = "Network-gated V3 IIA fixture reproduction block (test-only).";
    }

    test_hooks TestHookConfig {
        alloc_track [bool] = false, env = "DEGENBOT_ALLOC_TRACK", def = "false",
            doc = "Allocation-tracking gate for the math/pools bench suites (`1` enables).";
        self_abort_test [bool] = false, env = "DEGENBOT_SELF_ABORT_TEST", def = "false",
            doc = "Block-pump self-abort hatch (presence gates in tests).";
        unused_test_flag [bool] = true, env = "DEGENBOT_UNUSED_TEST_FLAG", def = "true",
            doc = "Default-ON flag-parse probe (asserted by the bot-core unit tests).";
    }

}

// Dynamic per-chain vars (documented, intentionally NOT static schema keys):
//   DEGENBOT_RPC_WS_CHAINID_<chain_id> — per-chain WS RPC override, the
//   suffix is the numeric chain id (see block_pump chain-id plumbing).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_paths_are_wellformed_and_unique() {
        let mut seen_toml = std::collections::BTreeSet::new();
        let mut seen_env = std::collections::BTreeSet::new();
        for k in SCHEMA {
            assert!(!k.section.is_empty() && !k.field.is_empty());
            assert!(
                k.env.starts_with("DEGENBOT_"),
                "env must be DEGENBOT_-prefixed"
            );
            assert!(
                seen_toml.insert(k.toml_path),
                "duplicate toml path {}",
                k.toml_path
            );
            assert!(seen_env.insert(k.env), "duplicate env name {}", k.env);
            assert!(
                !k.description.is_empty(),
                "missing description for {}",
                k.toml_path
            );
        }
    }

    #[test]
    fn fleet_section_declares_the_adr_042_keys() {
        let want = [
            ("fleet.quota_cpus", "DEGENBOT_FLEET_QUOTA_CPUS"),
            ("fleet.reserve_cpus", "DEGENBOT_FLEET_RESERVE_CPUS"),
            ("fleet.solver_cpus", "DEGENBOT_FLEET_SOLVER_CPUS"),
            ("fleet.sim_slot_cap", "DEGENBOT_FLEET_SIM_SLOT_CAP"),
            (
                "fleet.cordon_enter_events",
                "DEGENBOT_FLEET_CORDON_ENTER_EVENTS",
            ),
            (
                "fleet.cordon_enter_window_ms",
                "DEGENBOT_FLEET_CORDON_ENTER_WINDOW_MS",
            ),
            (
                "fleet.cordon_duty_percent",
                "DEGENBOT_FLEET_CORDON_DUTY_PERCENT",
            ),
            (
                "fleet.cordon_duty_window_ms",
                "DEGENBOT_FLEET_CORDON_DUTY_WINDOW_MS",
            ),
            (
                "fleet.cordon_exit_clean_ms",
                "DEGENBOT_FLEET_CORDON_EXIT_CLEAN_MS",
            ),
            (
                "fleet.cordon_sim_intake_floor",
                "DEGENBOT_FLEET_CORDON_SIM_INTAKE_FLOOR",
            ),
        ];
        for (toml_path, env) in want {
            let key = SCHEMA.iter().find(|k| k.toml_path == toml_path);
            assert!(
                key.is_some(),
                "schema key {toml_path} must be declared exactly once"
            );
            assert_eq!(key.map(|k| k.env), Some(env), "{toml_path} env drift");
        }
        // LW-T9 hard cutover: the stance key is RETIRED — it must not be
        // declared (mirror of the P6YXA6 solve.executor removal; the env
        // var + TOML key fail the load loudly at the loader).
        assert!(
            !SCHEMA.iter().any(|k| k.toml_path == "fleet.stance"),
            "fleet.stance must be retired from the schema"
        );
        assert!(
            !SCHEMA.iter().any(|k| k.env == "DEGENBOT_FLEET"),
            "DEGENBOT_FLEET must be retired from the schema"
        );
    }

    #[test]
    fn inject_executor_code_is_declared_with_default_false() {
        let key = SCHEMA
            .iter()
            .find(|k| k.toml_path == "simulation.inject_executor_code");
        assert!(key.is_some(), "key must be declared exactly once");
        assert_eq!(key.map(|k| k.env), Some("DEGENBOT_INJECT_EXECUTOR_CODE"));
        assert!(
            !BotConfig::default().simulation.inject_executor_code,
            "production default is the deployed executor: injection is opt-in"
        );
    }

    #[test]
    fn pathfinding_discovery_batch_size_is_declared_with_default_1000() {
        let key = SCHEMA
            .iter()
            .find(|k| k.toml_path == "pathfinding.discovery_batch_size");
        assert!(key.is_some(), "4IOEVT key must be declared exactly once");
        assert_eq!(key.map(|k| k.env), Some("DEGENBOT_DISCOVERY_BATCH_SIZE"));
        assert_eq!(BotConfig::default().pathfinding.discovery_batch_size, 1000);
    }

    #[test]
    fn logging_runs_dir_is_declared_with_default_under_home() {
        let key = SCHEMA.iter().find(|k| k.toml_path == "logging.runs_dir");
        assert!(
            key.is_some(),
            "logging.runs_dir must be declared exactly once"
        );
        assert_eq!(key.map(|k| k.env), Some("DEGENBOT_RUNS_DIR"));
        assert_eq!(
            key.map(|k| k.kind),
            Some(ValueKind {
                base: BaseKind::Path,
                optional: false,
            })
        );
        assert_eq!(
            BotConfig::default().logging.runs_dir,
            std::path::PathBuf::from("~/.local/state/degenbot/logs")
        );
    }

    #[test]
    fn persistence_state_dir_is_declared_with_default_under_home() {
        let key = SCHEMA
            .iter()
            .find(|k| k.toml_path == "persistence.state_dir");
        assert!(
            key.is_some(),
            "persistence.state_dir must be declared exactly once"
        );
        assert_eq!(key.map(|k| k.env), Some("DEGENBOT_STATE_DIR"));
        assert_eq!(
            key.map(|k| k.kind),
            Some(ValueKind {
                base: BaseKind::Path,
                optional: false,
            })
        );
        assert_eq!(
            BotConfig::default().persistence.state_dir,
            std::path::PathBuf::from("~/.local/state/degenbot/state")
        );
    }

    #[test]
    fn strategy_name_is_declared_as_optional_enum() {
        let key = SCHEMA.iter().find(|k| k.toml_path == "strategy.name");
        assert!(key.is_some(), "strategy.name must be declared exactly once");
        assert_eq!(key.map(|k| k.env), Some("DEGENBOT_STRATEGY_NAME"));
        assert_eq!(
            key.map(|k| k.kind),
            Some(ValueKind {
                base: BaseKind::Enum("StrategyName", &["Settlement", "Backrun"]),
                optional: true,
            })
        );
        assert_eq!(BotConfig::default().strategy.name, None);
    }

    #[test]
    fn strategy_name_parses_its_variants_case_insensitively() {
        let mut config = BotConfig::default();
        assert!(config.assign("strategy", "name", "settlement").is_ok());
        assert_eq!(config.strategy.name, Some(StrategyName::Settlement));
        assert!(config.assign("strategy", "name", "Backrun").is_ok());
        assert_eq!(config.strategy.name, Some(StrategyName::Backrun));
        assert!(config.assign("strategy", "name", "sandwich").is_err());
    }

    #[test]
    fn strategy_facets_are_declared_as_typed_sections() {
        // The per-arm facet namespaces exist as typed fields with dotted TOML
        // section paths. The backrun surface is declared under the dotted
        // facet section; settlement remains keyless.
        let config = BotConfig::default();
        assert_eq!(
            config.strategy.settlement,
            StrategySettlementConfig::default()
        );
        assert_eq!(config.strategy.backrun, StrategyBackrunConfig::default());
        assert!(SECTION_PATHS.contains(&"strategy.settlement"));
        assert!(SECTION_PATHS.contains(&"strategy.backrun"));
        assert!(
            !SCHEMA.iter().any(|k| k.section == "strategy.settlement"),
            "the settlement facet declares no keys yet"
        );
        let backrun: Vec<&str> = SCHEMA
            .iter()
            .filter(|k| k.section == "strategy.backrun")
            .map(|k| k.field)
            .collect();
        assert_eq!(
            backrun,
            vec![
                "bid_mode",
                "budget_wei",
                "max_bundle_wei",
                "bribe_bips",
                "priority_fee_gwei",
                "bundle_gas_est",
                "dry_run",
                "key_file",
                "executor",
                "operator",
                "sim_url",
                "stream_url",
                "rank_evidence",
                "connectors",
                "fixture_head",
                "stop_file",
            ]
        );
        // Every backrun key's dotted TOML path is the facet path + field.
        for key in SCHEMA.iter().filter(|k| k.section == "strategy.backrun") {
            assert_eq!(
                key.toml_path,
                format!("strategy.backrun.{}", key.field),
                "facet key toml path drift"
            );
            assert!(key.env.starts_with("DEGENBOT_STRATEGY_BACKRUN_"));
        }
    }

    #[test]
    fn backrun_facet_keys_collapse_to_declared_defaults() {
        let b = BotConfig::default().strategy.backrun;
        assert!(!b.bid_mode);
        assert_eq!(b.budget_wei, 0);
        assert_eq!(b.max_bundle_wei, 1_000_000_000_000_000);
        assert_eq!(b.bribe_bips, 9_800);
        assert_eq!(b.priority_fee_gwei, 2);
        assert_eq!(b.bundle_gas_est, 300_000);
        assert!(!b.dry_run);
        assert_eq!(b.key_file, None);
        assert_eq!(b.executor, "0x30b28ed8aa581fbc0191c3b532b0697773070e97");
        assert_eq!(b.operator, None);
        assert_eq!(b.sim_url, None);
        assert_eq!(b.stream_url, "");
        assert!(!b.rank_evidence);
        assert_eq!(b.connectors, 8);
        assert_eq!(b.fixture_head, None);
        assert_eq!(
            b.stop_file,
            std::path::PathBuf::from("/tmp/degenbot-sidecar-STOP")
        );
    }

    #[test]
    fn retired_solve_stance_keys_are_not_declared() {
        // WFF6MM hard cutover: the detached-solve stance key is RETIRED with
        // the in-cycle fallback arm — detached is the only solve arm now.
        // The key must not be declared; a surviving TOML key fails the load
        // with the generic unknown-key error (fail-closed) and a CLI
        // override naming it is rejected (mirror of the fleet.stance
        // retirement; the one-release pointed refusals were themselves
        // removed at d5c450c84 — ADR-012 deletion, not a flag).
        assert!(
            !SCHEMA
                .iter()
                .any(|k| k.toml_path == "solve.detached_solves"),
            "solve.detached_solves must be retired from the schema"
        );
        assert!(
            !SCHEMA.iter().any(|k| k.env == "DEGENBOT_DETACHED_SOLVES"),
            "DEGENBOT_DETACHED_SOLVES must be retired from the schema"
        );
    }
}
