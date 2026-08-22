//! OpenTelemetry tracer bullet — OTLP span export for the drain path.
//!
//! [OpenTelemetry](https://opentelemetry.io) gives the running bot the trace
//! view hotpath can't: per-block cause-and-effect across components, queryable
//! after the fact. The lasting instrumentation is the `tracing` spans on the
//! drain path (see `BlockPump::run_with_stream`'s instrument attribute);
//! this module only wires a subscriber-side layer + OTLP exporter onto them.
//!
//! # Gating
//!
//! The whole module is behind the `otel` Cargo feature — default builds
//! compile zero OpenTelemetry code (stronger than hotpath's no-op stubs).
//! Unlike the hotpath bullet there is **no env gate**: [`init_otel_tracing`]
//! is an explicit call site for pure-Rust consumers (this crate is a cdylib
//! with no Rust `main`; the Python-driven path adds the layer to its own
//! registry in `degenbot-python`, behind `DEGENBOT_OTEL=1` — epic KDUED5,
//! task K6PCKP).
//!
//! # Who may install the global subscriber
//!
//! [`init_otel_tracing`] composes `EnvFilter` + `fmt` + the `OTel` layer and
//! calls `try_init`. In a Python process the global subscriber is already
//! owned by `degenbot-python::python_log_layer::init_logging_subscriber`
//! (installed at `#[pymodule]` init, idempotent via its `OnceLock`), so
//! `try_init` fails and this function returns
//! [`OtelInitError::AlreadySetUp`] — by design, not a bug: on that path the
//! `OTel` layer must live inside the existing registry (K6PCKP).
//!
//! # Endpoint
//!
//! The OTLP/HTTP-protobuf exporter resolves its target the way the `OTel` spec
//! says `OTEL_EXPORTER_OTLP_*` env vars do: signal-specific endpoint first,
//! then `OTEL_EXPORTER_OTLP_ENDPOINT`, then `http://localhost:4318`
//! (`/v1/traces` appended). [`provider_from_endpoint`] is the code-configured
//! variant for pure-Rust consumers.

use std::sync::OnceLock;

#[cfg(feature = "otel")]
use opentelemetry_otlp::WithHttpConfig;

use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::resource::Resource;
use opentelemetry_sdk::trace::{SdkTracer, SdkTracerProvider, SpanExporter};
use thiserror::Error;

/// Service identity stamped on every exported span.
const SERVICE_NAME: &str = "degenbot-bot";

/// The standard bot resource: `service.name = "degenbot-bot"` +
/// `service.version = <workspace version>`, merged over the default detectors
/// (telemetry SDK info + `OTEL_RESOURCE_ATTRIBUTES` / `OTEL_SERVICE_NAME`
/// env).
///
/// Public so consumers composing their own registry (e.g. the `degenbot-python`
/// path, task K6PCKP) can share the exact identity, and so the resource is
/// directly testable (the SDK no longer stamps the resource per `SpanData`).
#[must_use]
pub fn bot_resource() -> Resource {
    Resource::builder()
        .with_service_name(SERVICE_NAME)
        .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
        .build()
}

/// Errors from [`init_otel_tracing`].
#[derive(Debug, Error)]
pub enum OtelInitError {
    /// The OTLP span exporter could not be built (unreachable config, bad
    /// endpoint scheme, ...).
    #[error("OTLP span exporter build failed: {0}")]
    ExporterBuild(#[from] opentelemetry_otlp::ExporterBuildError),
    /// A global tracing subscriber is already installed (typical:
    /// `degenbot-python`'s registry, installed at module init). The `OTel`
    /// layer must be added to that registry instead — see the module docs
    /// and epic KDUED5 task K6PCKP.
    #[error("{0}")]
    AlreadySetUp(&'static str),
}

/// Keeps the process-lifetime tracer provider alive (the batched exporter's
/// background thread lives while the provider does).
static HANDLE: OnceLock<OtelHandle> = OnceLock::new();

/// The process-lifetime handle installed by [`init_otel_tracing`], if any.
/// Exit paths (S53STH cooperative shutdown) flush + shut the provider down
/// through this instead of reaching for `process::exit`.
#[cfg(feature = "otel")]
#[must_use]
pub fn global_handle() -> Option<&'static OtelHandle> {
    HANDLE.get()
}

/// Process-lifetime handle into the `OTel` tracer provider created by
/// [`init_otel_tracing`]. Cloneable; cheap (the provider is internally
/// shared).
#[derive(Debug, Clone)]
#[must_use]
pub struct OtelHandle {
    provider: SdkTracerProvider,
}
impl OtelHandle {
    /// Create a handle owning the given provider (K6PCKP: the
    /// `degenbot-python` path stores one so its drainer can
    /// flush/kill it at shutdown).
    pub fn new(provider: SdkTracerProvider) -> Self {
        Self { provider }
    }

    /// Force any pending spans to the exporter (call before a clean
    /// shutdown; the batch processor otherwise flushes on its schedule).
    /// # Errors
    ///
    /// The flush can fail if the OTLP endpoint is unreachable or the
    /// provider has already been shut down.
    pub fn flush(&self) -> OTelSdkResult {
        self.provider.force_flush()
    }

    /// Shut the provider down (stops the batch processor thread; in-flight
    /// batches are flushed within the default timeout).
    /// # Errors
    ///
    /// The shutdown can fail if the final flush cannot be delivered in time.
    pub fn shutdown(&self) -> OTelSdkResult {
        self.provider.shutdown()
    }
}

/// Custom OTLP HTTP client: `reqwest::Client::new()` (no client-level total
/// timeout) plus an explicit per-request `tokio::time::timeout`.
///
/// WHY NOT THE DEFAULT: opentelemetry-otlp 0.32 builds its reqwest client
/// with `.timeout(d)` (a total request timeout). Under reqwest 0.13 that
/// combination fails INSTANTLY with a connect error when driven from a
/// current-thread tokio runtime - which is exactly what the SDK's
/// `BatchSpanProcessor(TokioCurrentThread)` uses. Every span export died
/// before a socket was opened, so Jaeger silently never received a single
/// trace from the bot (diagnosed 2026-08-22 via the `otel_jaeger_e2e` tests:
/// raw reqwest + multi-thread runtime both succeed; builder-timeout +
/// current-thread fails). A client without the builder timeout works on
/// both flavors, so we apply the deadline ourselves around `execute`.
#[derive(Debug, Clone)]
pub struct ReqwestClient {
    inner: reqwest::Client,
    timeout: std::time::Duration,
}

impl ReqwestClient {
    /// Client with the given per-request deadline (OTLP default 10s).
    #[must_use]
    pub fn new(timeout: std::time::Duration) -> Self {
        Self {
            inner: reqwest::Client::new(),
            timeout,
        }
    }
}

#[async_trait::async_trait]
impl opentelemetry_http::HttpClient for ReqwestClient {
    async fn send_bytes(
        &self,
        request: http::Request<opentelemetry_http::Bytes>,
    ) -> Result<
        opentelemetry_http::Response<opentelemetry_http::Bytes>,
        opentelemetry_http::HttpError,
    > {
        let request = request.try_into()?;
        let fut = async {
            let mut resp = self.inner.execute(request).await?;
            let status = resp.status();
            let headers = std::mem::take(resp.headers_mut());
            let body = resp.bytes().await?;
            let mut builder = http::Response::builder().status(status);
            for (k, v) in &headers {
                builder = builder.header(k, v);
            }
            builder
                .body(body)
                .map_err(|e| Box::<dyn std::error::Error + Send + Sync>::from(e.to_string()))
        };
        match tokio::time::timeout(self.timeout, fut).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(e)) => Err(opentelemetry_http::HttpError::from(e)),
            Err(_) => Err(opentelemetry_http::HttpError::from(Box::<
                dyn std::error::Error + Send + Sync,
            >::from(
                "OTLP export timed out",
            ))),
        }
    }
}

/// OTLP spec default request deadline (`OTEL_EXPORTER_OTLP_TIMEOUT`).
#[must_use]
pub fn default_otlp_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(10)
}

/// Build a tracer provider over an injectable span exporter, with the bot
/// resource and a batch span processor.
///
/// The exporter is the test seam (in-memory exporters in the integration
/// tests); production callers use [`provider_from_env_endpoint`] /
/// [`provider_from_endpoint`].
#[must_use]
pub fn provider_with_exporter<E>(exporter: E) -> (SdkTracerProvider, SdkTracer)
where
    E: SpanExporter + 'static,
{
    // The async-aware batch processor runs its worker on its own current-thread
    // tokio runtime. The plain `with_batch_exporter` processor instead calls the
    // exporter (which returns a future — the OTLP/HTTP client needs a tokio
    // reactor) from a bare std thread and panics with "there is no reactor
    // running".
    let provider = SdkTracerProvider::builder()
        .with_resource(bot_resource())
        .with_span_processor(
            opentelemetry_sdk::trace::span_processor_with_async_runtime::BatchSpanProcessor::builder(
                exporter,
                opentelemetry_sdk::runtime::TokioCurrentThread,
            )
            .build(),
        )
        .build();
    let tracer = provider.tracer(SERVICE_NAME);
    (provider, tracer)
}

/// Build the production tracer provider: OTLP/HTTP-protobuf exporter whose
/// endpoint follows the `OTel` spec env vars (`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`
/// > `OTEL_EXPORTER_OTLP_ENDPOINT` > `http://localhost:4318`).
/// # Errors
///
/// Returns [`OtelInitError::ExporterBuild`] if the OTLP HTTP exporter cannot
/// be built with the resolved (env-var-driven) endpoint.
pub fn provider_from_env_endpoint() -> Result<(SdkTracerProvider, SdkTracer), OtelInitError> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_http_client(ReqwestClient::new(default_otlp_timeout()))
        .build()?;
    Ok(provider_with_exporter(exporter))
}

/// Build the production tracer provider with an explicit OTLP/HTTP endpoint
/// (code-configured variant for pure-Rust consumers).
/// # Errors
///
/// Returns [`OtelInitError::ExporterBuild`] if the OTLP HTTP exporter cannot
/// be built for the given endpoint.
pub fn provider_from_endpoint(
    endpoint: &str,
) -> Result<(SdkTracerProvider, SdkTracer), OtelInitError> {
    use opentelemetry_otlp::WithExportConfig;
    // otlp 0.32 uses a programmatic endpoint AS-IS (no signal-path append;
    // only env-resolved endpoints get it), so append /v1/traces ourselves
    // when the caller passed a bare collector base URL.
    let full = if endpoint.contains("/v1/traces") {
        endpoint.to_owned()
    } else if endpoint.ends_with('/') {
        format!("{endpoint}v1/traces")
    } else {
        format!("{endpoint}/v1/traces")
    };
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(full)
        .with_http_client(ReqwestClient::new(default_otlp_timeout()))
        .build()?;
    Ok(provider_with_exporter(exporter))
}

/// The `OTel` span layer for an arbitrary `tracing_subscriber` registry.
///
/// Registry-agnostic on purpose: [`init_otel_tracing`] composes it onto a
/// fresh registry, and the Python-driven path (K6PCKP) composes it onto
/// `degenbot-python`'s own registry as a third layer.
#[must_use]
pub fn layer(
    tracer: SdkTracer,
) -> tracing_opentelemetry::OpenTelemetryLayer<tracing_subscriber::Registry, SdkTracer> {
    // Soak-2026-08-22 (v6): context activation MUST stay off. With it on,
    // `on_enter` pushes an `opentelemetry::ContextGuard` onto
    // tracing-opentelemetry's per-thread GUARD_STACK; at thread exit the stack
    // dtor drops those guards, and each guard's Drop touches opentelemetry's
    // OWN thread-local - already destroyed by then (destructor order across
    // the two crates' TLS vars is unspecified). Any thread that dies with a
    // leftover guard aborts: `AccessError` -> panic-in-dtor -> process abort.
    // Observed on CPython ThreadPoolExecutor workers during mass V4
    // registration (faulthandler + addr2line symbolized the full chain).
    //
    // Disabling activation empties GUARD_STACK forever - the hazard class
    // cannot occur. Parentage is unaffected: contextual parents fall back to
    // the tracing span tree (`ctx.lookup_current()`), which is our model
    // anyway (JYCTXI explicit roots + MQUKB6 tree); nothing in this
    // workspace reads the ambient OTel context.
    tracing_opentelemetry::OpenTelemetryLayer::new(tracer).with_context_activation(false)
}

/// Install the global tracing subscriber for pure-Rust consumers:
/// `EnvFilter` (`RUST_LOG`) + `fmt` (stderr) + the `OTel` layer over the
/// env-endpoint exporter.
///
/// Returns [`OtelInitError::AlreadySetUp`] (and logs a `warn!`) when a
/// global subscriber already exists — in particular when `degenbot-python`
/// owns it. Call at most once per process; the returned handle is kept for
/// process-lifetime flush/shutdown.
/// # Errors
///
/// Returns [`OtelInitError::ExporterBuild`] if the OTLP HTTP exporter cannot
/// be built, or [`OtelInitError::AlreadySetUp`] when a global tracing
/// subscriber already exists (e.g. [`degenbot-python`] owns it).
pub fn init_otel_tracing() -> Result<OtelHandle, OtelInitError> {
    use tracing_subscriber::layer::{Layer as _, SubscriberExt};
    use tracing_subscriber::util::SubscriberInitExt;

    if HANDLE.get().is_some() {
        return Err(OtelInitError::AlreadySetUp(
            "init_otel_tracing was already called in this process",
        ));
    }
    let (provider, tracer) = provider_from_env_endpoint()?;
    let handle = OtelHandle::new(provider);
    // The OTel layer binds S=Registry, so it must sit directly on the bare
    // registry; fmt/EnvFilter compose on top (they are Layer for any
    // LookupSpan subscriber).
    // Console-vs-trace split (2026-08-22 audit): the OTel layer gets the
    // record-level RUST_LOG filter (uncapped); stderr fmt gets the console
    // filter with the diagnostic cap, so high-frequency `degenbot::diag`
    // events stay off stdout while remaining visible in traces.
    let mut console_filter = tracing_subscriber::EnvFilter::from_default_env();
    if let Ok(cap) = crate::telemetry::DIAGNOSTIC_CONSOLE_CAP_DIRECTIVE.parse() {
        console_filter = console_filter.add_directive(cap);
    }
    let subscriber = tracing_subscriber::registry()
        .with(layer(tracer).with_filter(tracing_subscriber::EnvFilter::from_default_env()))
        .with(tracing_subscriber::fmt::layer().with_filter(console_filter));
    if subscriber.try_init().is_err() {
        tracing::warn!("OTel init: a global tracing subscriber already exists (e.g. degenbot-python's registry); the OTel layer must be added to that registry (epic KDUED5 task K6PCKP)");
        return Err(OtelInitError::AlreadySetUp(
            "a global tracing subscriber is already installed",
        ));
    }
    HANDLE.get_or_init(|| handle.clone());
    Ok(handle)
}

#[cfg(test)]
mod runtime_tests {
    use super::*;
    use opentelemetry::trace::Tracer;
    use std::future::Future;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// Records whether it ran with a tokio runtime context present -
    /// exactly what the OTLP/HTTP client (reqwest/hyper) needs at export
    /// time, or the batch-processor thread panics with "there is no
    /// reactor running".
    #[derive(Clone, Debug)]
    struct ProbeExporter {
        saw_runtime: Arc<AtomicBool>,
    }

    impl SpanExporter for ProbeExporter {
        fn export(
            &self,
            _batch: Vec<opentelemetry_sdk::trace::SpanData>,
        ) -> impl Future<Output = OTelSdkResult> + Send {
            let saw = Arc::clone(&self.saw_runtime);
            async move {
                saw.store(
                    tokio::runtime::Handle::try_current().is_ok(),
                    Ordering::SeqCst,
                );
                Ok(())
            }
        }
    }

    #[test]
    fn batch_processor_export_runs_inside_a_tokio_context() {
        let saw = Arc::new(AtomicBool::new(false));
        let (provider, tracer) = provider_with_exporter(ProbeExporter {
            saw_runtime: Arc::clone(&saw),
        });
        let span = tracer.start("probe.span");
        drop(span);
        let _ = provider.force_flush();
        assert!(
            saw.load(Ordering::SeqCst),
            "the span processor must drive exports inside a tokio runtime
               context (the OTLP HTTP client panics without one: no
               reactor running)"
        );
    }
}
