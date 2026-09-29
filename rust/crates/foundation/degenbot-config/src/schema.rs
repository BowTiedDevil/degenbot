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
    /// An operator-keyed `string -> string` table (the generated key type is
    /// `BTreeMap<String, String>`). A per-chain endpoint table holds one entry
    /// per chain, and the operator picks those keys at runtime: no closed
    /// element enum can type the value, so the value stays text and the key
    /// is paired with a [`KeyDecl::env_prefix`] family.
    StrMap,
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
            Self::StrMap => f.write_str("map<string, string>"),
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

/// One declared key's typed value, borrowed from the config that holds it.
///
/// The generated [`BotConfig::value`] reader returns this, so a surface that
/// walks [`SCHEMA`] — the Python driver's resolved verdict, the console's
/// resolved print — reads every key with its declared kind intact and without
/// one hand-written accessor per key.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigValue<'a> {
    /// A flag (`bool`, or an inverted one already read into a flag).
    Bool(bool),
    /// Free-form text.
    Text(std::borrow::Cow<'a, str>),
    /// A filesystem path AS WRITTEN — no `~` or state-home expansion, because
    /// expansion is the cascade's answer and this is the declared key.
    Path(std::borrow::Cow<'a, std::path::Path>),
    /// An unsigned count or amount (a millisecond duration, a `usize`, a
    /// `u64`, a `u128` wei value).
    Uint(u128),
    /// A signed count.
    Int(i64),
    /// A multiplier or ratio.
    Float(f64),
    /// A closed variant, rendered through its `Display` (lowercase).
    Enum(std::borrow::Cow<'a, str>),
    /// An operator-keyed table in key order.
    Map(Vec<(String, String)>),
}

impl ConfigValue<'_> {
    /// The value as text, for a surface that reports values rather than
    /// consuming them (a diagnostic print, a refusal message).
    #[must_use]
    pub fn to_text(&self) -> String {
        match self {
            Self::Bool(value) => value.to_string(),
            Self::Text(value) | Self::Enum(value) => value.to_string(),
            Self::Path(value) => value.display().to_string(),
            Self::Uint(value) => value.to_string(),
            Self::Int(value) => value.to_string(),
            Self::Float(value) => value.to_string(),
            Self::Map(entries) => entries
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join(","),
        }
    }
}

impl std::fmt::Display for ConfigValue<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_text())
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
    /// `DEGENBOT_*` environment variable name (operator continuity). For a
    /// family-shaped key this holds the family's prefix (see
    /// [`Self::env_prefix`]), so error labels and the writer's shadow check
    /// still have exactly one name to print.
    pub env: &'static str,
    /// Family prefix (`PREFIX_`) when the env layer is a SET of names sharing
    /// it rather than one name — a per-chain table sets
    /// `PREFIX_<chain_id>=<value>`, which no single env name can express.
    /// `None` for every single-env key.
    pub env_prefix: Option<&'static str>,
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

/// The chain-sample verification policy shared by the two backrun facets.
///
/// Declared once here because facet keys reference an existing enum type
/// rather than generating one; both `strategy.*_backrun.verify_ticks` keys use
/// it. Values are parsed case-insensitively with `-`/`_` folded, matching the
/// macro-generated enum convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum VerifyTicks {
    /// Sample-verify every admission.
    Strict,
    /// Verify the first admission per pool per process, then memoize.
    #[default]
    Bootstrap,
    /// Operator-declared confidence; no chain sample.
    Off,
}

impl fmt::Display for VerifyTicks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rendered = match self {
            Self::Strict => "strict",
            Self::Bootstrap => "bootstrap",
            Self::Off => "off",
        };
        f.write_str(rendered)
    }
}

impl std::str::FromStr for VerifyTicks {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let want = s.trim().to_ascii_lowercase().replace('-', "_");
        match want.as_str() {
            "strict" => Ok(Self::Strict),
            "bootstrap" => Ok(Self::Bootstrap),
            "off" => Ok(Self::Off),
            _ => Err(format!(
                "invalid VerifyTicks value {s:?} (expected one of: Strict Bootstrap Off)"
            )),
        }
    }
}

/// Declared env names that predate the `DEGENBOT_` prefix and keep their own.
///
/// The verification-retry names were the operator's contract before the prefix
/// existed, and the pure-Rust example reads the same four. Putting the prefix
/// in front of them would silently stop honoring an export an operator has
/// already written, which is the one outcome worse than a non-uniform naming
/// scheme: a name nothing reads. Every other declared key is `DEGENBOT_`, and
/// this list is the whole exception.
pub const UNPREFIXED_ENV_NAMES: &[&str] = &[
    "VERIFICATION_RETRY_MAX_ATTEMPTS",
    "VERIFICATION_RETRY_BASE_DELAY",
    "VERIFICATION_RETRY_MAX_DELAY",
    "VERIFICATION_RETRY_JITTER",
];

// ⚠ ONE DECLARATION SITE PER KEY BELOW. Do NOT add parallel env/TOML const
// lists anywhere in the workspace; extend this list and regenerate the doc
// (`REGEN_CONFIG_DOCS=1 cargo test -p degenbot-config`).
//
// Ordering is stable (defines doc + SCHEMA iteration order).

crate::config_schema! {

    // The operator's deployment, written in the file the loader selected and
    // overridden per entry by the environment (ADR-062 D1/D2/D4). The three
    // node tables are one key per transport with one entry per chain: the
    // chain is the table key, so the env layer is a NAME FAMILY
    // (`env_prefix`) rather than one variable, and each entry keeps the layer
    // that supplied it in `LoadedConfig::entry_provenance`.
    nodes NodesConfig {
        http [opt strmap] = None, env_prefix = "DEGENBOT_RPC_HTTP_CHAINID_", def = "(unset)",
            doc = "Per-chain JSON-RPC HTTP endpoint per chain id: [nodes] http = { 1 = \"https://eth.example/rpc\", 8453 = \"https://base.example/rpc\" } (or [nodes.http] with one `1 = \"...\"` line per chain). The env layer is the name family DEGENBOT_RPC_HTTP_CHAINID_<chain_id>, whose entry overrides the file entry for that chain alone. Every value must be an http:// or https:// URL.";
        ws [opt strmap] = None, env_prefix = "DEGENBOT_RPC_WS_CHAINID_", def = "(unset)",
            doc = "Per-chain subscription (WebSocket) endpoint per chain id, same table and family shape as nodes.http. The env layer is the name family DEGENBOT_RPC_WS_CHAINID_<chain_id>. Every value must be a ws:// or wss:// URL; a subscription consumer takes this transport or nodes.ipc, never nodes.http.";
        ipc [opt strmap] = None, env_prefix = "DEGENBOT_RPC_IPC_CHAINID_", def = "(unset)",
            doc = "Per-chain local IPC endpoint per chain id (a node running beside the bot, reachable over a Unix socket or a Windows named pipe), same table and family shape as nodes.http. The env layer is the name family DEGENBOT_RPC_IPC_CHAINID_<chain_id>. Every value must be an `ipc://` URL or an absolute socket path (a leading `/` on Unix, or a Windows named pipe under the `\\\\.\\pipe\\` namespace, e.g. `ipc://\\\\.\\pipe\\node.ipc`). A relative or `~/` path is refused: it resolves against the process working directory, not the config file's directory, and `~` is never expanded here. An IPC entry can serve requests and subscriptions alike.";
    }

    // The session's chain identity: the chain the bot runs against unless an
    // explicit override outranks it. It is an ordinary typed key, so the file
    // layer and DEGENBOT_DEFAULT_CHAIN_ID are one cascade (ADR-062 D4).
    session SessionConfig {
        chain_id [opt u64] = None, env = "DEGENBOT_DEFAULT_CHAIN_ID", def = "(unset)",
            doc = "Chain id the session runs against (a positive integer; the pre-0.6 top-level default_chain_id key is retired and refused with a pointer here). The endpoint tables are keyed by chain id, so this is the chain whose entry the node resolvers read; unset means no chain was named and a node endpoint cannot be selected.";
    }

    // The stateful core's own file, opened by the core (ADR-052): the default
    // is the XDG state-home path the resolver already implements.
    database DatabaseConfig {
        path [path] = std::path::PathBuf::from(crate::resolvers::DB_PATH_DEFAULT), env = "DEGENBOT_DB_PATH", def = "~/.local/state/degenbot/db/degenbot.db",
            doc = "SQLite database file this process owns. A leading ~ expands against HOME, and the declared default rebases onto an absolute $XDG_STATE_HOME; an explicit file or environment path expands as written. The pre-0.6 [database] `filepath` key is retired and refused as an unknown key.";
    }

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

    // The driver cockpit's incident probes: the per-interval tracemalloc
    // diff thread, the read-only /proc/self RSS sampler, and the faulthandler
    // repeat dumper. Every value is an interval, and zero is the production
    // posture (nothing armed) -- the zero IS the declared default, so a
    // consumer that wants a probe reads the key rather than keeping its own
    // copy of "off".
    diagnostics DiagnosticsConfig {
        tracemalloc_secs [f64] = 0.0, env = "DEGENBOT_TRACEMALLOC_SECS", def = "0.0",
            doc = "Interval in seconds between tracemalloc snapshot-diff dumps to stderr; 0 (or unset) arms nothing. A flat traced-current under a climbing RSS pins the growth outside the Python object graph, so the probe splits a memory diagnosis in half.";
        procmem_secs [f64] = 0.0, env = "DEGENBOT_PROCMEM_SECS", def = "0.0",
            doc = "Interval in seconds between /proc/self RSS/VmHWM CSV sampler rows; 0 (or unset) arms nothing. The sampler is read-only -- no snapshots, no allocator calls -- so it never perturbs the behavior being measured.";
        procmem_csv [path] = std::path::PathBuf::from("logs/procmem.csv"), env = "DEGENBOT_PROCMEM_CSV", def = "logs/procmem.csv",
            doc = "CSV output file for the procmem sampler, one row per procmem_secs interval. The parent directory is created when the probe arms.";
        faulthandler_timeout_secs [f64] = 0.0, env = "DEGENBOT_FAULTHANDLER_TIMEOUT_SECS", def = "0.0",
            doc = "Seconds after which faulthandler dumps every thread's stack; 0 (or unset) arms no watchdog. A dump is the only record of where a wedged process was when it stopped answering.";
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
            doc = "Root directory for durable, process-lifetime-independent bot state (e.g. the backrun arm's gap-quarantine journal). State here OUTLIVES sessions and is deliberately NOT nested under a per-session run directory. The default is the XDG state home (`$XDG_STATE_HOME` when absolute, else `$HOME/.local/state`); a leading `~` expands against HOME.";
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
        verify_retry_max_attempts [usize] = 4, env = "VERIFICATION_RETRY_MAX_ATTEMPTS", def = "4",
            doc = "Attempts for a transient verification RPC failure (per-call transport / provider-init) before it propagates. A genuine on-chain mismatch is never retried. Below 1 the policy is not a policy, so building one refuses rather than retrying nothing.";
        verify_retry_base_delay [f64] = 0.5, env = "VERIFICATION_RETRY_BASE_DELAY", def = "0.5",
            doc = "First backoff wait in seconds for a retried verification call; grows exponentially from here. A negative or non-finite value is refused when the policy is built.";
        verify_retry_max_delay [f64] = 4.0, env = "VERIFICATION_RETRY_MAX_DELAY", def = "4.0",
            doc = "Ceiling in seconds on one verification backoff wait. A value below verify_retry_base_delay is refused when the policy is built, because the cap would sit under the first wait it is meant to bound.";
        verify_retry_jitter [f64] = 0.5, env = "VERIFICATION_RETRY_JITTER", def = "0.5",
            doc = "Upper bound in seconds of the uniform jitter added to one backoff wait, so a recovering node is not hit by a synchronized retry herd. A value outside 0..=1 is refused when the policy is built.";
    }

    simulation SimulationConfig {
        sim_execute_gas [opt u64] = None, env = "DEGENBOT_SIM_EXECUTE_GAS", def = "(unset; EIP-7825 TX_GAS_LIMIT_CAP)",
            doc = "Override the execute() gas limit (decimal u64; garbage/0 falls back at the site while migrating).";
        inject_executor_code [bool] = false, env = "DEGENBOT_INJECT_EXECUTOR_CODE", def = "false",
            doc = "Simulate against executor bytecode injected into the evm overlay instead of a deployed contract. Injection mode also gates live submission off for safety; `1` opts in (dry-run/dev posture).";
        sim_exit_on_fail [bool] = false, env = "DEGENBOT_SIM_EXIT_ON_FAIL", def = "false",
            doc = "Abort the process when a sim fails (the live trap used to capture V3-hop fixtures).";
        probe_fixture [opt path] = None, env = "DEGENBOT_PROBE_FIXTURE", def = "(unset)",
            doc = "Corpus fixture for the offline executor A/B probe (ignore-listed test).";
        probe_ns [string] = String::from("1,2,4,8,16"), env = "DEGENBOT_PROBE_NS", def = "1,2,4,8,16",
            doc = "Comma-separated thread-count arms for the offline executor A/B probe.";
        probe_passes [usize] = 3, env = "DEGENBOT_PROBE_PASSES", def = "3",
            doc = "Passes per arm for the offline executor A/B probe.";
        pipeline_concurrency [usize] = 8, env = "DEGENBOT_SIM_PIPELINE_CONCURRENCY", def = "8",
            doc = "Sims in flight per block before the pipeline's submitter is held back; 1 reproduces the serial reference (one sim, FIFO submit) for an offline soak. Clamped to >= 1 at the use site.";
        exit_ignore_buckets [string] = String::new(), env = "DEGENBOT_SIM_EXIT_IGNORE_BUCKETS", def = "(empty)",
            doc = "Comma-separated failure buckets the sim-failure tripwire does not count (e.g. `empty,short`). The trap itself is simulation.sim_exit_on_fail; this only narrows an ARMED trap, and there is no default ignore set -- an unlisted bucket stops the bot.";
    }

    // 4IOEVT: discovery delivery batching. The startup discovery sweep's
    // async wrapper delivers paths in batches (one event-loop hop per batch)
    // instead of one hop per path; this typed key makes the batch size
    // operator-settable without a code change.
    pathfinding PathfindingConfig {
        discovery_batch_size [usize] = 1000, env = "DEGENBOT_DISCOVERY_BATCH_SIZE", def = "1000",
            doc = "Discovery-sweep delivery batch size (paths per async batch): the worker thread collects this many paths before the async consumer yields them and gives the event loop one turn. A value <= 1 falls back to the legacy per-path delivery.";
        max_registered_paths [usize] = 100_000, env = "DEGENBOT_MAX_PATHS", def = "100000",
            doc = "Ceiling on total registered arbitrage paths, applied before discovery runs so a discovery-heavy skip-fest stops instead of registering millions. 0 means uncapped.";
        reg_progress_secs [f64] = 30.0, env = "DEGENBOT_REG_PROGRESS_SECS", def = "30.0",
            doc = "Seconds between registration-progress summaries, which fire on the interval even when the path count never crosses a discovery_batch_size boundary. 0 emits on every update.";
    }

    // The driver's dispatch policy: how a decided arm becomes a transaction.
    // These are DRIVER-side stances, deliberately not folded into the core's
    // own policy surfaces -- a driver-side copy of a core-owned threshold is a
    // value the core would ignore.
    dispatch DispatchConfig {
        erc6909_profit [bool] = false, env = "DEGENBOT_ERC6909_PROFIT", def = "false",
            doc = "Capture profit through an ERC-6909 vault claim instead of a plain transfer; `1` opts in. The two capture paths need different executor bytecode, so this selects the whole post-profit seam.";
        min_profit_margin_bps [u64] = 0, env = "DEGENBOT_MIN_PROFIT_MARGIN_BPS", def = "0",
            doc = "Driver-side profit floor in basis points (1/100 of a percent) applied at the simulation seam before a candidate is dispatched. A floor is a magnitude, so the key is unsigned end to end: a negative value is refused by the layer that supplied it, not clamped into a silent second default. This is NOT solve.min_profit_wei, which is the core's own floor: the two arms of the simulation seam are measured against their own floors, so naming one does not size the other.";
        contracts_dir [opt path] = None, env = "DEGENBOT_CONTRACTS_DIR", def = "(unset)",
            doc = "Directory holding the executor runtime bytecode file the sim injects; unset falls through to the source-layout candidate the driver computes, and a wheel install must set it (or pass the file path explicitly).";
    }

    // The per-arm facet namespaces: the typed config home each strategy's own
    // knobs land in. Activation is per-facet and explicit — a strategy runs
    // when its facet sets `active`, and an activated facet must carry a
    // non-empty `endpoints` set (the readiness validation refuses an
    // unsettled activation). The pending-transaction reaction is shared by two
    // per-ecosystem strategies, each with its own facet.
    strategy StrategyConfig {
        settlement StrategySettlementConfig {
            active [bool] = false, env = "DEGENBOT_STRATEGY_SETTLEMENT_ACTIVE", def = "false",
                doc = "Activate the settled-block settlement arm in this process; inactive leaves the facet dormant.";
            endpoints [opt string] = None, env = "DEGENBOT_STRATEGY_SETTLEMENT_ENDPOINTS", def = "(unset; required when settlement is active)",
                doc = "Comma-separated broadcast endpoints for settlement submissions. Restricted to the pinned revert-protecting relay allowlist (docs/autonomous-user-journey/RELAYS_AND_GUARDRAILS.md).";
        }
        mevblocker_backrun StrategyMevblockerBackrunConfig {
            active [bool] = false, env = "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_ACTIVE", def = "false",
                doc = "Activate this per-ecosystem backrun arm in this process; inactive leaves the facet dormant.";
            bid_mode [bool] = false, env = "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_BID_MODE", def = "false",
                doc = "Explicit bid-mode flag; off is observe-only. Bid mode also requires a non-zero budget_wei (the legality gate reads both).";
            budget_wei [u128] = 0, env = "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_BUDGET_WEI", def = "0",
                doc = "Cumulative bid budget cap in wei (decimal text; TOML: quoted string). Zero makes bid mode illegal; the spent accumulator tracks the wallet's gas burn.";
            max_bundle_wei [u128] = 1_000_000_000_000_000u128, env = "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_MAX_BUNDLE_WEI", def = "1000000000000000",
                doc = "Hard per-submission cap in wei (decimal text; TOML: quoted string); a decided bid is clamped to it.";
            bribe_bips [u64] = 9800, env = "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_BRIBE_BIPS", def = "9800",
                doc = "The builder's bribe share of true profit in bips of 10_000 — the competitiveness ceiling (clamped at the site to <= 10_000).";
            priority_fee_gwei [u64] = 2, env = "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_PRIORITY_FEE_GWEI", def = "2",
                doc = "The operator's priority fee in gwei, converted to wei when pricing the wallet's gas burn.";
            bundle_gas_est [u64] = 300_000, env = "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_BUNDLE_GAS_EST", def = "300000",
                doc = "Composed-bundle gas estimate priced into the net-of-gas bid gate until an exact in-scratch measurement replaces it.";
            gas_floor_wei [u64] = 1, env = "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_GAS_FLOOR_WEI", def = "1",
                doc = "The envelope gate's profit floor in wei: a declared chain whose solver bound tops out below this is skipped without a simulation. Default 1 wei = solve everything and let the net-of-gas bid gate decide; raise to pre-filter thin cycles.";
            verify_ticks [enum VerifyTicks Strict Bootstrap Off] = VerifyTicks::Bootstrap, env = "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_VERIFY_TICKS", def = "bootstrap",
                doc = "Chain-sample verification policy for ingress V3 tick-map admission: strict verifies every admission, bootstrap verifies the first admission per pool per process then memoizes, off declares operator confidence and emits a loud boot entry. Integrity checks are unconditional under off.";
            dry_run [bool] = false, env = "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_DRY_RUN", def = "false",
                doc = "Sign-nothing dispatch: every candidate skips as DryRun. Plain bool words are accepted.";
            key_file [opt path] = None, env = "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_KEY_FILE", def = "(unset)",
                doc = "Hex secp256k1 operator key file. Unset means no signing material is loaded (observe-only); the key never leaves TxSigner.";
            executor [string] = String::from("0x30b28ed8aa581fbc0191c3b532b0697773070e97"), env = "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_EXECUTOR", def = "0x30b28ed8aa581fbc0191c3b532b0697773070e97",
                doc = "Executor contract address the composed backrun calls; parsed and validated at the driver boot.";
            operator [opt string] = None, env = "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_OPERATOR", def = "(unset; falls back to EXECUTOR_OWNER_ADDRESS)",
                doc = "Executor owner / sim caller address. Unset falls back to the legacy EXECUTOR_OWNER_ADDRESS env name, then the built-in default; parsed at the driver boot.";
            sim_url [opt string] = None, env = "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_SIM_URL", def = "(unset; the chain node)",
                doc = "Bundle-sim endpoint serving eth_callMany. Unset reuses the chain node; MEVBlocker's /fast tier answers method-missing, so the node is the fallback.";
            rank_evidence [bool] = false, env = "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_RANK_EVIDENCE", def = "false",
                doc = "Run the live deep-pair ranking sanity probe before any frame trusts the connector-depth truncation (diagnostic).";
            connectors [usize] = 8, env = "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_CONNECTORS", def = "8",
                doc = "Discovery fan-out cap: the maximum number of WETH-entry cycles admitted per frame (strongest by touched-pool count).";
            cycle_max_hops [usize] = 4, env = "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_CYCLE_MAX_HOPS", def = "4",
                doc = "Hop-depth cap per admitted cycle: the WETH stake pin plus up to cycle_max_hops - 1 connectors (cycle length in pools). Minimum 2 (one pin + one connector); a lower value fails the load. Distinct from `connectors`, which caps the cycles admitted per frame.";
            fixture_head [opt u64] = None, env = "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_FIXTURE_HEAD", def = "(unset)",
                doc = "Offline dry-run's pinned head block: replays captured frames against the chain view they were pending in instead of the live tip. Unset falls back to the fetched head.";
            stop_file [path] = std::path::PathBuf::from("/tmp/degenbot-sidecar-STOP"), env = "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_STOP_FILE", def = "/tmp/degenbot-sidecar-STOP",
                doc = "Kill-switch path: while the file exists the decision layer drops every candidate and the loop halts.";
            endpoints [opt string] = None, env = "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_ENDPOINTS", def = "(unset; required when mevblocker_backrun is active)",
                doc = "MEVBlocker searcher WebSocket for the private bundle broadcast (single URL).";
            mevblocker_url [opt string] = None, env = "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_MEVBLOCKER_URL", def = "(unset: bundle-only bid)",
                doc = "Private-broadcast RPC for the raw relay fan-out. Set arms the private-broadcast arm: the signed backrun goes raw to this endpoint first, then to the chain node as the public fallback relay.";
        }
        txpool_backrun StrategyTxpoolBackrunConfig {
            active [bool] = false, env = "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_ACTIVE", def = "false",
                doc = "Activate this per-ecosystem backrun arm in this process; inactive leaves the facet dormant.";
            bid_mode [bool] = false, env = "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_BID_MODE", def = "false",
                doc = "Explicit bid-mode flag; off is observe-only. Bid mode also requires a non-zero budget_wei (the legality gate reads both).";
            budget_wei [u128] = 0, env = "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_BUDGET_WEI", def = "0",
                doc = "Cumulative bid budget cap in wei (decimal text; TOML: quoted string). Zero makes bid mode illegal; the spent accumulator tracks the wallet's gas burn.";
            max_bundle_wei [u128] = 1_000_000_000_000_000u128, env = "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_MAX_BUNDLE_WEI", def = "1000000000000000",
                doc = "Hard per-submission cap in wei (decimal text; TOML: quoted string); a decided bid is clamped to it.";
            bribe_bips [u64] = 9800, env = "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_BRIBE_BIPS", def = "9800",
                doc = "The builder's bribe share of true profit in bips of 10_000 — the competitiveness ceiling (clamped at the site to <= 10_000).";
            priority_fee_gwei [u64] = 2, env = "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_PRIORITY_FEE_GWEI", def = "2",
                doc = "The operator's priority fee in gwei, converted to wei when pricing the wallet's gas burn.";
            bundle_gas_est [u64] = 300_000, env = "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_BUNDLE_GAS_EST", def = "300000",
                doc = "Composed-bundle gas estimate priced into the net-of-gas bid gate until an exact in-scratch measurement replaces it.";
            gas_floor_wei [u64] = 1, env = "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_GAS_FLOOR_WEI", def = "1",
                doc = "The envelope gate's profit floor in wei: a declared chain whose solver bound tops out below this is skipped without a simulation. Default 1 wei = solve everything and let the net-of-gas bid gate decide; raise to pre-filter thin cycles.";
            verify_ticks [enum VerifyTicks Strict Bootstrap Off] = VerifyTicks::Bootstrap, env = "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_VERIFY_TICKS", def = "bootstrap",
                doc = "Chain-sample verification policy for ingress V3 tick-map admission: strict verifies every admission, bootstrap verifies the first admission per pool per process then memoizes, off declares operator confidence and emits a loud boot entry. Integrity checks are unconditional under off.";
            dry_run [bool] = false, env = "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_DRY_RUN", def = "false",
                doc = "Sign-nothing dispatch: every candidate skips as DryRun. Plain bool words are accepted.";
            key_file [opt path] = None, env = "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_KEY_FILE", def = "(unset)",
                doc = "Hex secp256k1 operator key file. Unset means no signing material is loaded (observe-only); the key never leaves TxSigner.";
            executor [string] = String::from("0x30b28ed8aa581fbc0191c3b532b0697773070e97"), env = "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_EXECUTOR", def = "0x30b28ed8aa581fbc0191c3b532b0697773070e97",
                doc = "Executor contract address the composed backrun calls; parsed and validated at the driver boot.";
            operator [opt string] = None, env = "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_OPERATOR", def = "(unset; falls back to EXECUTOR_OWNER_ADDRESS)",
                doc = "Executor owner / sim caller address. Unset falls back to the legacy EXECUTOR_OWNER_ADDRESS env name, then the built-in default; parsed at the driver boot.";
            sim_url [opt string] = None, env = "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_SIM_URL", def = "(unset; the chain node)",
                doc = "Bundle-sim endpoint serving eth_callMany. Unset reuses the chain node; MEVBlocker's /fast tier answers method-missing, so the node is the fallback.";
            rank_evidence [bool] = false, env = "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_RANK_EVIDENCE", def = "false",
                doc = "Run the live deep-pair ranking sanity probe before any frame trusts the connector-depth truncation (diagnostic).";
            connectors [usize] = 8, env = "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_CONNECTORS", def = "8",
                doc = "Discovery fan-out cap: the maximum number of WETH-entry cycles admitted per frame (strongest by touched-pool count).";
            cycle_max_hops [usize] = 4, env = "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_CYCLE_MAX_HOPS", def = "4",
                doc = "Hop-depth cap per admitted cycle: the WETH stake pin plus up to cycle_max_hops - 1 connectors (cycle length in pools). Minimum 2 (one pin + one connector); a lower value fails the load. Distinct from `connectors`, which caps the cycles admitted per frame.";
            fixture_head [opt u64] = None, env = "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_FIXTURE_HEAD", def = "(unset)",
                doc = "Offline dry-run's pinned head block: replays captured frames against the chain view they were pending in instead of the live tip. Unset falls back to the fetched head.";
            stop_file [path] = std::path::PathBuf::from("/tmp/degenbot-sidecar-STOP"), env = "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_STOP_FILE", def = "/tmp/degenbot-sidecar-STOP",
                doc = "Kill-switch path: while the file exists the decision layer drops every candidate and the loop halts.";
            endpoints [opt string] = None, env = "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_ENDPOINTS", def = "(unset; required when txpool_backrun is active)",
                doc = "Comma-separated public relay fan-out endpoints for raw backrun broadcast, with the read provider as fallback.";
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

impl BotConfig {
    /// Semantic validation the per-key scalar parsers cannot express: a value
    /// that parses into its declared kind but cannot serve its domain fails
    /// the load with the remedy rather than a silent clamp.
    ///
    /// # Errors
    ///
    /// Returns every problem found, mirroring the loader's aggregate
    /// [`ConfigError`](crate::error::ConfigError).
    pub fn validate(&self) -> Result<(), crate::error::ConfigError> {
        let mut problems: Vec<String> = Vec::new();
        validate_node_table(
            "nodes.http",
            crate::resolvers::RPC_HTTP_ENV_PREFIX,
            NodeTransport::Http,
            self.nodes.http.as_ref(),
            &mut problems,
        );
        validate_node_table(
            "nodes.ws",
            crate::resolvers::RPC_WS_ENV_PREFIX,
            NodeTransport::Ws,
            self.nodes.ws.as_ref(),
            &mut problems,
        );
        validate_node_table(
            "nodes.ipc",
            crate::resolvers::RPC_IPC_ENV_PREFIX,
            NodeTransport::Ipc,
            self.nodes.ipc.as_ref(),
            &mut problems,
        );
        if self.session.chain_id == Some(0) {
            problems.push(format!(
                "session.chain_id ({}) is 0; a chain id is a positive integer \u{2014} set it to 1 or more",
                crate::resolvers::DEFAULT_CHAIN_ID_ENV
            ));
        }
        for (path, env, hops) in [
            (
                "strategy.mevblocker_backrun.cycle_max_hops",
                "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_CYCLE_MAX_HOPS",
                self.strategy.mevblocker_backrun.cycle_max_hops,
            ),
            (
                "strategy.txpool_backrun.cycle_max_hops",
                "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_CYCLE_MAX_HOPS",
                self.strategy.txpool_backrun.cycle_max_hops,
            ),
        ] {
            if hops < 2 {
                problems.push(format!(
                    "{path} ({env}) is {hops}; the minimum is 2 (the WETH-entry pin plus \
                     at least one connector) — set it to 2 or more"
                ));
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(crate::error::ConfigError::of(problems))
        }
    }
}

/// Which transport a `[nodes.*]` key declares, and therefore what one of its
/// entry values must be able to serve. The transport is a property of the KEY:
/// a value's capability decides which key may hold it (ADR-062 D3), so the
/// table it was typed into has to accept the transport or the operator's table
/// placement is refused instead of silently mis-served. The resolver reads the
/// same three transports through this one enumeration, so the capability a
/// value must have and the key it lands in cannot drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeTransport {
    /// `nodes.http`: request-only.
    Http,
    /// `nodes.ws`: subscriptions.
    Ws,
    /// `nodes.ipc`: requests and subscriptions over a local socket.
    Ipc,
}

impl NodeTransport {
    /// Every transport, in declaration order — the order a surface enumerates
    /// the endpoint tables in.
    pub const ALL: [Self; 3] = [Self::Http, Self::Ws, Self::Ipc];

    /// The transport's short name, as the operator spelled it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Ws => "ws",
            Self::Ipc => "ipc",
        }
    }

    /// The dotted TOML path of the declared key this transport fills.
    #[must_use]
    pub const fn key_path(self) -> &'static str {
        match self {
            Self::Http => "nodes.http",
            Self::Ws => "nodes.ws",
            Self::Ipc => "nodes.ipc",
        }
    }

    /// The `PREFIX_` of the per-chain env name family that overrides one entry
    /// of this transport's table.
    #[must_use]
    pub const fn env_prefix(self) -> &'static str {
        match self {
            Self::Http => crate::resolvers::RPC_HTTP_ENV_PREFIX,
            Self::Ws => crate::resolvers::RPC_WS_ENV_PREFIX,
            Self::Ipc => crate::resolvers::RPC_IPC_ENV_PREFIX,
        }
    }

    /// What a valid entry value looks like, spelled for the refusal message.
    #[must_use]
    pub const fn expected(self) -> &'static str {
        match self {
            Self::Http => "an http:// or https:// URL",
            Self::Ws => "a ws:// or wss:// URL",
            Self::Ipc => "an ipc:// URL or an absolute socket path (a leading `/`, or a Windows named pipe under `\\\\.\\pipe\\`)",
        }
    }

    /// The transport that can serve `value`, or `None` when no transport
    /// accepts it. The value's own shape IS its transport (ADR-062 D6), so a
    /// single flag can carry every transport without disagreeing with itself,
    /// and a scheme we have not thought of yet is refused rather than guessed.
    #[must_use]
    pub fn classify(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|transport| transport.accepts(value))
    }

    /// Whether `value` can serve this transport.
    fn accepts(self, value: &str) -> bool {
        match self {
            Self::Http => value.starts_with("http://") || value.starts_with("https://"),
            Self::Ws => value.starts_with("ws://") || value.starts_with("wss://"),
            Self::Ipc => value.starts_with("ipc://") || is_socket_path(value),
        }
    }
}

/// A local socket path an IPC entry may name: an absolute path or a Windows
/// named pipe. These are the path forms the transport's own IPC predicate
/// dials. A relative path and a `~/` path are NOT ones — they resolve
/// against the process working directory, not the config file's directory,
/// and `~` is never expanded by the loader. A drive path is not one either:
/// it names no named pipe. A bare word is not one — `localhost:8545` is
/// a typo that would otherwise be accepted as a relative path and fail at
/// the socket.
fn is_socket_path(value: &str) -> bool {
    const WINDOWS_NAMED_PIPE: &str = "\\\\.";
    value.starts_with('/') || value.starts_with(WINDOWS_NAMED_PIPE)
}

/// The Windows drive-letter form (a drive letter, a colon, then a slash),
/// which names no socket the transport dials. It is recognized only to give
/// that operator the named-pipe remedy in the refusal.
fn is_drive_path(value: &str) -> bool {
    value
        .as_bytes()
        .get(1)
        .is_some_and(|c| *c == b':' && matches!(value.as_bytes().get(2), Some(b'\\' | b'/')))
}

/// Why an `ipc` entry was refused when its shape names a path the transport
/// does not dial. `None` means the value is not one of those path shapes, so
/// the generic transport-mismatch message is enough.
fn ipc_path_hint(value: &str) -> Option<&'static str> {
    if value.starts_with("~/") || value.starts_with("./") || value.starts_with("../") {
        Some(
            "a relative or `~` path resolves against the process working directory, not the config file's directory, and `~` is not expanded here — name an absolute socket path (a leading `/`) or an `ipc://` URL",
        )
    } else if is_drive_path(value) {
        Some(
            "a Windows drive path is not a named pipe — use the pipe namespace (`ipc://\\\\.\\pipe\\node.ipc`) or an absolute path",
        )
    } else {
        None
    }
}

/// The refusal for one `[nodes.*]` entry that cannot serve its key's
/// transport. Http and Ws get the transport-mismatch message; an `ipc` entry
/// that is a path shape the transport does not dial gets the reason instead,
/// because the operator's mental model (a path relative to the config file,
/// or a `~` that expands) is the actual error.
fn entry_refusal(key: &str, transport: NodeTransport, chain: &str, value: &str) -> String {
    // The value is echoed so the operator can see what was refused, but a
    // credential in it must not survive into a diagnostic (ADR-062 D12).
    let prefix = format!(
        "{key} entry for chain {chain} is {:?}, which is not {}",
        crate::redact_uri(value),
        transport.expected()
    );
    match (transport, ipc_path_hint(value)) {
        (NodeTransport::Ipc, Some(hint)) => format!("{prefix} \u{2014} {hint}"),
        _ => format!(
            "{prefix} \u{2014} put the endpoint in the [nodes.*] table that declares its transport"
        ),
    }
}

/// Validate one `[nodes.*]` table: the table key is a chain id, the entry is
/// not blank, and the value can serve the key's transport. Each problem names
/// the offending entry and the fix, because the table is operator-chosen and a
/// wrong entry is otherwise found at connection time, in another process.
fn validate_node_table(
    key: &str,
    env_prefix: &str,
    transport: NodeTransport,
    table: Option<&std::collections::BTreeMap<String, String>>,
    problems: &mut Vec<String>,
) {
    let Some(table) = table else {
        return;
    };
    for (chain, value) in table {
        if chain.parse::<u64>().is_err() {
            problems.push(format!(
                "{key} ({env_prefix}<chain_id>) is keyed by {chain:?}, which is not a \
                 chain id \u{2014} key the table by the decimal chain id (e.g. 1 = \"{}\")",
                transport.expected()
            ));
        }
        if value.trim().is_empty() {
            problems.push(format!(
                "{key} entry for chain {chain} is empty \u{2014} a blank value means \
                 \"this layer supplied nothing\" in the environment, so an empty entry \
                 is a mistake: give chain {chain} {} or delete the entry",
                transport.expected()
            ));
            continue;
        }
        if !transport.accepts(value) {
            problems.push(entry_refusal(key, transport, chain, value));
        }
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::expect_used, reason = "test assertions fail loudly")]

    use super::*;

    /// The generated reader answers exactly the keys the schema declares, in
    /// both directions: a key with no reader arm is unreachable by every
    /// surface that walks `SCHEMA`, and a reader arm for a key nobody
    /// declared is a value nothing can set. Either defect is a silent hole
    /// in the one declaration site, so the census pins the set equality
    /// rather than counting arms.
    #[test]
    fn declared_keys_and_readable_keys_are_the_same_set() {
        let declared: std::collections::BTreeSet<(&str, &str)> =
            SCHEMA.iter().map(|k| (k.section, k.field)).collect();
        let readable: std::collections::BTreeSet<(&str, &str)> =
            READABLE_KEYS.iter().copied().collect();
        assert_eq!(
            readable, declared,
            "the generated reader and the declared schema must cover the same keys"
        );
    }

    /// A value read back through the schema keeps the kind the key declared,
    /// so a consumer of the projection cannot be handed a number where a path
    /// belongs. One key per kind family.
    #[test]
    fn a_value_reads_back_with_the_kind_its_key_declared() {
        let mut config = BotConfig::default();
        let set = |cfg: &mut BotConfig, section: &str, field: &str, raw: &str| {
            cfg.assign(section, field, raw)
                .expect("a declared key accepts a raw value its kind names");
        };
        set(&mut config, "telemetry", "otel", "0");
        set(&mut config, "pump", "pump_debounce_ms", "75");
        set(&mut config, "telemetry", "metrics_addr", "127.0.0.1:1");
        set(&mut config, "database", "path", "/var/lib/x.db");
        set(&mut config, "pathfinding", "discovery_batch_size", "7");
        set(&mut config, "verify", "verify_spotcheck_permyriad", "3");
        set(&mut config, "allocator", "mimalloc_purge_delay_ms", "-2");
        set(&mut config, "allocator", "mimalloc_purge_delay_mult", "2.5");
        set(
            &mut config,
            "solve",
            "min_profit_wei",
            "1000000000000000000",
        );
        set(&mut config, "pump", "quiesce_mode", "fixed");
        set(&mut config, "telemetry", "diag", "sim=debug,solver=trace");
        set(&mut config, "nodes", "http", "1=https://eth.example");

        let value = |section: &str, field: &str| config.value(section, field);
        assert_eq!(value("telemetry", "otel"), Some(ConfigValue::Bool(false)));
        assert_eq!(
            value("pump", "pump_debounce_ms"),
            Some(ConfigValue::Uint(75))
        );
        assert_eq!(
            value("telemetry", "metrics_addr"),
            Some(ConfigValue::Text("127.0.0.1:1".into()))
        );
        assert_eq!(
            value("database", "path"),
            Some(ConfigValue::Path(
                std::path::Path::new("/var/lib/x.db").into()
            ))
        );
        assert_eq!(
            value("pathfinding", "discovery_batch_size"),
            Some(ConfigValue::Uint(7))
        );
        assert_eq!(
            value("verify", "verify_spotcheck_permyriad"),
            Some(ConfigValue::Uint(3))
        );
        assert_eq!(
            value("allocator", "mimalloc_purge_delay_ms"),
            Some(ConfigValue::Int(-2))
        );
        assert_eq!(
            value("allocator", "mimalloc_purge_delay_mult"),
            Some(ConfigValue::Float(2.5))
        );
        assert_eq!(
            value("solve", "min_profit_wei"),
            Some(ConfigValue::Uint(1_000_000_000_000_000_000))
        );
        assert_eq!(
            value("pump", "quiesce_mode"),
            Some(ConfigValue::Enum("fixed".into()))
        );
        assert_eq!(
            value("telemetry", "diag"),
            Some(ConfigValue::Map(vec![
                ("sim".to_string(), "debug".to_string()),
                ("solver".to_string(), "trace".to_string()),
            ]))
        );
        assert_eq!(
            value("nodes", "http"),
            Some(ConfigValue::Map(vec![(
                "1".to_string(),
                "https://eth.example".to_string()
            )]))
        );
    }

    /// An unset optional key reads as absent rather than as its declared
    /// default: "the operator said nothing" and "the operator said the
    /// default" are different facts, and a consumer that cannot tell them
    /// apart invents provenance.
    #[test]
    fn an_unset_optional_key_reads_as_absent() {
        let config = BotConfig::default();
        assert_eq!(config.value("session", "chain_id"), None);
        assert_eq!(config.value("nodes", "ipc"), None);
        assert_eq!(
            config.value("session", "no_such_key"),
            None,
            "an undeclared key is not a value"
        );
    }

    #[test]
    fn schema_paths_are_wellformed_and_unique() {
        let mut seen_toml = std::collections::BTreeSet::new();
        let mut seen_env = std::collections::BTreeSet::new();
        for k in SCHEMA {
            assert!(!k.section.is_empty() && !k.field.is_empty());
            assert!(
                k.env.starts_with("DEGENBOT_") || UNPREFIXED_ENV_NAMES.contains(&k.env),
                "env must be DEGENBOT_-prefixed, or be named in UNPREFIXED_ENV_NAMES: {}",
                k.env
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

    /// The declaration macro needs a literal for the env name, so these keys
    /// are checked against the resolver constants that own the spelling: one
    /// constant, one meaning, and a rename cannot drift the two apart.
    #[test]
    fn the_node_and_session_keys_reuse_the_resolver_spellings() {
        use crate::resolvers::{
            DB_PATH_DEFAULT, DB_PATH_ENV, DEFAULT_CHAIN_ID_ENV, RPC_HTTP_ENV_PREFIX,
            RPC_IPC_ENV_PREFIX, RPC_WS_ENV_PREFIX,
        };
        for (toml_path, env, family) in [
            ("nodes.http", RPC_HTTP_ENV_PREFIX, Some(RPC_HTTP_ENV_PREFIX)),
            ("nodes.ws", RPC_WS_ENV_PREFIX, Some(RPC_WS_ENV_PREFIX)),
            ("nodes.ipc", RPC_IPC_ENV_PREFIX, Some(RPC_IPC_ENV_PREFIX)),
            ("session.chain_id", DEFAULT_CHAIN_ID_ENV, None),
            ("database.path", DB_PATH_ENV, None),
        ] {
            let key = SCHEMA.iter().find(|k| k.toml_path == toml_path);
            assert!(key.is_some(), "{toml_path} must be declared exactly once");
            assert_eq!(key.map(|k| k.env), Some(env), "{toml_path} env drift");
            assert_eq!(
                key.and_then(|k| k.env_prefix),
                family,
                "{toml_path} env-family drift"
            );
        }
        // The declared default IS the resolver's default, spelling included,
        // so the file layer and the resolver's built-in fallback cannot name
        // two different database files.
        let database = SCHEMA
            .iter()
            .find(|k| k.toml_path == "database.path")
            .map(|k| k.default_repr);
        assert_eq!(database, Some(DB_PATH_DEFAULT));
        assert_eq!(
            BotConfig::default().database.path,
            std::path::PathBuf::from(DB_PATH_DEFAULT)
        );
        // Unset by default: no chain is named and no endpoint is invented.
        assert_eq!(BotConfig::default().session.chain_id, None);
        assert!(BotConfig::default().nodes.http.is_none());
        assert!(BotConfig::default().nodes.ws.is_none());
        assert!(BotConfig::default().nodes.ipc.is_none());
    }

    #[test]
    fn a_node_value_classifies_into_the_transport_it_can_serve() {
        for (value, transport) in [
            ("http://127.0.0.1:8545", NodeTransport::Http),
            ("https://eth.example.com", NodeTransport::Http),
            ("ws://127.0.0.1:8546", NodeTransport::Ws),
            ("wss://eth.example.com/ws", NodeTransport::Ws),
            ("ipc:///tmp/anvil.ipc", NodeTransport::Ipc),
            ("/tmp/anvil.ipc", NodeTransport::Ipc),
            ("\\\\.\\pipe\\geth.ipc", NodeTransport::Ipc),
        ] {
            assert_eq!(
                NodeTransport::classify(value),
                Some(transport),
                "{value:?} classifies into {} over {}",
                transport.key_path(),
                transport.expected()
            );
        }
    }

    #[test]
    fn a_value_no_transport_serves_classifies_to_nothing() {
        for value in [
            "ftp://x",
            "localhost:8545",
            "",
            "eth.example.com",
            "~/node.ipc",
            "./node.ipc",
            "../node.ipc",
            "C:\\node.ipc",
        ] {
            assert_eq!(
                NodeTransport::classify(value),
                None,
                "{value:?} names no transport"
            );
        }
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "test fixtures fail loudly on an unconstructible prerequisite"
    )]
    fn ipc_entries_accept_ipc_urls_and_absolute_socket_paths() {
        for value in [
            "ipc:///tmp/anvil.ipc",
            "/tmp/anvil.ipc",
            "\\\\.\\pipe\\geth.ipc",
        ] {
            let mut config = BotConfig::default();
            config.nodes.ipc = Some(std::collections::BTreeMap::from([(
                "1".to_string(),
                value.to_string(),
            )]));
            if let Err(error) = config.validate() {
                panic!("{value:?} must be an accepted ipc entry: {error}");
            }
        }
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "test fixtures fail loudly on an unconstructible prerequisite"
    )]
    fn relative_tilde_and_drive_ipc_entries_are_refused_with_the_reason() {
        for value in ["~/node.ipc", "./node.ipc", "../node.ipc"] {
            let mut config = BotConfig::default();
            config.nodes.ipc = Some(std::collections::BTreeMap::from([(
                "1".to_string(),
                value.to_string(),
            )]));
            let Err(error) = config.validate() else {
                panic!("a relative or ~ ipc path must be refused: {value:?}");
            };
            let message = error.to_string();
            assert!(
                message.contains("process working directory") && message.contains("ipc://"),
                "the refusal must explain the cwd resolution and name the ipc:// escape: {message}"
            );
        }

        let mut config = BotConfig::default();
        config.nodes.ipc = Some(std::collections::BTreeMap::from([(
            "1".to_string(),
            "C:\\node.ipc".to_string(),
        )]));
        let Err(error) = config.validate() else {
            panic!("a drive path must be refused");
        };
        let message = error.to_string();
        assert!(
            message.contains("named pipe") && message.contains("ipc://"),
            "the drive-path refusal must point at the named-pipe namespace: {message}"
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
    fn strategy_arm_selector_is_retired() {
        // A single-arm selector cannot express a two-strategy host; the
        // per-facet `active` keys own activation now, so every retired
        // selector spelling must stay undeclared (fail-closed at the load).
        assert!(
            !SCHEMA.iter().any(|k| k.toml_path == "strategy.name"
                || k.env == std::concat!("DEGENBOT_", "STRATEGY_NAME")),
            "the retired single-arm selector must stay undeclared"
        );
    }

    #[test]
    fn strategy_activation_keys_parse_and_collapse_to_defaults() {
        let mut config = BotConfig::default();
        let defaults = &config.strategy;
        assert!(!defaults.settlement.active);
        assert_eq!(defaults.settlement.endpoints, None);
        assert!(!defaults.mevblocker_backrun.active);
        assert_eq!(defaults.mevblocker_backrun.endpoints, None);
        assert!(!defaults.txpool_backrun.active);
        assert_eq!(defaults.txpool_backrun.endpoints, None);

        assert!(config
            .assign("strategy.settlement", "active", "true")
            .is_ok());
        assert!(config
            .assign(
                "strategy.settlement",
                "endpoints",
                "https://rpc.flashbots.net?hint=hash,https://rpc.mevblocker.io/noreverts"
            )
            .is_ok());
        assert!(config
            .assign("strategy.mevblocker_backrun", "active", "1")
            .is_ok());
        assert!(config
            .assign(
                "strategy.mevblocker_backrun",
                "endpoints",
                "wss://searchers.example"
            )
            .is_ok());
        assert!(config
            .assign("strategy.txpool_backrun", "active", "1")
            .is_ok());
        assert!(config
            .assign(
                "strategy.txpool_backrun",
                "endpoints",
                "https://rpc.beaverbuild.org,https://rpc.flashbots.net"
            )
            .is_ok());
        assert!(
            config
                .assign("strategy.settlement", "active", "maybe")
                .is_err(),
            "junk activation fails the assign, not a later gate"
        );

        let c = &config.strategy;
        assert!(c.settlement.active);
        assert_eq!(
            c.settlement.endpoints.as_deref(),
            Some("https://rpc.flashbots.net?hint=hash,https://rpc.mevblocker.io/noreverts")
        );

        assert!(c.mevblocker_backrun.active);
        assert_eq!(
            c.mevblocker_backrun.endpoints.as_deref(),
            Some("wss://searchers.example")
        );
        assert!(c.txpool_backrun.active);
        assert_eq!(
            c.txpool_backrun.endpoints.as_deref(),
            Some("https://rpc.beaverbuild.org,https://rpc.flashbots.net")
        );
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one assertion block per declared facet key"
    )]
    fn strategy_facets_are_declared_as_typed_sections() {
        // The per-arm facet namespaces exist as typed fields with dotted TOML
        // section paths. Settlement plus one facet per backrun ecosystem.
        let config = BotConfig::default();
        assert_eq!(
            config.strategy.settlement,
            StrategySettlementConfig::default()
        );
        assert_eq!(
            config.strategy.mevblocker_backrun,
            StrategyMevblockerBackrunConfig::default()
        );
        assert_eq!(
            config.strategy.txpool_backrun,
            StrategyTxpoolBackrunConfig::default()
        );
        assert!(SECTION_PATHS.contains(&"strategy.settlement"));
        assert!(SECTION_PATHS.contains(&"strategy.mevblocker_backrun"));
        assert!(SECTION_PATHS.contains(&"strategy.txpool_backrun"));
        let settlement: Vec<&str> = SCHEMA
            .iter()
            .filter(|k| k.section == "strategy.settlement")
            .map(|k| k.field)
            .collect();
        assert_eq!(settlement, vec!["active", "endpoints"]);
        for key in SCHEMA.iter().filter(|k| k.section == "strategy.settlement") {
            assert!(key
                .env
                .starts_with(std::concat!("DEGENBOT_", "STRATEGY_SETTLEMENT_")));
        }
        let mevblocker: Vec<&str> = SCHEMA
            .iter()
            .filter(|k| k.section == "strategy.mevblocker_backrun")
            .map(|k| k.field)
            .collect();
        assert_eq!(
            mevblocker,
            vec![
                "active",
                "bid_mode",
                "budget_wei",
                "max_bundle_wei",
                "bribe_bips",
                "priority_fee_gwei",
                "bundle_gas_est",
                "gas_floor_wei",
                "verify_ticks",
                "dry_run",
                "key_file",
                "executor",
                "operator",
                "sim_url",
                "rank_evidence",
                "connectors",
                "cycle_max_hops",
                "fixture_head",
                "stop_file",
                "endpoints",
                "mevblocker_url",
            ]
        );
        let peer: Vec<&str> = SCHEMA
            .iter()
            .filter(|k| k.section == "strategy.txpool_backrun")
            .map(|k| k.field)
            .collect();
        assert_eq!(
            peer,
            vec![
                "active",
                "bid_mode",
                "budget_wei",
                "max_bundle_wei",
                "bribe_bips",
                "priority_fee_gwei",
                "bundle_gas_est",
                "gas_floor_wei",
                "verify_ticks",
                "dry_run",
                "key_file",
                "executor",
                "operator",
                "sim_url",
                "rank_evidence",
                "connectors",
                "cycle_max_hops",
                "fixture_head",
                "stop_file",
                "endpoints",
            ]
        );
        // Each facet's dotted TOML path is its own section path + field.
        for key in SCHEMA
            .iter()
            .filter(|k| k.section.starts_with("strategy.") && k.section != "strategy.settlement")
        {
            assert_eq!(key.toml_path, format!("{}.{}", key.section, key.field));
        }
        for key in SCHEMA
            .iter()
            .filter(|k| k.section == "strategy.mevblocker_backrun")
        {
            assert!(key.env.starts_with("DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_"));
        }
        for key in SCHEMA
            .iter()
            .filter(|k| k.section == "strategy.txpool_backrun")
        {
            assert!(key.env.starts_with("DEGENBOT_STRATEGY_TXPOOL_BACKRUN_"));
        }
    }

    #[test]
    fn backrun_facet_keys_collapse_to_declared_defaults() {
        for b in [default_mevblocker_backrun(), default_txpool_backrun()] {
            assert!(!b.active);
            assert!(!b.bid_mode);
            assert_eq!(b.budget_wei, 0);
            assert_eq!(b.max_bundle_wei, 1_000_000_000_000_000);
            assert_eq!(b.bribe_bips, 9_800);
            assert_eq!(b.priority_fee_gwei, 2);
            assert_eq!(b.bundle_gas_est, 300_000);
            assert_eq!(b.gas_floor_wei, 1);
            assert_eq!(b.verify_ticks, VerifyTicks::Bootstrap);
            assert!(!b.dry_run);
            assert_eq!(b.key_file, None);
            assert_eq!(b.executor, "0x30b28ed8aa581fbc0191c3b532b0697773070e97");
            assert_eq!(b.operator, None);
            assert_eq!(b.sim_url, None);
            assert_eq!(b.endpoints, None);
            assert!(!b.rank_evidence);
            assert_eq!(b.connectors, 8);
            assert_eq!(b.cycle_max_hops, 4);
            assert_eq!(b.fixture_head, None);
            assert_eq!(
                b.stop_file,
                std::path::PathBuf::from("/tmp/degenbot-sidecar-STOP")
            );
        }
        assert_eq!(
            BotConfig::default()
                .strategy
                .mevblocker_backrun
                .mevblocker_url,
            None
        );
    }

    // The shared knob set of both per-ecosystem backrun facets, so the
    // defaults test reads each facet through one shape.
    #[expect(
        clippy::struct_excessive_bools,
        reason = "mirrors the facet's declared bool keys, one field per key"
    )]
    struct BackrunDefaults {
        active: bool,
        bid_mode: bool,
        budget_wei: u128,
        max_bundle_wei: u128,
        bribe_bips: u64,
        priority_fee_gwei: u64,
        bundle_gas_est: u64,
        gas_floor_wei: u64,
        verify_ticks: VerifyTicks,
        dry_run: bool,
        key_file: Option<std::path::PathBuf>,
        executor: String,
        operator: Option<String>,
        sim_url: Option<String>,
        rank_evidence: bool,
        connectors: usize,
        cycle_max_hops: usize,
        fixture_head: Option<u64>,
        stop_file: std::path::PathBuf,
        endpoints: Option<String>,
    }

    fn default_mevblocker_backrun() -> BackrunDefaults {
        let f = BotConfig::default().strategy.mevblocker_backrun;
        BackrunDefaults {
            active: f.active,
            bid_mode: f.bid_mode,
            budget_wei: f.budget_wei,
            max_bundle_wei: f.max_bundle_wei,
            bribe_bips: f.bribe_bips,
            priority_fee_gwei: f.priority_fee_gwei,
            bundle_gas_est: f.bundle_gas_est,
            gas_floor_wei: f.gas_floor_wei,
            verify_ticks: f.verify_ticks,
            dry_run: f.dry_run,
            key_file: f.key_file,
            executor: f.executor,
            operator: f.operator,
            sim_url: f.sim_url,
            rank_evidence: f.rank_evidence,
            connectors: f.connectors,
            cycle_max_hops: f.cycle_max_hops,
            fixture_head: f.fixture_head,
            stop_file: f.stop_file,
            endpoints: f.endpoints,
        }
    }

    fn default_txpool_backrun() -> BackrunDefaults {
        let f = BotConfig::default().strategy.txpool_backrun;
        BackrunDefaults {
            active: f.active,
            bid_mode: f.bid_mode,
            budget_wei: f.budget_wei,
            max_bundle_wei: f.max_bundle_wei,
            bribe_bips: f.bribe_bips,
            priority_fee_gwei: f.priority_fee_gwei,
            bundle_gas_est: f.bundle_gas_est,
            gas_floor_wei: f.gas_floor_wei,
            verify_ticks: f.verify_ticks,
            dry_run: f.dry_run,
            key_file: f.key_file,
            executor: f.executor,
            operator: f.operator,
            sim_url: f.sim_url,
            rank_evidence: f.rank_evidence,
            connectors: f.connectors,
            cycle_max_hops: f.cycle_max_hops,
            fixture_head: f.fixture_head,
            stop_file: f.stop_file,
            endpoints: f.endpoints,
        }
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "test fixtures fail loudly on an unconstructible prerequisite"
    )]
    fn cycle_max_hops_below_two_is_refused_with_the_remedy() {
        for (section, field) in [
            ("strategy.mevblocker_backrun", "cycle_max_hops"),
            ("strategy.txpool_backrun", "cycle_max_hops"),
        ] {
            for bad in ["0", "1"] {
                let mut config = BotConfig::default();
                config
                    .assign(section, field, bad)
                    .expect("the raw value parses as usize");
                let error = config
                    .validate()
                    .expect_err("below the pin-plus-connector minimum must refuse");
                let message = error.to_string();
                assert!(
                    message.contains("minimum") && message.contains("connector"),
                    "the refusal names the remedy: {message}"
                );
            }
        }
        BotConfig::default()
            .validate()
            .expect("the declared default 4 is valid");
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

    // `strmap` and the family-shaped `env_prefix` exist for tables whose KEYS
    // the operator picks at runtime (a per-chain endpoint table holds one
    // entry per chain): no closed element enum and no single env name can
    // describe one. The fixture expands the REAL declaration macro through
    // the same path a runtime key takes, so the vocabulary is pinned end to
    // end while the shipped SCHEMA — and the generated doc — stay unchanged.
    //
    // The family form is declared in BOTH positions a facet-aware key can
    // take — a section body and a facet body — because each position is a
    // separate set of macro arms that joins the section path before it reads
    // the env layer. A facet is the shape production uses (every `strategy.*`
    // arm), so covering only the section body would leave the facet arms
    // exercised by nothing.
    mod strmap_vocabulary {
        crate::config_schema! {
            strmap_fixture StrmapFixtureConfig {
                http [strmap] = ::std::collections::BTreeMap::new(), env_prefix = "FIXTURE_STRMAP_HTTP_", def = "(empty)",
                    doc = "Family-shaped per-key table (one env name per entry).";
                routes [strmap] = ::std::collections::BTreeMap::new(), env = "FIXTURE_STRMAP_ROUTES", def = "(empty)",
                    doc = "Single-name per-key table (one comma-separated env value).";
                probe [opt strmap] = None, env = "FIXTURE_STRMAP_PROBE", def = "(unset)",
                    doc = "Unset-able per-key table.";
                endpoints StrmapFixtureEndpointsConfig {
                    active [bool] = false, env = "FIXTURE_STRMAP_FACET_ACTIVE", def = "false",
                        doc = "Single-env key sharing the facet, mirroring the production strategy facets.";
                    rpc_urls [strmap] = ::std::collections::BTreeMap::new(), env_prefix = "FIXTURE_STRMAP_FACET_RPC_",
                        def = "(empty)", doc = "Family-shaped key inside a facet (one env name per entry).";
                    probe_urls [opt strmap] = None, env_prefix = "FIXTURE_STRMAP_FACET_PROBE_",
                        def = "(unset)", doc = "Unset-able family-shaped key inside a facet.";
                }
            }
        }
    }

    /// The generated reader answers the fixture's keys in BOTH positions a
    /// family-shaped key can take, so a reader arm covering only a section
    /// body fails here rather than at the first runtime read of a facet key.
    #[test]
    fn the_generated_reader_answers_a_family_shaped_key_in_both_positions() {
        assert_eq!(
            strmap_vocabulary::READABLE_KEYS.len(),
            strmap_vocabulary::SCHEMA.len()
        );
        // The seam projection is emitted beside the reader census from the
        // same arms, so the fixture exercises it in both key positions too.
        assert_eq!(
            strmap_vocabulary::VALUES_PROJECTION.len(),
            strmap_vocabulary::SCHEMA.len()
        );
        assert!(strmap_vocabulary::VALUES_PROJECTION
            .iter()
            .any(
                |(section, field)| (*section, *field) == ("strmap_fixture.endpoints", "probe_urls")
            ));
        let mut config = strmap_vocabulary::BotConfig::default();
        let set =
            |cfg: &mut strmap_vocabulary::BotConfig, section: &str, field: &str, raw: &str| {
                cfg.assign(section, field, raw)
                    .expect("a declared family key parses its raw form");
            };
        set(&mut config, "strmap_fixture", "http", "1=https://a.example");
        set(
            &mut config,
            "strmap_fixture",
            "probe",
            "1=https://b.example",
        );
        set(
            &mut config,
            "strmap_fixture.endpoints",
            "rpc_urls",
            "1=https://c.example",
        );
        set(
            &mut config,
            "strmap_fixture.endpoints",
            "probe_urls",
            "1=https://d.example",
        );
        let entry = |value: &str| ConfigValue::Map(vec![("1".to_string(), value.to_string())]);
        assert_eq!(
            config.value("strmap_fixture", "http"),
            Some(entry("https://a.example"))
        );
        assert_eq!(
            config.value("strmap_fixture", "probe"),
            Some(entry("https://b.example"))
        );
        assert_eq!(
            config.value("strmap_fixture.endpoints", "rpc_urls"),
            Some(entry("https://c.example"))
        );
        assert_eq!(
            config.value("strmap_fixture.endpoints", "probe_urls"),
            Some(entry("https://d.example"))
        );
    }

    #[test]
    fn strmap_keys_declare_an_empty_table_and_their_env_shape() {
        let config = strmap_vocabulary::BotConfig::default();
        assert!(config.strmap_fixture.http.is_empty());
        assert!(config.strmap_fixture.routes.is_empty());
        assert!(config.strmap_fixture.probe.is_none());

        let http = strmap_vocabulary::SCHEMA.iter().find(|k| k.field == "http");
        assert_eq!(
            http.map(|k| k.kind),
            Some(ValueKind {
                base: BaseKind::StrMap,
                optional: false
            })
        );
        // A family-shaped key keeps ONE name to print (the prefix) so error
        // labels and the writer's shadow check stay single-valued.
        assert_eq!(
            http.and_then(|k| k.env_prefix),
            Some("FIXTURE_STRMAP_HTTP_")
        );
        assert_eq!(http.map(|k| k.env), Some("FIXTURE_STRMAP_HTTP_"));
        assert_eq!(
            http.map(KeyDecl::label),
            Some("strmap_fixture.http (FIXTURE_STRMAP_HTTP_ / map<string, string>)".to_string())
        );

        let routes = strmap_vocabulary::SCHEMA
            .iter()
            .find(|k| k.field == "routes");
        assert_eq!(
            routes.map(|k| k.kind),
            Some(ValueKind {
                base: BaseKind::StrMap,
                optional: false
            })
        );
        assert_eq!(routes.and_then(|k| k.env_prefix), None);
        assert_eq!(routes.map(|k| k.env), Some("FIXTURE_STRMAP_ROUTES"));

        let probe = strmap_vocabulary::SCHEMA
            .iter()
            .find(|k| k.field == "probe");
        assert_eq!(
            probe.map(|k| k.kind),
            Some(ValueKind {
                base: BaseKind::StrMap,
                optional: true
            })
        );
        assert_eq!(
            probe.map(KeyDecl::label),
            Some(
                "strmap_fixture.probe (FIXTURE_STRMAP_PROBE / Option<map<string, string>>)"
                    .to_string()
            )
        );
        assert!(
            strmap_vocabulary::SECTION_PATHS.contains(&"strmap_fixture"),
            "the fixture section is recorded like any declared section"
        );
    }

    #[test]
    fn a_family_shaped_key_inside_a_facet_registers_its_dotted_path() {
        // A facet leaf is flattened to a two-part section path BEFORE the
        // env layer is read, so the family form must survive the join: the
        // registry facts below are the only thing pinning the dotted section
        // path, the `<dotted>.<field>` TOML path, and the prefix in both the
        // `env` and `env_prefix` slots.
        let rpc = strmap_vocabulary::SCHEMA
            .iter()
            .find(|k| k.field == "rpc_urls");
        assert_eq!(rpc.map(|k| k.section), Some("strmap_fixture.endpoints"));
        assert_eq!(
            rpc.map(|k| k.toml_path),
            Some("strmap_fixture.endpoints.rpc_urls")
        );
        assert_eq!(rpc.map(|k| k.env), Some("FIXTURE_STRMAP_FACET_RPC_"));
        assert_eq!(
            rpc.and_then(|k| k.env_prefix),
            Some("FIXTURE_STRMAP_FACET_RPC_")
        );
        assert_eq!(
            rpc.map(|k| k.kind),
            Some(ValueKind {
                base: BaseKind::StrMap,
                optional: false
            })
        );
        assert_eq!(
            rpc.map(KeyDecl::label),
            Some(
                "strmap_fixture.endpoints.rpc_urls (FIXTURE_STRMAP_FACET_RPC_ / map<string, string>)"
                    .to_string()
            )
        );

        // A second family key in the same facet pins the prefix PER KEY: a
        // declaration that reused the first key's prefix (or dropped it)
        // would still satisfy the assertions above.
        let probe = strmap_vocabulary::SCHEMA
            .iter()
            .find(|k| k.field == "probe_urls");
        assert_eq!(probe.map(|k| k.section), Some("strmap_fixture.endpoints"));
        assert_eq!(probe.map(|k| k.env), Some("FIXTURE_STRMAP_FACET_PROBE_"));
        assert_eq!(
            probe.and_then(|k| k.env_prefix),
            Some("FIXTURE_STRMAP_FACET_PROBE_")
        );
        assert_eq!(
            probe.map(|k| k.kind),
            Some(ValueKind {
                base: BaseKind::StrMap,
                optional: true
            })
        );

        // The single-env key in the same facet still gets `env_prefix: None`,
        // so the two forms do not bleed into each other.
        let active = strmap_vocabulary::SCHEMA
            .iter()
            .find(|k| k.field == "active");
        assert_eq!(active.map(|k| k.section), Some("strmap_fixture.endpoints"));
        assert_eq!(active.map(|k| k.env), Some("FIXTURE_STRMAP_FACET_ACTIVE"));
        assert_eq!(active.and_then(|k| k.env_prefix), None);

        assert!(
            strmap_vocabulary::SECTION_PATHS.contains(&"strmap_fixture.endpoints"),
            "the facet's dotted path is recorded like any declared section"
        );
    }

    #[test]
    fn a_family_shaped_key_inside_a_facet_assigns_through_the_two_level_arm() {
        let mut config = strmap_vocabulary::BotConfig::default();
        assert!(!config.strmap_fixture.endpoints.active);
        assert!(config.strmap_fixture.endpoints.rpc_urls.is_empty());
        assert!(config.strmap_fixture.endpoints.probe_urls.is_none());

        assert!(config
            .assign("strmap_fixture.endpoints", "active", "1")
            .is_ok());
        assert!(config.strmap_fixture.endpoints.active);
        assert!(config
            .assign(
                "strmap_fixture.endpoints",
                "rpc_urls",
                "1=https://a, 8453 = https://b"
            )
            .is_ok());
        let rpc = &config.strmap_fixture.endpoints.rpc_urls;
        assert_eq!(rpc.len(), 2);
        assert_eq!(rpc.get("1").map(String::as_str), Some("https://a"));
        assert_eq!(rpc.get("8453").map(String::as_str), Some("https://b"));
        assert!(config
            .assign("strmap_fixture.endpoints", "probe_urls", "1=https://c")
            .is_ok());
        assert_eq!(
            config
                .strmap_fixture
                .endpoints
                .probe_urls
                .as_ref()
                .and_then(|p| p.get("1"))
                .map(String::as_str),
            Some("https://c")
        );

        // The two-level arm labels a refusal with the FULL dotted path, so an
        // operator sees the facet-qualified key rather than a bare field name.
        let message = config
            .assign("strmap_fixture.endpoints", "rpc_urls", "1")
            .err();
        assert!(
            message
                .as_deref()
                .is_some_and(|m| m.starts_with("strmap_fixture.endpoints.rpc_urls: ")),
            "a refused facet assign names the dotted key: {message:?}"
        );
        assert_eq!(
            config
                .strmap_fixture
                .endpoints
                .rpc_urls
                .get("1")
                .map(String::as_str),
            Some("https://a"),
            "a refused assign leaves the loaded table untouched"
        );
    }

    #[test]
    fn strmap_assign_parses_a_trimmed_key_value_list() {
        let mut config = strmap_vocabulary::BotConfig::default();
        assert!(config
            .assign("strmap_fixture", "http", "1=https://a, 8453 = https://b")
            .is_ok());
        let http = &config.strmap_fixture.http;
        assert_eq!(http.len(), 2);
        assert_eq!(http.get("1").map(String::as_str), Some("https://a"));
        assert_eq!(http.get("8453").map(String::as_str), Some("https://b"));

        assert!(config
            .assign("strmap_fixture", "probe", "1=https://a")
            .is_ok());
        assert_eq!(
            config
                .strmap_fixture
                .probe
                .as_ref()
                .map(std::collections::BTreeMap::len),
            Some(1)
        );
    }

    #[test]
    fn strmap_assign_refuses_a_malformed_list_and_keeps_the_previous_table() {
        let mut config = strmap_vocabulary::BotConfig::default();
        assert!(config
            .assign("strmap_fixture", "routes", "1=https://a")
            .is_ok());
        for bad in [
            "1",
            "1=https://a,",
            "1=https://a,,2=https://b",
            "=https://a",
        ] {
            let message = config.assign("strmap_fixture", "routes", bad).err();
            assert!(
                message.as_deref().is_some_and(|m| m.contains("key=value")),
                "{bad:?} must be refused naming the key=value shape: {message:?}"
            );
        }
        assert_eq!(
            config.strmap_fixture.routes.get("1").map(String::as_str),
            Some("https://a"),
            "a refused assign leaves the loaded table untouched"
        );
    }

    #[test]
    fn a_later_strmap_assign_replaces_the_whole_table() {
        // The layer model is last-writer-wins per key, so a later env/file
        // value REPLACES the table rather than merging into it.
        let mut config = strmap_vocabulary::BotConfig::default();
        assert!(config
            .assign("strmap_fixture", "routes", "1=https://a,2=https://b")
            .is_ok());
        assert!(config
            .assign("strmap_fixture", "routes", "8453=https://c")
            .is_ok());
        let routes = &config.strmap_fixture.routes;
        assert_eq!(routes.len(), 1);
        assert_eq!(routes.get("8453").map(String::as_str), Some("https://c"));
    }
}
