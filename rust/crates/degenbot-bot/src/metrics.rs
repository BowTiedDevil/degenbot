//! OpenTelemetry **metrics** pipeline — runtime stats for the drain path.
//!
//! Traces (see [`crate::otel`]) answer "what happened in block N"; this module
//! answers "how are we doing over time": latency histograms, counters, and
//! gauges scraped by Prometheus and charted in Grafana. The exporter is a
//! pull-style Prometheus reader — the SDK pushes collected metrics into a
//! `prometheus::Registry` on scrape, and a tiny std-only HTTP listener serves
//! the text format (no async runtime, no extra HTTP stack).
//!
//! # Gating
//!
//! Behind the `otel` Cargo feature like [`crate::otel`] — default builds
//! compile zero metrics code.
//!
//! # Cardinality rule
//!
//! Metric labels must stay low-cardinality (`verdict`, `pool_type`,
//! `outcome`, `dry_run`, ...). NEVER label by `path_id`, pool id, or tx hash —
//! those belong in trace span fields (click from a dashboard anomaly to an
//! example trace), and unbounded label sets will explode the scraper.

use degenbot_core::{op_info, op_warn};
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use opentelemetry::metrics::MeterProvider as _;
use opentelemetry_sdk::error::OTelSdkError;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use thiserror::Error;

/// Instrumentation scope for every metric the bot records.
const METER_NAME: &str = "degenbot-bot";

/// Default scrape endpoint (Prometheus convention range, dev-safe bind).
pub const DEFAULT_METRICS_ADDR: &str = "127.0.0.1:9464";

/// Env var overriding the scrape endpoint.
const METRICS_ADDR_ENV: &str = "DEGENBOT_METRICS_ADDR";

/// Live distinct-series count, refreshed by every [`render`] (the scrape path)
/// and reported by the `degenbot.metric_series` observable gauge (ADR-043 §9).
///
/// The gauge callback MUST NOT call [`prometheus::Registry::gather`] itself:
/// it runs inside the exporter's collect, so a nested gather re-enters the
/// registry lock and deadlocks the scrape. Reading this atomic keeps the
/// callback re-entrancy-free; the reported value lags one scrape.
static SERIES_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Errors from metrics initialization.
#[derive(Debug, Error)]
pub enum MetricsInitError {
    /// The Prometheus exporter could not be built.
    #[error("prometheus exporter build failed: {0}")]
    Exporter(#[from] OTelSdkError),
    /// The scrape endpoint could not be bound (address in use, no permission,
    /// bad [`METRICS_ADDR_ENV`] override).
    #[error("metrics scrape endpoint bind failed: {0}")]
    Bind(#[from] std::io::Error),
}

/// Build a meter provider whose readings land in the returned Prometheus
/// registry, stamped with the standard bot resource. Test seam AND production
/// builder — tests assert against the registry directly; the app path wraps
/// this in [`init_global_metrics`].
///
/// # Errors
///
/// Returns [`MetricsInitError::Exporter`] if the exporter cannot be built.
pub fn build_prometheus_provider(
) -> Result<(SdkMeterProvider, prometheus::Registry), MetricsInitError> {
    let registry = prometheus::Registry::new();
    let exporter = opentelemetry_prometheus::exporter()
        .with_registry(registry.clone())
        .build()?;
    let provider = SdkMeterProvider::builder()
        .with_resource(crate::otel::bot_resource())
        .with_reader(exporter)
        .build();
    install_metric_series_self_metric(&provider);
    Ok((provider, registry))
}

/// ADR-043 §9: expose the live distinct-series count so a cardinality blowup
/// is visible BEFORE the collector falls over.
///
/// The value is taken from the registry at scrape time and includes this
/// gauge's own series, so the floor is 1 and no threshold can be tuned to
/// zero. It is the metric-side twin of the log-volume gate: an unexpected jump
/// is a diff, not a surprise.
fn install_metric_series_self_metric(provider: &SdkMeterProvider) {
    let _self_metric = provider
        .meter(METER_NAME)
        .u64_observable_gauge("degenbot.metric_series")
        .with_description(
            "Live distinct Prometheus series count (cardinality twin, ADR-043 section 9)",
        )
        .with_callback(|observer| {
            observer.observe(SERIES_COUNT.load(Ordering::Relaxed), &[]);
        })
        .build();
}

/// Render a registry to the Prometheus text exposition format (what
/// `/metrics` serves and Grafana scrapes). Empty string if the encoder fails —
/// the registry only produces families it built itself, so this is defensive,
/// not a real path.
#[must_use]
pub fn render(registry: &prometheus::Registry) -> String {
    use prometheus::Encoder as _;
    let encoder = prometheus::TextEncoder::new();
    let mut buf = Vec::new();
    if encoder.encode(&registry.gather(), &mut buf).is_err() {
        return String::new();
    }
    let text = String::from_utf8(buf).unwrap_or_default();
    // ADR-043 §9: refresh the series-count self-metric from the scrape we just
    // rendered (sample lines are everything that is not a `#` comment).
    let series = text
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .count() as u64;
    SERIES_COUNT.store(series, Ordering::Relaxed);
    text
}

/// Serve `/metrics` until `stop` is set: one accept loop of blocking I/O on
/// the calling thread, no async runtime required. Every response closes the
/// connection (HTTP/1.1 `Connection: close`) so scrapes are stateless.
/// Non-`/metrics` paths get a 404. The loop checks `stop` per accepted
/// connection — set it, then open one final connection to wake the accept.
/// Blocks the calling thread; run it on a spawned thread (the env-driven
/// wrapper does; tests own their threads).
pub fn serve_on_listener(
    listener: &TcpListener,
    body: &(impl Fn() -> String + Send + Sync),
    stop: &AtomicBool,
) {
    for stream in listener.incoming() {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let Ok(mut stream) = stream else {
            continue;
        };
        let Some(path) = read_request_path(&mut stream) else {
            continue;
        };
        if path == "/metrics" {
            let payload = body();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            let _ = stream.write_all(response.as_bytes());
        } else {
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
        let _ = stream.flush();
    }
}

/// Read one HTTP request line and return the request path (`None` on garbage).
fn read_request_path(stream: &mut TcpStream) -> Option<String> {
    let mut buf = [0u8; 1024];
    let n = stream.read(&mut buf).ok()?;
    let line = std::str::from_utf8(&buf[..n]).ok()?.lines().next()?;
    let mut parts = line.split_whitespace();
    let _method = parts.next()?;
    parts.next().map(str::to_owned)
}

/// Resolve the scrape endpoint: `DEGENBOT_METRICS_ADDR` first, then the
/// default `127.0.0.1:9464`.
///
/// # Errors
///
/// Returns [`MetricsInitError::Bind`] with the parse error wrapped when the
/// override is not a valid socket address — a boot-time config fault, surfaced
/// at init rather than silently binding somewhere unexpected.
pub fn metrics_addr_from_env() -> Result<SocketAddr, MetricsInitError> {
    // KAHU5W: the address is a typed schema key (`telemetry.metrics_addr` /
    // `DEGENBOT_METRICS_ADDR`); the loader owns any env read.
    // KAHU5W: typed schema key `telemetry.metrics_addr`; the loader owns
    // the env read — no direct env access here.
    let raw = crate::bot_core::stance::config()
        .telemetry
        .metrics_addr
        .clone();
    raw.parse().map_err(|e| {
        MetricsInitError::Bind(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{METRICS_ADDR_ENV}={raw:?} is not a valid socket address: {e}"),
        ))
    })
}

/// Process-lifetime global: the provider keeps instrument storage alive; the
/// stop flag lets [`shutdown_global_metrics`] end the scrape thread.
static GLOBAL: OnceLock<GlobalMetrics> = OnceLock::new();
struct GlobalMetrics {
    provider: SdkMeterProvider,
    stop: std::sync::Arc<AtomicBool>,
}

/// Initialize the process-global metrics provider + scrape server. Idempotent:
/// a second call returns `Ok(())` unchanged (mirrors the Python path's
/// `OnceLock` subscriber discipline). The server thread runs for the process
/// lifetime; [`shutdown_global_metrics`] stops it and flushes.
///
/// # Errors
///
/// Returns [`MetricsInitError::Exporter`] if the exporter cannot be built, or
/// [`MetricsInitError::Bind`] if the scrape endpoint cannot be resolved/bound.
pub fn init_global_metrics() -> Result<(), MetricsInitError> {
    init_global_metrics_with_addr(metrics_addr_from_env()?)
}

/// [`init_global_metrics`] with an explicit scrape endpoint - for consumers
/// that resolve the address from their own config layer (the Python-driven
/// path: `DEGENBOT_METRICS_ADDR` env > `otel.metrics_addr` in config.toml >
/// default). Idempotent like the env variant.
///
/// # Errors
///
/// Same contract as [`init_global_metrics`].
pub fn init_global_metrics_with_addr(addr: SocketAddr) -> Result<(), MetricsInitError> {
    if GLOBAL.get().is_some() {
        return Ok(());
    }
    let (provider, registry) = build_prometheus_provider()?;
    let listener = TcpListener::bind(addr)?;
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let global = GlobalMetrics {
        provider,
        stop: std::sync::Arc::clone(&stop),
    };
    GLOBAL.get_or_init(|| global);

    // PE4FPM: self-register the scrape thread (before the spawn below).
    degenbot_core::worker_census::register(degenbot_core::worker_census::WorkerCensusEntry {
        resource: "metrics_scrape",
        kind: "std scrape-server thread (Prometheus text endpoint)",
        count: 1,
        thread_name: "degenbot-metrics",
        sizing:
            "exactly one (fixed; spawn failure degrades to the log — endpoint inactive, no abort)",
        binding: "pinned",
    });

    // Serve on a dedicated std thread — the scrape endpoint must not depend on
    // the bot's tokio runtime staying alive (or vice versa). A spawn failure
    // leaves metrics initialized but unscrapeable; that degrades to the log,
    // it does not abort the bot.
    let body_registry = registry;
    let body_stop = std::sync::Arc::clone(&stop);
    let spawned = std::thread::Builder::new()
        .name("degenbot-metrics".to_owned())
        .spawn(move || {
            let body = move || render(&body_registry);
            serve_on_listener(&listener, &body, &body_stop);
        });
    if spawned.is_err() {
        op_warn!(
            domain = pump,
            "metrics scrape thread spawn failed - endpoint inactive"
        );
        stop.store(true, Ordering::Relaxed);
    } else {
        op_info!(domain = pump, addr = %addr, "Prometheus metrics endpoint active");
    }
    Ok(())
}

/// Count a log record dropped by a bounded console queue (ADR-043 §6).
/// Renders as `degenbot.log_dropped_total{sink}` — a silent ceiling is not
/// acceptable, so the drop is observable. No-ops before
/// [`init_global_metrics`] (telemetry off), same discipline as every other
/// observation site.
pub fn record_log_drop(sink: &'static str) {
    static LOG_DROPS: OnceLock<Option<opentelemetry::metrics::Counter<u64>>> = OnceLock::new();
    let counter = LOG_DROPS.get_or_init(|| {
        try_global_meter().map(|meter| meter.u64_counter("degenbot.log_dropped_total").build())
    });
    if let Some(counter) = counter {
        counter.add(1, &[opentelemetry::KeyValue::new("sink", sink)]);
    }
}

/// The process-global meter for recording instruments, or `None` before
/// [`init_global_metrics`] has run. Boot order is init-then-record; `None`
/// here means the otel feature is compiled in but the gate was off, which
/// record sites treat as "telemetry disabled" — the branch is one atomic load
/// behind the `OnceLock`, cheap enough for hot-path observation sites.
#[must_use]
pub fn try_global_meter() -> Option<opentelemetry::metrics::Meter> {
    GLOBAL.get().map(|g| g.provider.meter(METER_NAME))
}

/// Flush the metrics provider WITHOUT stopping the scrape server.
///
/// The pre-teardown flush (ADR-043 §6): the OTLP exporter needs the tokio
/// runtime for its final batch, so a flush after runtime teardown exports
/// nothing. No-ops when init has not run.
pub fn flush_global_metrics() {
    if let Some(global) = GLOBAL.get() {
        let _ = global.provider.force_flush();
    }
}

/// Stop the scrape server and flush provider state (clean-shutdown path).
pub fn shutdown_global_metrics() {
    if let Some(global) = GLOBAL.get() {
        global.stop.store(true, Ordering::Relaxed);
        let _ = global.provider.force_flush();
        let _ = global.provider.shutdown();
    }
}
