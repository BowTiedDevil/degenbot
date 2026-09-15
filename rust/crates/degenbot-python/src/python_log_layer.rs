//! Tracing subscriber layer that forwards events to Python `logging`.
//!
//! Replaces `pyo3_log::init()` (per-record `Python::attach`) with a
//! batched-drain pattern: events are pushed onto a bounded
//! [`ArrayQueue`](crossbeam_queue::ArrayQueue) from any thread (no GIL
//! needed), and a dedicated OS thread drains the queue, batching up to 256
//! records or every 50 ms, then forwarding the batch to Python `logging`
//! via ONE `Python::attach` per flush.
//!
//! The queue is bounded so a stalled TTY or full pipe cannot stall the
//! per-log pump; a push past the ceiling drops the record and counts it as
//! `degenbot.log_dropped_total{sink="console"}` (ADR-043 §6 — never a silent
//! ceiling).
//!
//! This layer is the ONE console-emitting writer in the Python driver
//! (ADR-043 §6): the stderr `fmt` layer is routed to `io::sink()` because the
//! binding is present, so a record is never written twice.
//!
//! This makes the pump's tokio workers GIL-free for logging: the per-record
//! GIL round-trip is replaced by a lock-free queue push.
//!
//! # Shutdown
//!
//! Call [`shutdown_log_drainer`] to stop the drainer thread and flush
//! remaining records. The Python driver should call this before interpreter
//! finalization (e.g. in `__aexit__` or via Python `atexit`).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_queue::ArrayQueue;
use pyo3::prelude::*;
use pyo3::types::PyModule;
use tracing_subscriber::layer::{Context, Layer, Layered};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Registry;

/// Maximum batch size forwarded per `Python::attach` flush.
const BATCH_SIZE: usize = 256;

/// Maximum time between flushes — a partially full batch is flushed after
/// this interval to avoid starving the Python side.
const FLUSH_INTERVAL: Duration = Duration::from_millis(50);

/// A single log record ready for Python forwarding.
struct PythonLogRecord {
    /// The Python logger name, derived from the Rust target path.
    /// e.g. `"degenbot_bot.bot_core.block_pump"`.
    logger_name: String,
    /// Log level string: `"ERROR"`, `"WARN"`, `"INFO"`, `"DEBUG"`, or `"TRACE"`.
    level: String,
    /// The formatted log message.
    message: String,
}

/// Capacity of the bounded console queue: deep enough that a normal block's
/// burst never hits the ceiling, shallow enough that a stalled console cannot
/// grow memory without bound.
const CONSOLE_QUEUE_CAPACITY: usize = 8192;

/// Shared state between the tracing [`Layer`] and the drainer thread.
struct PythonLogLayerState {
    /// Bounded, lock-free queue shared with the drainer thread.
    queue: ArrayQueue<PythonLogRecord>,
    /// Records dropped because the queue was full (`log_dropped_total`).
    dropped: AtomicU64,
    /// Set to `true` to signal the drainer thread to shut down.
    shutdown: AtomicBool,
}

/// Count a console drop in Prometheus (`degenbot.log_dropped_total{sink}`).
/// The metric stack lives behind the `otel` feature; without it the drop is
/// still counted in-process ([`PythonLogLayerState::dropped`]).
#[cfg(feature = "otel")]
fn record_console_drop() {
    degenbot_bot::metrics::record_log_drop("console");
}

#[cfg(not(feature = "otel"))]
fn record_console_drop() {}

impl PythonLogLayerState {
    /// Push a record, counting a drop if the bounded queue is full.
    fn push(&self, record: PythonLogRecord) {
        if self.queue.push(record).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            record_console_drop();
        }
    }
}

/// A [`tracing_subscriber::Layer`] that forwards events to Python logging
/// via a batched, GIL-free channel.
///
/// Events are formatted and pushed onto a bounded queue (drops counted). A
/// dedicated OS thread drains the queue and flushes batches to Python via one
/// `Python::attach` per flush. See module-level docs.
pub struct PythonLogLayer {
    state: Arc<PythonLogLayerState>,
}

impl PythonLogLayer {
    /// Create a new `PythonLogLayer` and spawn the drainer thread.
    ///
    /// Call [`shutdown_log_drainer`] at shutdown to flush remaining records.
    ///
    /// # Panics
    ///
    /// Panics if the OS thread can't be spawned (e.g. resource exhaustion).
    #[must_use]
    pub fn new() -> Self {
        let state = Arc::new(PythonLogLayerState {
            queue: ArrayQueue::new(CONSOLE_QUEUE_CAPACITY),
            dropped: AtomicU64::new(0),
            shutdown: AtomicBool::new(false),
        });
        let drainer_state = Arc::clone(&state);
        // PE4FPM: self-register the drainer thread.
        degenbot_core::worker_census::register(degenbot_core::worker_census::WorkerCensusEntry {
            resource: "rust_log_drainer",
            kind: "std drainer thread (tracing → Python logging bridge)",
            count: 1,
            thread_name: "rust-log-drainer",
            sizing: "exactly one (fixed; built with the layer at logging init)",
            binding: "pinned",
        });
        #[expect(clippy::expect_used)] // invariant-guarded (documented)
        let _drainer = thread::Builder::new()
            .name("rust-log-drainer".into())
            .spawn(move || drainer_loop(drainer_state))
            .expect("spawn rust-log-drainer thread");
        Self { state }
    }

    /// Register a `shutdown_log_drainer()` pyfunction on the given module
    /// so the Python driver can call it at shutdown.
    ///
    /// # Errors
    ///
    /// Returns a `PyErr` if the function can't be registered on the module.
    pub fn register_pyfunction(m: &Bound<'_, PyModule>) -> PyResult<()> {
        m.add_function(pyo3::wrap_pyfunction!(shutdown_log_drainer, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(flush_telemetry, m)?)?;
        Ok(())
    }
}

impl Default for PythonLogLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for PythonLogLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    /// TPMFLV: every span creation records the creating thread into the
    /// diagnostics thread-registry (std thread-id -> OS TID + last span).
    /// Runs on the span-creating thread; cheap (map lookup, one /proc read
    /// per thread lifetime) and feeds the GIL-deadlock self-dump.
    fn on_new_span(
        &self,
        attr: &tracing::span::Attributes<'_>,
        _id: &tracing::span::Id,
        _ctx: Context<'_, S>,
    ) {
        // Span source-location is not exposed by this tracing version's
        // Metadata; the Jaeger spans already carry code.file.path / line.
        crate::diagnostics::thread_registry::note_current_thread(attr.metadata().name(), None);
    }

    fn on_event(&self, event: &tracing::Event, _ctx: Context<'_, S>) {
        let meta = event.metadata();

        // Never recurse: if an event originates from the drainer thread
        // (which holds the GIL during flush), skip it to avoid re-entering
        // the Python logging layer.
        if thread::current().name() == Some("rust-log-drainer") {
            return;
        }

        // Parse the level.
        let level = meta.level().to_string(); // "ERROR", "WARN", "INFO", "DEBUG", "TRACE"

        // Build the Python logger name from the Rust target path.
        // `tracing_log::LogTracer` bridges `log::` records as tracing events
        // with `target = "log"` and the original log target stored in the
        // `log.target` field. Handle this transparently.
        let logger_name = if meta.target() == "log" {
            // Extract the original log target from the event's fields.
            extract_log_target(event).unwrap_or_else(|| "log".to_string())
        } else {
            meta.target().to_string()
        }
        .replace("::", ".");

        // Format the message by visiting all fields.
        let message = format_event_message(event);

        let record = PythonLogRecord {
            logger_name,
            level,
            message,
        };

        // Bounded push (ADR-043 §6): the drainer batches + forwards as fast
        // as Python logging consumes, and the console filter keeps
        // high-frequency diagnostics off this path, so the ceiling is
        // headroom — but a stalled console drops + COUNTS rather than stalling
        // the pump or growing memory without bound.
        self.state.push(record);
    }
}

/// Extract the original `log::` target from a `LogTracer`-bridged event.
///
/// When `tracing_log::LogTracer` bridges a `log::` record, the tracing event
/// has `target = "log"` and the original log target is stored in the
/// `log.target` field. This function visits the event fields and returns
/// the original target if found.
/// Resolve the OTLP endpoint for span export. Precedence (first wins):
///
/// 1. `OTEL_EXPORTER_OTLP_ENDPOINT` env var (standard `OTel` signal env —
///    explicit, always wins),
/// 2. `telemetry.jaeger_endpoint` in the user config file (the modern
///    OTLP-key home; the pre-0.6 `[otel]` table is retired and any file
///    still carrying it is refused at boot — JLFE2F),
/// 3. `None` — the exporter falls back to its built-in default
///    (`http://localhost:4318`).
///
/// A malformed config file is treated as absent (warn + fall through) rather
/// than disabling telemetry — a typo in one key must not silence tracing.
#[cfg(feature = "otel")]
#[must_use]
pub(crate) fn resolve_otlp_endpoint(
    env_raw: Option<&str>,
    config_file: Option<&std::path::Path>,
) -> Option<String> {
    if let Some(url) = env_raw.filter(|s| !s.is_empty()) {
        return Some(url.to_owned());
    }
    let path = config_file?;
    let Ok(text) = std::fs::read_to_string(path) else {
        return None;
    };
    match text.parse::<toml::Table>() {
        Ok(table) => table
            .get("telemetry")
            .and_then(|telemetry| telemetry.get("jaeger_endpoint"))
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        Err(e) => {
            tracing::warn!(%e, path = %path.display(), "user config.toml failed to parse - otel.endpoint ignored");
            None
        }
    }
}

/// Read the `[failure_policy]` override table from the user config file
/// (ADR-040 D3): `kind` or `kind.reason` → action string. Malformed FILE →
/// no overrides (warn + continue, matching the config helpers' fail-open
/// rule); an INVALID key/action (a table that parses but names an unknown
/// bucket or action) is a boot error — the bail path panics loudly at the
/// mgr call site after logging.
pub(crate) fn read_failure_policy_overrides(
    config_file: Option<&std::path::Path>,
) -> Result<Vec<(String, String)>, String> {
    let Some(path) = config_file else {
        return Ok(Vec::new());
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(Vec::new());
    };
    match text.parse::<toml::Table>() {
        Ok(table) => {
            let Some(policy) = table.get("failure_policy") else {
                return Ok(Vec::new());
            };
            let Some(entries) = policy.as_table() else {
                return Err("must be a table of bucket = action".to_owned());
            };
            let mut out = Vec::new();
            for (k, v) in entries {
                if let Some(a) = v.as_str() {
                    // flat form: kind = "action" (or a quoted "kind.reason" key)
                    out.push((k.clone(), a.to_owned()));
                    continue;
                }
                if let Some(sub) = v.as_table() {
                    // nested form: kind = { reason = "action" } or
                    // [failure_policy.kind] reason = "action" - flatten one
                    // level so the reason sub-taxonomies read naturally.
                    for (k2, v2) in sub {
                        let Some(a) = v2.as_str() else {
                            return Err(format!(
                                "[failure_policy].{k}.{k2} must be a string action"
                            ));
                        };
                        out.push((format!("{k}.{k2}"), a.to_owned()));
                    }
                    continue;
                }
                return Err(format!("[failure_policy].{k} must be a string action"));
            }
            Ok(out)
        }
        Err(e) => {
            tracing::warn!(
                %e,
                path = %path.display(),
                "user config.toml failed to parse - failure_policy ignored"
            );
            Ok(Vec::new())
        }
    }
}

fn extract_log_target(event: &tracing::Event) -> Option<String> {
    struct LogTargetExtractor(Option<String>);

    impl tracing::field::Visit for LogTargetExtractor {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            if field.name() == "log.target" {
                self.0 = Some(value.to_owned());
            }
        }

        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "log.target" {
                self.0 = Some(format!("{value:?}"));
            }
        }
    }

    let mut extractor = LogTargetExtractor(None);
    event.record(&mut extractor);
    extractor.0
}

/// Visit all fields of a tracing event and format them into a string.
fn format_event_message(event: &tracing::Event) -> String {
    struct FieldCollector {
        message: Option<String>,
        other: Vec<String>,
    }

    impl tracing::field::Visit for FieldCollector {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            // Skip LogTracer bridge metadata fields — they are not user-facing.
            if field.name().starts_with("log.") {
                return;
            }
            let formatted = format!("{value:?}");
            if field.name() == "message" {
                // Strip surrounding quotes from debug representation.
                let stripped = formatted
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                    .unwrap_or(&formatted);
                self.message = Some(stripped.to_owned());
            } else {
                self.other.push(format!("{}={}", field.name(), formatted));
            }
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            // Skip LogTracer bridge metadata fields.
            if field.name().starts_with("log.") {
                return;
            }
            if field.name() == "message" {
                self.message = Some(value.to_owned());
            } else {
                self.other.push(format!("{}={:?}", field.name(), value));
            }
        }
    }

    let mut collector = FieldCollector {
        message: None,
        other: Vec::new(),
    };
    event.record(&mut collector);

    let mut msg = collector.message.unwrap_or_default();
    if !collector.other.is_empty() {
        msg.push_str(" {");
        msg.push_str(&collector.other.join(", "));
        msg.push('}');
    }
    msg
}

/// The drainer thread main loop.
///
/// Batches up to `BATCH_SIZE` records or flushes after `FLUSH_INTERVAL`,
/// then forwards the batch to Python logging via a single `Python::attach`.
#[expect(clippy::needless_pass_by_value)] // owned Arc moved into drainer thread closure
fn drainer_loop(state: Arc<PythonLogLayerState>) {
    let mut batch: Vec<PythonLogRecord> = Vec::with_capacity(BATCH_SIZE);
    let mut last_flush = Instant::now();

    loop {
        // Drain as many records from the queue as available (up to batch_size).
        while batch.len() < BATCH_SIZE {
            match state.queue.pop() {
                Some(record) => batch.push(record),
                None => break,
            }
        }

        let elapsed = last_flush.elapsed();
        let should_flush =
            !batch.is_empty() && (batch.len() >= BATCH_SIZE || elapsed >= FLUSH_INTERVAL);

        if should_flush {
            flush_batch_to_python(&batch);
            batch.clear();
            last_flush = Instant::now();
        }

        // Check shutdown: if the flag is set, flush remaining and exit.
        if state.shutdown.load(Ordering::Acquire) {
            if !batch.is_empty() {
                flush_batch_to_python(&batch);
                batch.clear();
            }
            // One final drain of any records pushed between the pop loop
            // and the shutdown check.
            while let Some(record) = state.queue.pop() {
                batch.push(record);
            }
            if !batch.is_empty() {
                flush_batch_to_python(&batch);
            }
            break;
        }

        // Avoid busy-waiting: sleep before retrying.
        thread::sleep(Duration::from_millis(10));
    }
}

/// Forward a batch of records to Python logging via ONE `Python::attach`.
///
/// `try_attach`, not `attach`: this is a plain OS thread, and nothing joins it
/// (`shutdown_log_drainer` only raises the shutdown flag), so a flush can be in
/// flight while `CPython` finalizes — e.g. a driver shell that raises and exits
/// immediately. `PyGILState_Ensure` during finalization mints an auto
/// thread-state that shutdown then destroys, and the release aborts the whole
/// process with "`PyGILState_Release`: auto-releasing thread-state, but no
/// thread-state for this thread". Dropping the tail batch when the interpreter
/// is shutting down is the correct trade at process exit (pyo3 documents
/// `try_attach` for exactly this case).
fn flush_batch_to_python(records: &[PythonLogRecord]) {
    let _ = Python::try_attach(|py| {
        let Ok(logging_mod) = py.import("logging") else {
            return; // Python logging not available — drop batch.
        };

        // Cache the level constants on first use.
        let level_error = logging_mod.getattr("ERROR").ok();
        let level_warn = logging_mod.getattr("WARN").ok();
        let level_info = logging_mod.getattr("INFO").ok();
        let level_debug = logging_mod.getattr("DEBUG").ok();

        for record in records {
            // Resolve the numeric level.
            let py_level = match record.level.as_str() {
                "ERROR" => level_error.as_ref(),
                "WARN" => level_warn.as_ref(),
                "DEBUG" | "TRACE" => level_debug.as_ref(),
                _ => level_info.as_ref(), // INFO or fallback
            };

            let Some(py_level) = py_level else {
                continue; // Can't determine level — skip.
            };

            // Get the target-specific logger.
            let Ok(logger) = logging_mod.call_method1("getLogger", (record.logger_name.as_str(),))
            else {
                continue;
            };

            // Call logger.log(level, msg).
            let _ = logger.call_method1("log", (py_level, record.message.as_str()));
        }
    });
}

/// `PyO3` function: signal the drainer thread to shut down and flush
/// remaining records.
///
/// This is idempotent: calling it multiple times is safe. The drainer
/// thread is joined implicitly (it exits on its own after the shutdown
/// flag + final flush).
#[pyfunction]
pub fn shutdown_log_drainer() {
    // The layer is installed as a global subscriber; to find its state,
    // we use a global static.
    if let Some(state) = GLOBAL_LAYER_STATE.get() {
        state.shutdown.store(true, Ordering::Release);
    }
    flush_telemetry();
    // K6PCKP: kill the OTel provider after the flush (None-safe when the
    // DEGENBOT_OTEL env gate was off).
    #[cfg(feature = "otel")]
    if let Some(handle) = OTEL_PROVIDER.get() {
        let _ = handle.shutdown();
    }
}

/// `PyO3` function: flush every telemetry provider (spans + metrics) so the
/// tail of a run is not lost to teardown ordering (ADR-043 §6).
///
/// Call this BEFORE the tokio runtime is dropped: the OTLP exporter needs the
/// runtime for its final batch, so a flush after teardown exports nothing.
/// Idempotent and none-safe (telemetry off -> no-op).
#[pyfunction]
pub fn flush_telemetry() {
    #[cfg(feature = "otel")]
    {
        if let Some(handle) = OTEL_PROVIDER.get() {
            let _ = handle.flush();
        }
        degenbot_bot::metrics::flush_global_metrics();
    }
}

/// K6PCKP: process-lifetime `OTel` tracer provider for the Python-driven
/// registry. Set by `init_logging_subscriber` when `DEGENBOT_OTEL=1`;
/// `shutdown_log_drainer` flushes/kills it.
#[cfg(feature = "otel")]
static OTEL_PROVIDER: std::sync::OnceLock<degenbot_bot::otel::OtelHandle> =
    std::sync::OnceLock::new();

/// Global reference to the layer state so `shutdown_log_drainer` can
/// signal it. Set during `init_logging_subscriber`.
static GLOBAL_LAYER_STATE: std::sync::OnceLock<Arc<PythonLogLayerState>> =
    std::sync::OnceLock::new();

/// Guard against calling `init_logging_subscriber` more than once.
static INIT_DONE: std::sync::OnceLock<()> = std::sync::OnceLock::new();

/// The CONSOLE filter (stderr fmt + Python forwarder) resolved from the typed
/// telemetry config per ADR-043 §4: the Python-driver wiring default (`info`),
/// overridden by `telemetry.log_level` and escalated per-domain by
/// `telemetry.diag`; an explicit `RUST_LOG` wins verbatim on every sink.
fn console_env_filter() -> EnvFilter {
    EnvFilter::new(
        &degenbot_bot::telemetry::resolve_filters(
            degenbot_bot::telemetry::CONSOLE_WIRING_DEFAULT_PYTHON,
        )
        .console,
    )
}

/// The RECORD-level filter for the `OTel` layer (`warn,degenbot=debug` by
/// default), so Jaeger keeps every degenbot diagnostic event even when the
/// console is quiet.
#[cfg(feature = "otel")]
fn record_env_filter() -> EnvFilter {
    EnvFilter::new(
        &degenbot_bot::telemetry::resolve_filters(
            degenbot_bot::telemetry::CONSOLE_WIRING_DEFAULT_PYTHON,
        )
        .otel,
    )
}

/// Install the tracing subscriber stack.
///
/// This function:
/// 1. Installs `tracing_log::LogTracer` as the global `log` logger, so
///    existing `log::` calls flow through the tracing subscriber.
/// 2. Builds a `tracing_subscriber::Registry` with two layers:
///    - A `fmt` layer writing to stderr (controlled by `RUST_LOG`).
///    - A [`PythonLogLayer`] forwarding events to Python `logging`.
///
/// Must be called from the `#[pymodule]` init (where Python is initialized
/// and the GIL is held). Safe to call multiple times — subsequent calls
/// are no-ops.
/// The base registry stack that [`build_base_registry`] returns: the
/// Python slot `P` on top of the `fmt` writer layer on top of
/// `EnvFilter`.
type MiddleRegistry = Layered<
    tracing_subscriber::fmt::Layer<
        Layered<EnvFilter, Registry>,
        tracing_subscriber::fmt::format::DefaultFields,
        tracing_subscriber::fmt::format::Format<tracing_subscriber::fmt::format::Full>,
        tracing_subscriber::fmt::writer::BoxMakeWriter,
    >,
    Layered<EnvFilter, Registry>,
>;

type BaseRegistry<P> = Layered<P, MiddleRegistry>;

/// Assembles the base logging registry: `EnvFilter` + stderr `fmt` and a
/// final Python-forwarding slot `P` (K6PCKP extraction). Byte-equivalent
/// to the pre-K6PCKP inline assembly in `init_logging_subscriber`;
/// extracted so the `OTel` layer can layer on top and the stack is testable
/// without a `Python::attach`-capable drainer (seam C).
#[must_use]
pub(crate) fn build_base_registry<P>(env_filter: EnvFilter, python_layer: P) -> BaseRegistry<P>
where
    P: Layer<MiddleRegistry> + 'static,
{
    use tracing_subscriber::layer::SubscriberExt;
    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(fmt_writer())
                .with_ansi(true),
        )
        .with(python_layer)
}

/// The writer for the stderr `fmt` layer: always `io::sink()` in the Python
/// driver.
///
/// Exactly one console-emitting writer per process (ADR-043 §6), and the
/// binding IS present here, so [`PythonLogLayer`] owns the console and the
/// `fmt` layer must stay silent — otherwise every record is written twice.
/// This replaces the `DEGENBOT_LOG_FMT` two-tunnel hack (retired). The writer
/// (not the layer) is swapped so the builder types — and every caller/test
/// against them — stay unchanged.
fn fmt_writer() -> tracing_subscriber::fmt::writer::BoxMakeWriter {
    tracing_subscriber::fmt::writer::BoxMakeWriter::new(std::io::sink)
}

#[cfg(feature = "otel")]
/// The `OTel` layer gated by the RECORD-level filter (uncapped diagnostics).
type OtelRecordGated<L> =
    Layered<tracing_subscriber::filter::Filtered<L, EnvFilter, Registry>, Registry>;

/// The stderr fmt layer gated by the CONSOLE filter (diagnostics capped).
#[cfg(feature = "otel")]
type MiddleRegistryOtel<L> = Layered<
    tracing_subscriber::filter::Filtered<
        tracing_subscriber::fmt::Layer<
            OtelRecordGated<L>,
            tracing_subscriber::fmt::format::DefaultFields,
            tracing_subscriber::fmt::format::Format<tracing_subscriber::fmt::format::Full>,
            tracing_subscriber::fmt::writer::BoxMakeWriter,
        >,
        EnvFilter,
        OtelRecordGated<L>,
    >,
    OtelRecordGated<L>,
>;

#[cfg(feature = "otel")]
type BaseRegistryOtel<P, L> = Layered<
    tracing_subscriber::filter::Filtered<P, EnvFilter, MiddleRegistryOtel<L>>,
    MiddleRegistryOtel<L>,
>;

/// Assembles the base registry with the `OTel` span layer pinned at the
/// bottom of the stack (the SDK layer can only layer onto the bare
/// Registry): `OTel + EnvFilter + fmt + the Python slot`.
#[must_use]
#[cfg(feature = "otel")]
#[cfg(feature = "otel")]
pub(crate) fn build_base_registry_with_otel<P, L>(
    console_filter: EnvFilter,
    record_filter: EnvFilter,
    otel_layer: L,
    python_layer: P,
) -> BaseRegistryOtel<P, L>
where
    P: Layer<MiddleRegistryOtel<L>> + 'static,
    L: Layer<Registry> + 'static,
{
    use tracing_subscriber::layer::Layer as _;
    use tracing_subscriber::layer::SubscriberExt;
    tracing_subscriber::registry()
        // Per-layer filters (2026-08-22 audit): the OTel layer sees the
        // RECORD-level filter (diagnostics uncapped — Jaeger keeps full
        // detail); the console sinks see the console filter (diagnostics
        // capped at warn). A single global filter could not split them.
        .with(otel_layer.with_filter(record_filter))
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(fmt_writer())
                .with_ansi(true)
                .with_filter(console_filter.clone()),
        )
        .with(python_layer.with_filter(console_filter))
}

pub fn init_logging_subscriber() {
    let () = INIT_DONE.get_or_init(|| {

        // Build the Python-forwarding layer.
        let python_layer = PythonLogLayer::new();

        // Store the state reference for `shutdown_log_drainer`.
        let state = Arc::clone(&python_layer.state);
        let _ = GLOBAL_LAYER_STATE.set(state);

        // Set the global log -> tracing bridge BEFORE the subscriber, so
        // any log:: calls during subscriber setup are captured.
        let _ = tracing_log::LogTracer::init();

        // Build the subscriber registry. `EnvFilter` controls which events
        // each layer receives; ADR-043 §4 resolves it from the typed telemetry
        // config, with an explicit `RUST_LOG` winning verbatim on every sink
        // (the config knobs are then ignored, with one WARN).
        #[cfg(feature = "otel")]
        let (console_filter, record_filter) = (console_env_filter(), record_env_filter());
        #[cfg(not(feature = "otel"))]
        let console_filter = console_env_filter();

        #[cfg(feature = "otel")]
        {
            // K6PCKP: OTel OTLP span layer. Import-time env gate (same
            // rationale as DEGENBOT_HOTPATH): the pymodule init is the one
            // justified implicit call site. Layered third on the base
            // registry; the provider lives in OTEL_PROVIDER for
            // `shutdown_log_drainer` flush/kill.
            //
            // T5/RMH23E dev default: ON whenever the `otel` feature is
            // compiled (dev builds only — release wheels carry zero OTel
            // code). Opt out with DEGENBOT_OTEL=0. Endpoint precedence:
            // OTEL_EXPORTER_OTLP_ENDPOINT env > telemetry.jaeger_endpoint
            // in ~/.config/degenbot/config.toml (JLFE2F: the pre-0.6
            // [otel] table is retired) > exporter default.
            let endpoint = resolve_otlp_endpoint(
                std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok().as_deref(),
                ::degenbot_config::standard_file_path().as_deref(),
            );
            let build_provider = || match &endpoint {
                Some(url) => degenbot_bot::otel::provider_from_endpoint(url),
                None => degenbot_bot::otel::provider_from_env_endpoint(),
            };
            // KAHU5W: typed schema key `telemetry.otel` (`DEGENBOT_OTEL`).
            // The legacy per-key `~/.config/degenbot/config.toml` fallback is
            // subsumed by the typed loader's `--config` layer.
            let otel_enabled = ::degenbot_config::holder::config().telemetry.otel;
            let otel_layer = if otel_enabled {
                match build_provider() {
                    Ok((provider, tracer)) => {
                        let _ = OTEL_PROVIDER.set(degenbot_bot::otel::OtelHandle::new(provider));
                        tracing::info!(
                            endpoint = endpoint.as_deref().unwrap_or("http://localhost:4318 (exporter default)"),
                            "OTel OTLP span layer active"
                        );
                        Some(degenbot_bot::otel::layer(tracer))
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "OTel layer disabled: OTLP exporter build failed");
                        None
                    }
                }
            } else {
                tracing::debug!(
                    "OTel span layer inactive (set telemetry.otel = false / DEGENBOT_OTEL=0 to opt out)"
                );
                None
            };
            // T1: metrics ride the same DEGENBOT_OTEL gate — the Prometheus
            // scrape endpoint (DEGENBOT_METRICS_ADDR, default 127.0.0.1:9464)
            // serves whatever instruments the drain path records. Fail-open:
            // a bind failure logs and the bot runs without scrapeable metrics.
            if otel_layer.is_some() {
                // Scrape endpoint (KAHU5W): typed schema key
                // `telemetry.metrics_addr` / `DEGENBOT_METRICS_ADDR`.
                let metrics_raw =
                    ::degenbot_config::holder::config().telemetry.metrics_addr.clone();
                match metrics_raw.parse::<std::net::SocketAddr>() {
                    Ok(addr) => match degenbot_bot::metrics::init_global_metrics_with_addr(addr) {
                        Ok(()) => tracing::info!(addr = %addr, "Prometheus metrics endpoint active"),
                        Err(e) => {
                            tracing::warn!(error = %e, "metrics endpoint disabled: exporter build failed");
                        }
                    },
                    Err(e) => tracing::warn!(
                        raw = %metrics_raw,
                        error = %e,
                        "metrics endpoint disabled: invalid address (check DEGENBOT_METRICS_ADDR / otel.metrics_addr)"
                    ),
                }
            }
            match otel_layer {
                Some(ol) => set_global_subscriber(build_base_registry_with_otel(
                    console_filter,
                    record_filter,
                    ol,
                    python_layer,
                )),
                None => set_global_subscriber(build_base_registry(console_filter, python_layer)),
            }
        }

        #[cfg(not(feature = "otel"))]
        {
            set_global_subscriber(build_base_registry(console_filter, python_layer));
        }
    });
}

/// Install the registry as the global default subscriber without touching
/// the `log` global logger.
///
/// `SubscriberInitExt::init()` ALSO initializes a `log` compat layer when
/// tracing-subscriber's `tracing-log` feature is compiled in — and Cargo
/// feature unification can flip that default feature on for any workspace-
/// wide build even though this crate resolves without it. That second
/// install then fails against the explicit `LogTracer::init()` bridge above,
/// panicking with "failed to set global default subscriber:
/// `SetLoggerError(())`" (TU252C: deterministic `cargo test --workspace`
/// failure of `degenbot_rs --test logging_parity`). The free function sets
/// only the subscriber; the explicit bridge remains the single owner of the
/// `log` slot under every feature resolution.
fn set_global_subscriber<S>(subscriber: S)
where
    S: tracing::Subscriber + Send + Sync + 'static,
{
    if let Err(e) = tracing::subscriber::set_global_default(subscriber) {
        // Fail-open: a pre-existing subscriber keeps serving. Route through
        // `log` rather than our own (not-installed) layer — if some other
        // bridge owns the slot, this still surfaces; otherwise it drops.
        log::warn!("degenbot: global tracing subscriber not installed: {e}");
    }
}

// clippy --all-targets gate (UX66EM): this test module uses unwrap; the
// feature-gated otel test's expect calls carry their own targeted
// #[expect(clippy::expect_used)] so the module attribute stays fulfilled
// in default (no-otel) builds too.
#[expect(clippy::unwrap_used)]
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing::Level;
    use tracing_subscriber::layer::{Context, Layer};
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::registry::LookupSpan;
    use tracing_subscriber::Registry;

    /// A layer that records every event it receives as `(target, level)`,
    /// so a test can assert exactly which events the default filter lets
    /// through.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<(String, Level)>>>);

    impl<S> Layer<S> for Capture
    where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            self.0.lock().unwrap().push((
                event.metadata().target().to_string(),
                *event.metadata().level(),
            ));
        }
    }

    fn run_with_default_filter<R>(f: impl FnOnce() -> R) -> (R, Vec<(String, Level)>) {
        let capture = Capture::default();
        let subscriber = Registry::default()
            .with(console_env_filter())
            .with(capture.clone());
        let ret = tracing::dispatcher::with_default(&tracing::Dispatch::new(subscriber), f);
        let records = capture.0.lock().unwrap().clone();
        (ret, records)
    }

    /// ADR-043 §6: the console queue is bounded and every drop is counted
    /// (a silent ceiling is not acceptable).
    #[test]
    fn bounded_console_queue_counts_drops() {
        let state = PythonLogLayerState {
            queue: ArrayQueue::new(2),
            dropped: AtomicU64::new(0),
            shutdown: AtomicBool::new(false),
        };
        for i in 0..3 {
            state.push(PythonLogRecord {
                logger_name: "degenbot.test".to_string(),
                level: "INFO".to_string(),
                message: format!("record {i}"),
            });
        }
        assert_eq!(state.dropped.load(Ordering::Relaxed), 1);
        assert_eq!(state.queue.len(), 2);
    }
    /// K6PCKP seam C: the "`OTel`" layer composes onto the base logging
    /// registry (`EnvFilter` + fmt + the Python slot) and receives spans
    /// without a "`Python::attach"-capable` drainer thread. A "Capture"
    /// stand-in plays the Python slot (the real drainer needs the
    /// interpreter); this also proves both layers live in one registry.
    /// The global slot is per-process and no other lib test sets one,
    /// so this test owns it deterministically (repo convention).
    // Targeted expect (fulfilled when the otel feature is on): the global
    /// subscriber slot and the exporter/provider plumbing only fail on
    /// programmer error, so the test asserts loudly rather than propagating.
    #[expect(clippy::expect_used)]
    #[cfg(feature = "otel")]
    #[test]
    fn otel_layer_composes_into_the_base_registry() {
        use degenbot_bot::otel;
        use opentelemetry_sdk::trace::InMemorySpanExporter;

        let exporter = InMemorySpanExporter::default();
        let (provider, tracer) = otel::provider_with_exporter(exporter.clone());

        let capture = Capture::default();
        // ADR-043 §4 split: the console layer is quiet (warn) while the record
        // layer enables the degenbot diagnostics at debug.
        let subscriber = build_base_registry_with_otel(
            EnvFilter::new("warn"),
            EnvFilter::new("degenbot=debug"),
            otel::layer(tracer),
            capture.clone(),
        );
        // Scoped thread-local subscriber: every span this test observes is
        // created on THIS thread (in_scope), so the once-per-process global
        // slot stays free for the cross-thread sim-span test (7LV6VN T1).
        tracing::subscriber::with_default(subscriber, || {
            tracing::info_span!("seam.c.span").in_scope(|| {
                tracing::warn!(target: "degenbot_bot::bot_core::block_pump", "seam c event");
            });
            provider.force_flush().expect("flush");

            // Console-vs-trace split invariant: a high-frequency diagnostic
            // event (degenbot::diag INFO) must reach the OTel layer (uncapped
            // record filter) while being capped OFF the console sinks. Kept
            // inside the scoped subscriber - all emissions stay on THIS
            // thread, so the thread-local layer sees them.
            tracing::info_span!("seam.c.diag.span").in_scope(|| {
                tracing::info!(
                    target: degenbot_bot::telemetry::DIAGNOSTIC_TARGET,
                    "seam c diag event"
                );
            });
            provider.force_flush().expect("flush after diag event");
        });

        let spans = exporter.get_finished_spans().expect("spans");
        assert!(
            spans.iter().any(|sp| sp.name.as_ref() == "seam.c.span"),
            "seam.c.span missing from the OTel exporter; got {:?}",
            spans.iter().map(|sp| sp.name.as_ref()).collect::<Vec<_>>()
        );

        let spans = exporter
            .get_finished_spans()
            .expect("spans after diag event");
        assert!(
            spans
                .iter()
                .any(|sp| sp.events.iter().any(|e| e.name == "seam c diag event")),
            "diagnostic event missing from the OTel layer — the record filter \
             must NOT carry the console cap"
        );

        let records = capture.0.lock().expect("test lock").clone();
        assert!(
            !records
                .iter()
                .any(|(target, level)| { *level == Level::INFO && target.contains("diag") }),
            "degenbot diagnostic INFO leaked to the quiet console sink"
        );
        assert!(
            records
                .iter()
                .any(|(target, level)| { *level == Level::WARN && target.contains("block_pump") }),
            "WARN event did not reach the Python-slot layer; records: {records:?}"
        );
    }

    #[test]
    fn default_filter_keeps_alloy_error_but_drops_alloy_info_debug() {
        let ((), records) = run_with_default_filter(|| {
            // ERROR on an alloy target — must still surface (real failures).
            tracing::event!(target: "alloy_pubsub::service", Level::ERROR, "backend error");
            // Routine INFO/DEBUG — the noise being throttled.
            tracing::event!(
                target: "alloy_pubsub::service",
                Level::INFO,
                "Pubsub service request channel closed. Shutting down."
            );
            tracing::event!(target: "alloy_pubsub::service", Level::DEBUG, "detail");
        });

        let on_alloy = |level: Level| {
            records
                .iter()
                .any(|(target, lvl)| target.starts_with("alloy") && *lvl == level)
        };
        assert!(
            on_alloy(Level::ERROR),
            "alloy ERROR must pass the `=warn` throttle"
        );
        assert!(
            !on_alloy(Level::INFO),
            "alloy INFO must be throttled by the default filter"
        );
        assert!(
            !on_alloy(Level::DEBUG),
            "alloy DEBUG must be throttled by the default filter"
        );
    }

    #[test]
    fn default_filter_keeps_degenbot_targets_at_info() {
        let ((), records) = run_with_default_filter(|| {
            tracing::event!(target: "degenbot_bot::bot_core::block_pump", Level::INFO, "mine");
        });
        assert!(
            records
                .iter()
                .any(|(target, level)| target.starts_with("degenbot") && *level == Level::INFO),
            "degenbot's own INFO records must not be throttled"
        );
    }

    // T5/RMH23E: OTLP endpoint precedence - env var beats config file;
    // config file beats None; missing/malformed config falls through to None
    // (exporter default) without disabling telemetry.
    #[cfg(feature = "otel")]
    #[test]
    fn resolve_otlp_endpoint_precedence() {
        use std::io::Write as _;

        let dir = std::env::temp_dir().join(format!("degenbot-otel-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.toml");

        // 1. env wins over an existing config file
        let contents = concat!(
            "[telemetry]\n",
            "jaeger_endpoint = \"http://from-config:4318\"\n"
        );
        std::fs::write(&cfg, contents).unwrap();
        assert_eq!(
            resolve_otlp_endpoint(Some("http://from-env:4318"), Some(&cfg)).as_deref(),
            Some("http://from-env:4318"),
            "env var must win over the config file"
        );

        // 2. config endpoint used when env is unset or empty
        assert_eq!(
            resolve_otlp_endpoint(None, Some(&cfg)).as_deref(),
            Some("http://from-config:4318")
        );
        assert_eq!(
            resolve_otlp_endpoint(Some(""), Some(&cfg)).as_deref(),
            Some("http://from-config:4318")
        );

        // 3. malformed config treated as absent (warn + fall through)
        let mut bad = std::fs::File::create(&cfg).unwrap();
        writeln!(bad, "not [valid toml ====").unwrap();
        drop(bad);
        assert_eq!(resolve_otlp_endpoint(None, Some(&cfg)), None);

        // 4. missing file -> None (exporter default)
        assert_eq!(
            resolve_otlp_endpoint(None, Some(&dir.join("nope.toml"))),
            None
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
