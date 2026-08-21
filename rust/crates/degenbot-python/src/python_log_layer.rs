//! Tracing subscriber layer that forwards events to Python `logging`.
//!
//! Replaces `pyo3_log::init()` (per-record `Python::attach`) with a
//! batched-drain pattern: events are pushed onto a bounded
//! [`ArrayQueue`](crossbeam_queue::ArrayQueue) from any thread (no GIL
//! needed), and a dedicated OS thread drains the queue, batching up to 256
//! records or every 50 ms, then forwarding the batch to Python `logging`
//! via ONE `Python::attach` per flush.
//!
//! This makes the pump's tokio workers GIL-free for logging: the per-record
//! GIL round-trip is replaced by a lock-free queue push.
//!
//! # Shutdown
//!
//! Call [`shutdown_log_drainer`] to stop the drainer thread and flush
//! remaining records. The Python driver should call this before interpreter
//! finalization (e.g. in `__aexit__` or via Python `atexit`).

use std::sync::atomic::{AtomicBool, Ordering};
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

/// Maximum number of log records buffered before dropping oldest.
const QUEUE_CAPACITY: usize = 1024;

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

/// Shared state between the tracing [`Layer`] and the drainer thread.
struct PythonLogLayerState {
    /// Bounded, lock-free queue shared with the drainer thread.
    queue: ArrayQueue<PythonLogRecord>,
    /// Set to `true` to signal the drainer thread to shut down.
    shutdown: AtomicBool,
}

/// A [`tracing_subscriber::Layer`] that forwards events to Python logging
/// via a batched, GIL-free channel.
///
/// Events are formatted and pushed onto a bounded queue. A dedicated OS
/// thread drains the queue and flushes batches to Python via one
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
            queue: ArrayQueue::new(QUEUE_CAPACITY),
            shutdown: AtomicBool::new(false),
        });
        let drainer_state = Arc::clone(&state);
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

        // Push onto the queue; drop oldest on overflow.
        if let Err(dropped) = self.state.queue.push(record) {
            // Queue is full — the record is dropped. The dropped oldest
            // record from the `push` return is not forwarded, but since
            // this is a diagnostic path, dropping is acceptable.
            let _ = dropped;
        }
    }
}

/// Extract the original `log::` target from a `LogTracer`-bridged event.
///
/// When `tracing_log::LogTracer` bridges a `log::` record, the tracing event
/// has `target = "log"` and the original log target is stored in the
/// `log.target` field. This function visits the event fields and returns
/// the original target if found.
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
fn flush_batch_to_python(records: &[PythonLogRecord]) {
    Python::attach(|py| {
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
    // K6PCKP: flush + kill the OTel provider (None-safe when the
    // DEGENBOT_OTEL env gate was off).
    #[cfg(feature = "otel")]
    if let Some(handle) = OTEL_PROVIDER.get() {
        let _ = handle.flush();
        let _ = handle.shutdown();
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

/// The default tracing `EnvFilter` when `RUST_LOG` is unset.
///
/// Defaults to `info` globally, EXCEPT the alloy-internal transport/network
/// crates are throttled to `warn`. alloy emits routine lifecycle INFO from its
/// internal pubsub/transport services (e.g. ``INFO alloy_pubsub::service:
/// Pubsub service request channel closed. Shutting down.`` on a clean provider
/// teardown) which is third-party noise on the Python-driven log / stderr
/// stream, not a degenbot-originated diagnostic. Their WARN/ERROR records
/// still pass (a `warn` directive enables ERROR and WARN — those signal real
/// connection failures); only INFO/DEBUG/TRACE are dropped. degenbot's own
/// `degenbot_*` targets are untouched. An explicit `RUST_LOG` overrides this
/// default entirely.
fn default_env_filter() -> EnvFilter {
    let mut filter = EnvFilter::new("info");
    for target in [
        "alloy_pubsub",
        "alloy_transport",
        "alloy_transport_ws",
        "alloy_transport_ipc",
        "alloy_transport_http",
        "alloy_provider",
        "alloy_rpc",
        "alloy_network",
        "alloy_contract",
        "tungstenite",
    ] {
        // Directives are static and always valid; parse defensively (an
        // unrecognized directive would just leave the target unfiltered rather
        // than abort subscriber setup).
        if let Ok(directive) = format!("{target}=warn").parse() {
            filter = filter.add_directive(directive);
        }
    }
    filter
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
                .with_writer(tracing_subscriber::fmt::writer::BoxMakeWriter::new(
                    std::io::stderr,
                ))
                .with_ansi(true),
        )
        .with(python_layer)
}

#[cfg(feature = "otel")]
type MiddleRegistryOtel<L> = Layered<
    tracing_subscriber::fmt::Layer<
        Layered<EnvFilter, Layered<L, Registry>>,
        tracing_subscriber::fmt::format::DefaultFields,
        tracing_subscriber::fmt::format::Format<tracing_subscriber::fmt::format::Full>,
        tracing_subscriber::fmt::writer::BoxMakeWriter,
    >,
    Layered<EnvFilter, Layered<L, Registry>>,
>;

#[cfg(feature = "otel")]
type BaseRegistryOtel<P, L> = Layered<P, MiddleRegistryOtel<L>>;

/// Assembles the base registry with the `OTel` span layer pinned at the
/// bottom of the stack (the SDK layer can only layer onto the bare
/// Registry): `OTel + EnvFilter + fmt + the Python slot`.
#[must_use]
#[cfg(feature = "otel")]
#[cfg(feature = "otel")]
pub(crate) fn build_base_registry_with_otel<P, L>(
    env_filter: EnvFilter,
    otel_layer: L,
    python_layer: P,
) -> BaseRegistryOtel<P, L>
where
    P: Layer<MiddleRegistryOtel<L>> + 'static,
    L: Layer<Registry> + 'static,
{
    use tracing_subscriber::layer::SubscriberExt;
    tracing_subscriber::registry()
        .with(otel_layer)
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(tracing_subscriber::fmt::writer::BoxMakeWriter::new(
                    std::io::stderr,
                ))
                .with_ansi(true),
        )
        .with(python_layer)
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

        // Build the subscriber registry.
        // `EnvFilter` controls which events each layer receives. Use
        // `RUST_LOG` if set, otherwise default to `info` (matching pyo3-log's
        // unconditional forwarding — Python `logging` handles its own
        // per-logger level filtering).
        let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| default_env_filter());

        #[cfg(feature = "otel")]
        {
            // K6PCKP: OTel OTLP span layer. Import-time env gate (same
            // rationale as DEGENBOT_HOTPATH): the pymodule init is the one
            // justified implicit call site. Layered third on the base
            // registry; the provider lives in OTEL_PROVIDER for
            // `shutdown_log_drainer` flush/kill.
            let otel_layer = if let Ok("1") = std::env::var("DEGENBOT_OTEL").as_deref() { match degenbot_bot::otel::provider_from_env_endpoint() {
                Ok((provider, tracer)) => {
                    let _ = OTEL_PROVIDER.set(degenbot_bot::otel::OtelHandle::new(provider));
                    tracing::info!("OTel OTLP span layer active (endpoint from OTEL_EXPORTER_OTLP_* or http://localhost:4318)");
                    Some(degenbot_bot::otel::layer(tracer))
                }
                Err(e) => {
                    tracing::warn!(error = %e, "OTel layer disabled: OTLP exporter build failed");
                    None
                }
            } } else {
                tracing::debug!("OTel span layer inactive (set DEGENBOT_OTEL=1 to enable)");
                None
            };
            match otel_layer {
                Some(ol) => set_global_subscriber(build_base_registry_with_otel(
                    env_filter,
                    ol,
                    python_layer,
                )),
                None => set_global_subscriber(build_base_registry(env_filter, python_layer)),
            }
        }

        #[cfg(not(feature = "otel"))]
        {
            set_global_subscriber(build_base_registry(env_filter, python_layer));
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

// clippy --all-targets gate (UX66EM): this test module uses unwrap but no
// expect, so expecting expect_used here is an unfulfilled-lint-expectation.
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
            .with(default_env_filter())
            .with(capture.clone());
        let ret = tracing::dispatcher::with_default(&tracing::Dispatch::new(subscriber), f);
        let records = capture.0.lock().unwrap().clone();
        (ret, records)
    }
    /// K6PCKP seam C: the "`OTel`" layer composes onto the base logging
    /// registry (`EnvFilter` + fmt + the Python slot) and receives spans
    /// without a "`Python::attach"-capable` drainer thread. A "Capture"
    /// stand-in plays the Python slot (the real drainer needs the
    /// interpreter); this also proves both layers live in one registry.
    /// The global slot is per-process and no other lib test sets one,
    /// so this test owns it deterministically (repo convention).
    #[cfg(feature = "otel")]
    #[test]
    fn otel_layer_composes_into_the_base_registry() {
        use degenbot_bot::otel;
        use opentelemetry_sdk::trace::InMemorySpanExporter;

        let exporter = InMemorySpanExporter::default();
        let (provider, tracer) = otel::provider_with_exporter(exporter.clone());

        let capture = Capture::default();
        let subscriber = build_base_registry_with_otel(
            EnvFilter::new("info"),
            otel::layer(tracer),
            capture.clone(),
        );
        tracing::subscriber::set_global_default(subscriber)
            .expect("this test owns the lib-binary global slot");

        tracing::info_span!("seam.c.span").in_scope(|| {
            tracing::info!(target: "degenbot_bot::bot_core::block_pump", "seam c event");
        });
        provider.force_flush().expect("flush");

        let spans = exporter.get_finished_spans().expect("spans");
        assert!(
            spans.iter().any(|sp| sp.name.as_ref() == "seam.c.span"),
            "seam.c.span missing from the OTel exporter; got {:?}",
            spans.iter().map(|sp| sp.name.as_ref()).collect::<Vec<_>>()
        );
        let records = capture.0.lock().expect("test lock").clone();
        assert!(
            records
                .iter()
                .any(|(target, level)| { *level == Level::INFO && target.contains("block_pump") }),
            "event did not reach the Python-slot layer; records: {records:?}"
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
}
