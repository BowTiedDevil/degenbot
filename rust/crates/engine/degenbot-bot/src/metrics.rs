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
//!
//! # Lifecycle
//!
//! One pipeline per process with one explicit phase ([`Phase`]). Two
//! consequences of that explicitness are load-bearing:
//!
//! - A stopped pipeline grants no meter. An instrument recording into a
//!   shut-down provider has no reader left to drain it, so post-stop
//!   observation sites must see `None` rather than collect into a dead
//!   registry.
//! - A second init reports the endpoint already bound instead of binding a
//!   second listener, and init after a stop is refused rather than pretending
//!   to restart a pipeline whose instrument sets are already out.
//!
//! The scrape endpoint is the one part that may fail alone: an unspawnable
//! serve thread leaves the meter live and the endpoint inactive, and that
//! failure is named ([`Endpoint::Unserved`]) rather than swallowed.

use degenbot_core::{op_info, op_warn};
use parking_lot::RwLock;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

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
    /// The pipeline was stopped, so there is no live provider left to record
    /// into. Terminal: [`shutdown_global_metrics`] never leaves a restartable
    /// pipeline behind.
    #[error("metrics pipeline is stopped ({reason}) and is never restarted")]
    Stopped { reason: StopReason },
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
    // the address is a typed schema key (`telemetry.metrics_addr` /
    // `DEGENBOT_METRICS_ADDR`); the loader owns any env read.
    // typed schema key `telemetry.metrics_addr`; the loader owns
    // the env read — no direct env access here.
    let raw = degenbot_substrate::stance::config()
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

/// Why the pipeline stopped. A closed set: a new teardown path names itself
/// here, because this reason is what a refused init reports back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// [`shutdown_global_metrics`] ran: the serve loop was signalled and the
    /// provider flushed and shut down.
    Shutdown,
}

impl std::fmt::Display for StopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shutdown => f.write_str("shutdown"),
        }
    }
}

/// Where `/metrics` is served — or why the registry cannot be scraped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// The serve loop is up on this bound address; a `:0` request reports the
    /// port the kernel resolved.
    Serving(SocketAddr),
    /// The meter is live but the serve thread never spawned, so nothing serves
    /// the registry. Named rather than silent: recordings still land in the
    /// registry, and the caller can log the fault.
    Unserved {
        /// The address nothing is bound to.
        requested: SocketAddr,
        /// The spawn failure.
        fault: String,
    },
}

impl Endpoint {
    /// The address this endpoint serves, or was asked to serve.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        match self {
            Self::Serving(addr) => *addr,
            Self::Unserved { requested, .. } => *requested,
        }
    }
}

/// The pipeline phase — the single source of truth for "does a meter exist?".
///
/// [`Self::NotStarted`] and [`Self::Stopped`] both answer no; they differ in
/// retry. Only a stopped pipeline refuses one: its provider was flushed and
/// shut down, and every instrument set built from it is already in scrapers'
/// hands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Nothing built yet.
    NotStarted,
    /// The meter is live; `endpoint` says whether it is scrapeable.
    Running { endpoint: Endpoint },
    /// Terminal.
    Stopped { reason: StopReason },
}

/// What an init attempt must do about the current phase.
///
/// This is the whole transition table: one pure decision — no sockets, no
/// provider, no globals — which is what makes it unit-testable directly
/// instead of through the process-global (whose lifecycle a test can only walk
/// once per process).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitDecision {
    /// Nothing is running: build the provider and bind `requested`.
    Start { requested: SocketAddr },
    /// The pipeline is live; report where and change nothing.
    AlreadyRunning { endpoint: Endpoint },
    /// Refused: the pipeline was stopped.
    Refused { reason: StopReason },
}

impl Phase {
    /// Decide what an init call does, from the phase alone.
    #[must_use]
    pub fn decide_init(&self, requested: SocketAddr) -> InitDecision {
        match self {
            Self::NotStarted => InitDecision::Start { requested },
            Self::Running { endpoint } => InitDecision::AlreadyRunning {
                endpoint: endpoint.clone(),
            },
            Self::Stopped { reason } => InitDecision::Refused { reason: *reason },
        }
    }
}

/// What an init call did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetricsInit {
    /// This call built the pipeline.
    Started {
        /// The scrape posture — [`Endpoint::Serving`] on the happy path.
        endpoint: Endpoint,
    },
    /// An earlier call owns the pipeline, which is already bound here. A `:0`
    /// request reports that bound address, not the zero it asked for.
    AlreadyRunning {
        /// The posture the running pipeline reports.
        endpoint: Endpoint,
    },
}

impl MetricsInit {
    /// The scrape posture, whichever arm this is.
    #[must_use]
    pub fn endpoint(&self) -> &Endpoint {
        match self {
            Self::Started { endpoint } | Self::AlreadyRunning { endpoint } => endpoint,
        }
    }
}

/// The live halves of a `Running` phase.
struct Running {
    endpoint: Endpoint,
    /// The provider every instrument set is built from. It lives inside the
    /// phase that grants the meter, so a phase which is not `Running` cannot
    /// hand one out.
    provider: SdkMeterProvider,
    /// Signs the serve loop out. Allocated even when no loop was spawned: a
    /// shutdown then stores into a flag nobody reads, instead of tracking which
    /// `endpoint` arm owns a loop.
    stop: Arc<AtomicBool>,
}

/// The process-global cell's state. [`GlobalMetrics`]'s private transitions are
/// its only writers, so the provider and the phase move together.
enum State {
    NotStarted,
    Running(Running),
    Stopped { reason: StopReason },
}

impl State {
    /// The reportable phase.
    fn phase(&self) -> Phase {
        match self {
            Self::NotStarted => Phase::NotStarted,
            Self::Running(running) => Phase::Running {
                endpoint: running.endpoint.clone(),
            },
            Self::Stopped { reason } => Phase::Stopped { reason: *reason },
        }
    }

    /// Retire the pipeline. Only `Running` owns resources to stop; `NotStarted`
    /// passes through so a stop before any init leaves a later init startable,
    /// and a second `Stopped` keeps the reason it stopped for.
    fn stopped(self, reason: StopReason) -> (Self, Option<Running>) {
        match self {
            Self::Running(running) => (Self::Stopped { reason }, Some(running)),
            other => (other, None),
        }
    }
}

/// `NotStarted` is what [`GlobalMetrics::stop`] swaps out of the lock, so the
/// live resources can be taken without cloning them.
impl Default for State {
    fn default() -> Self {
        Self::NotStarted
    }
}

/// The scrape-thread spawner. Injected so the spawn-failure arm is reachable in
/// a test that cannot exhaust the process's thread limit:
/// [`GlobalMetrics::init`] passes [`spawn_scrape_server`], tests pass a failing
/// stub.
type ScrapeSpawner = fn(TcpListener, prometheus::Registry, Arc<AtomicBool>) -> std::io::Result<()>;

/// Run [`serve_on_listener`] on a dedicated std thread.
///
/// The scrape endpoint must not depend on the bot's tokio runtime staying alive
/// (or vice versa). A spawn failure leaves the meter live and the endpoint
/// inactive — a degradation the caller reports, never an abort.
fn spawn_scrape_server(
    listener: TcpListener,
    registry: prometheus::Registry,
    stop: Arc<AtomicBool>,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("degenbot-metrics".to_owned())
        .spawn(move || {
            let body = move || render(&registry);
            serve_on_listener(&listener, &body, &stop);
        })
        .map(drop)
}

/// One metrics pipeline. The process-global is a single instance of it; tests
/// own instances of their own, which is why the lifecycle contract below needs
/// no process-wide test serialization.
struct GlobalMetrics {
    state: RwLock<State>,
}

impl GlobalMetrics {
    const fn new() -> Self {
        Self {
            state: RwLock::new(State::NotStarted),
        }
    }

    fn phase(&self) -> Phase {
        self.state.read().phase()
    }

    fn is_running(&self) -> bool {
        matches!(*self.state.read(), State::Running(_))
    }

    fn meter(&self) -> Option<opentelemetry::metrics::Meter> {
        match &*self.state.read() {
            State::Running(running) => Some(running.provider.meter(METER_NAME)),
            State::NotStarted | State::Stopped { .. } => None,
        }
    }

    fn flush(&self) {
        if let State::Running(running) = &*self.state.read() {
            let _ = running.provider.force_flush();
        }
    }

    /// The init transition.
    ///
    /// One write lock spans decide-and-start, so two racing calls cannot bind
    /// twice. Nothing is logged inside it: an `op_*` event can reach
    /// [`record_log_drop`], which takes a read of this same lock.
    fn init(&self, addr: SocketAddr) -> Result<MetricsInit, MetricsInitError> {
        self.init_with(addr, spawn_scrape_server)
    }

    /// [`Self::init`] with the serve-thread spawn injected — the seam that makes
    /// the spawn-failure arm testable.
    fn init_with(
        &self,
        addr: SocketAddr,
        spawn: ScrapeSpawner,
    ) -> Result<MetricsInit, MetricsInitError> {
        let mut state = self.state.write();
        match state.phase().decide_init(addr) {
            InitDecision::AlreadyRunning { endpoint } => {
                Ok(MetricsInit::AlreadyRunning { endpoint })
            }
            InitDecision::Refused { reason } => Err(MetricsInitError::Stopped { reason }),
            InitDecision::Start { requested } => {
                // Both failures return before the phase is touched: nothing was
                // published, so a retry after a transient exporter or bind fault
                // starts cleanly.
                let (provider, registry) = build_prometheus_provider()?;
                let listener = TcpListener::bind(requested)?;
                let bound = listener.local_addr()?;
                let stop = Arc::new(AtomicBool::new(false));
                let endpoint = match spawn(listener, registry, Arc::clone(&stop)) {
                    Ok(()) => Endpoint::Serving(bound),
                    Err(e) => Endpoint::Unserved {
                        requested: bound,
                        fault: e.to_string(),
                    },
                };
                *state = State::Running(Running {
                    endpoint: endpoint.clone(),
                    provider,
                    stop,
                });
                Ok(MetricsInit::Started { endpoint })
            }
        }
    }

    /// Stop: sign the serve loop out, flush, shut the provider down, and let the
    /// meter go `None`.
    ///
    /// The teardown runs after the lock is released — the SDK takes its own
    /// locks, and the phase transition is already published.
    fn stop(&self, reason: StopReason) {
        let retired = {
            let mut state = self.state.write();
            let (next, retired) = std::mem::take(&mut *state).stopped(reason);
            *state = next;
            retired
        };
        let Some(running) = retired else {
            return;
        };
        running.stop.store(true, Ordering::Relaxed);
        let _ = running.provider.force_flush();
        let _ = running.provider.shutdown();
    }
}

/// The process-global pipeline: one instance, so a second init anywhere in the
/// process reports the endpoint already bound instead of binding another.
static GLOBAL: GlobalMetrics = GlobalMetrics::new();

/// The boot side effects that ride a *started* pipeline. They are not part of
/// the init transition because both re-enter the record gate (the census
/// export hook resolves the instrument set, which reads the phase), so they
/// must run outside its write lock.
fn on_pipeline_started() {
    // The updater telemetry port rides the metrics boot: the port exists iff
    // the meter its hooks forward to can exist (one install site; the hooks
    // resolve the instrument set lazily, so install order is irrelevant —
    // the same discipline as the substrate port install at shell boot).
    crate::instruments::install_updater_telemetry_port();

    degenbot_core::worker_census::register(degenbot_core::worker_census::WorkerCensusEntry {
        resource: "metrics_scrape",
        kind: "std scrape-server thread (Prometheus text endpoint)",
        count: 1,
        thread_name: "degenbot-metrics",
        sizing:
            "exactly one (fixed; spawn failure degrades to the log — endpoint inactive, no abort)",
        binding: "pinned",
    });
}

/// Initialize the process-global metrics provider + scrape server.
///
/// Idempotent by reporting, not by ignoring: a second call returns
/// [`MetricsInit::AlreadyRunning`] carrying the address already bound and binds
/// nothing new. The serve thread runs for the process lifetime;
/// [`shutdown_global_metrics`] stops it and flushes.
///
/// # Errors
///
/// [`MetricsInitError::Exporter`] if the exporter cannot be built,
/// [`MetricsInitError::Bind`] if the scrape endpoint cannot be
/// resolved/bound, or [`MetricsInitError::Stopped`] once the pipeline was
/// stopped.
pub fn init_global_metrics() -> Result<MetricsInit, MetricsInitError> {
    init_global_metrics_with_addr(metrics_addr_from_env()?)
}

/// [`init_global_metrics`] with an explicit scrape endpoint — for consumers
/// that resolve the address from their own config layer (the Python-driven
/// path: `DEGENBOT_METRICS_ADDR` env > `otel.metrics_addr` in config.toml >
/// default). Same idempotence and stop contract as the env variant.
///
/// # Errors
///
/// Same contract as [`init_global_metrics`].
pub fn init_global_metrics_with_addr(addr: SocketAddr) -> Result<MetricsInit, MetricsInitError> {
    let outcome = GLOBAL.init(addr)?;
    match &outcome {
        MetricsInit::Started { endpoint } => {
            on_pipeline_started();
            match endpoint {
                Endpoint::Serving(bound) => op_info!(
                    domain = pump,
                    addr = %bound,
                    "Prometheus metrics endpoint active"
                ),
                Endpoint::Unserved { requested, fault } => op_warn!(
                    domain = pump,
                    addr = %requested,
                    fault = %fault,
                    "metrics scrape thread spawn failed - endpoint inactive, meter still recording"
                ),
            }
        }
        MetricsInit::AlreadyRunning { endpoint } => op_info!(
            domain = pump,
            addr = %endpoint.addr(),
            "Prometheus metrics endpoint already running"
        ),
    }
    Ok(outcome)
}

/// The drop counter behind [`record_log_drop`], built once from the running
/// pipeline's meter and reused for every drop after it.
static LOG_DROPS: OnceLock<Option<opentelemetry::metrics::Counter<u64>>> = OnceLock::new();

/// Count a log record dropped by a bounded console queue (ADR-043 §6).
/// Renders as `degenbot.log_dropped_total{sink}` — a silent ceiling is not
/// acceptable, so the drop is observable. No-ops while no pipeline is running
/// (telemetry off, or already stopped), same discipline as every other
/// observation site.
pub fn record_log_drop(sink: &'static str) {
    if !is_running() {
        return;
    }
    let counter = LOG_DROPS.get_or_init(|| {
        try_global_meter().map(|meter| meter.u64_counter("degenbot.log_dropped_total").build())
    });
    if let Some(counter) = counter {
        counter.add(1, &[opentelemetry::KeyValue::new("sink", sink)]);
    }
}

/// The process-global meter for recording instruments, or `None` while no
/// pipeline is running — before [`init_global_metrics`] and after
/// [`shutdown_global_metrics`].
///
/// Record sites treat `None` as "telemetry disabled". The branch is one
/// `RwLock` read, cheap enough for hot-path observation sites, and it is what
/// keeps a stopped pipeline from collecting into a provider whose reader is
/// gone.
#[must_use]
pub fn try_global_meter() -> Option<opentelemetry::metrics::Meter> {
    GLOBAL.meter()
}

/// Whether a meter exists right now — the record-site gate the instrument
/// lookup consults before it hands out its lazy instrument set.
#[must_use]
pub fn is_running() -> bool {
    GLOBAL.is_running()
}

/// The current phase: the endpoint a running pipeline bound (or why it is
/// unscrapeable), or the reason it stopped.
#[must_use]
pub fn phase() -> Phase {
    GLOBAL.phase()
}

/// Flush the metrics provider WITHOUT stopping the scrape server.
///
/// The pre-teardown flush (ADR-043 §6): the OTLP exporter needs the tokio
/// runtime for its final batch, so a flush after runtime teardown exports
/// nothing. No-ops while no pipeline is running.
pub fn flush_global_metrics() {
    GLOBAL.flush();
}

/// Stop the scrape server and flush + shut the provider down (clean-shutdown
/// path). Terminal: every later init is refused, and every later meter lookup
/// is `None`.
pub fn shutdown_global_metrics() {
    GLOBAL.stop(StopReason::Shutdown);
}

#[cfg(test)]
mod tests {
    //! The lifecycle contract against owned instances. The transition table is
    //! data, so nothing here touches the process-global — a global lifecycle can
    //! only be walked once per process, which is exactly the serialization an
    //! instance-scoped test avoids.

    #![expect(clippy::expect_used, clippy::panic)]

    use super::*;

    fn ephemeral() -> SocketAddr {
        "127.0.0.1:0".parse().expect("literal socket address")
    }

    fn literal(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    #[test]
    fn a_not_started_phase_asks_for_a_start() {
        let requested = literal(9464);
        assert_eq!(
            Phase::NotStarted.decide_init(requested),
            InitDecision::Start { requested }
        );
    }

    #[test]
    fn a_running_phase_reports_the_bound_address() {
        let bound = literal(9464);
        let phase = Phase::Running {
            endpoint: Endpoint::Serving(bound),
        };
        assert_eq!(
            phase.decide_init(ephemeral()),
            InitDecision::AlreadyRunning {
                endpoint: Endpoint::Serving(bound)
            },
            "a `:0` request must report the bound address, never the zero it asked for"
        );
    }

    #[test]
    fn a_stopped_phase_refuses_init() {
        let phase = Phase::Stopped {
            reason: StopReason::Shutdown,
        };
        assert_eq!(
            phase.decide_init(ephemeral()),
            InitDecision::Refused {
                reason: StopReason::Shutdown
            }
        );
    }

    #[test]
    fn an_unserved_endpoint_keeps_its_fault_when_it_reports_itself() {
        let endpoint = Endpoint::Unserved {
            requested: literal(9464),
            fault: "thread limit".to_owned(),
        };
        assert_eq!(endpoint.addr(), literal(9464));
        assert_eq!(
            Phase::Running {
                endpoint: endpoint.clone()
            }
            .decide_init(ephemeral()),
            InitDecision::AlreadyRunning { endpoint },
            "the named fault survives a repeated init"
        );
    }

    #[test]
    fn a_pipeline_serves_once_then_stops_for_good() {
        let pipeline = GlobalMetrics::new();
        assert!(!pipeline.is_running());
        assert!(pipeline.meter().is_none(), "nothing is live before init");

        let started = pipeline.init(ephemeral()).expect("a fresh pipeline starts");
        let MetricsInit::Started {
            endpoint: Endpoint::Serving(bound),
        } = started
        else {
            panic!("a fresh pipeline serves its scrape endpoint, got {started:?}");
        };
        assert_ne!(bound.port(), 0, "a `:0` bind reports the resolved port");
        assert!(pipeline.is_running());
        assert!(pipeline.meter().is_some());

        let second = pipeline
            .init(ephemeral())
            .expect("a second init reports instead of binding");
        assert_eq!(
            second,
            MetricsInit::AlreadyRunning {
                endpoint: Endpoint::Serving(bound)
            },
            "the second init reports the address already bound"
        );

        pipeline.flush();

        pipeline.stop(StopReason::Shutdown);
        assert_eq!(
            pipeline.phase(),
            Phase::Stopped {
                reason: StopReason::Shutdown
            }
        );
        assert!(!pipeline.is_running());
        assert!(
            pipeline.meter().is_none(),
            "a stopped pipeline grants no meter"
        );

        pipeline.stop(StopReason::Shutdown);
        pipeline.flush();
        assert!(
            matches!(
                pipeline.init(ephemeral()),
                Err(MetricsInitError::Stopped {
                    reason: StopReason::Shutdown
                })
            ),
            "init after a stop is refused"
        );

        let _ = TcpStream::connect(bound);
    }

    #[test]
    fn a_stop_before_any_init_leaves_the_pipeline_startable() {
        let pipeline = GlobalMetrics::new();
        pipeline.stop(StopReason::Shutdown);
        assert_eq!(
            pipeline.phase(),
            Phase::NotStarted,
            "a stop cannot fabricate a pipeline it never had"
        );

        let started = pipeline
            .init(ephemeral())
            .expect("a pipeline that never started still starts");
        assert!(matches!(started, MetricsInit::Started { .. }));
        pipeline.stop(StopReason::Shutdown);
    }

    #[test]
    fn a_bind_fault_leaves_the_pipeline_startable() {
        let pipeline = GlobalMetrics::new();
        let occupant = TcpListener::bind(ephemeral()).expect("bind the occupying listener");
        let taken = occupant.local_addr().expect("local_addr of the occupant");

        assert!(
            matches!(pipeline.init(taken), Err(MetricsInitError::Bind(_))),
            "the bind fault surfaces to the caller"
        );
        assert_eq!(
            pipeline.phase(),
            Phase::NotStarted,
            "a bind fault publishes nothing"
        );
        assert!(pipeline.meter().is_none());

        let started = pipeline
            .init(ephemeral())
            .expect("a retry after a bind fault starts");
        let MetricsInit::Started {
            endpoint: Endpoint::Serving(bound),
        } = started
        else {
            panic!("a retry serves, got {started:?}");
        };
        pipeline.stop(StopReason::Shutdown);
        let _ = TcpStream::connect(bound);
    }

    #[test]
    fn a_scrape_spawn_fault_is_named_and_leaves_the_meter_live() {
        let pipeline = GlobalMetrics::new();
        let started = pipeline
            .init_with(ephemeral(), |_listener, _registry, _stop| {
                Err(std::io::Error::other("no threads left"))
            })
            .expect("the provider is usable without a scrape thread");

        let MetricsInit::Started { endpoint } = started else {
            panic!("a fresh pipeline starts");
        };
        let Endpoint::Unserved { requested, fault } = &endpoint else {
            panic!("a spawn fault must be named, got {endpoint:?}");
        };
        assert_ne!(requested.port(), 0, "the bound address is reported");
        assert!(fault.contains("no threads left"), "got: {fault}");
        assert!(pipeline.is_running(), "the meter stays live");
        assert!(pipeline.meter().is_some());

        pipeline.stop(StopReason::Shutdown);
        assert!(
            pipeline.meter().is_none(),
            "a stopped pipeline grants no meter"
        );
    }
}
