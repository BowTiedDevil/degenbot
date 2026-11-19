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
//! registry in `degenbot-python`, behind `DEGENBOT_OTEL=1`  — the
//! Python path composes it the same way).
//!
//! # Who may install the global subscriber
//!
//! [`init_otel_tracing`] composes `EnvFilter` + `fmt` + the `OTel` layer and
//! calls `try_init`. In a Python process the global subscriber is already
//! owned by `degenbot-python::python_log_layer::init_logging_subscriber`
//! (installed at `#[pymodule]` init, idempotent via its `OnceLock`), so
//! `try_init` fails and this function returns
//! [`OtelInitError::AlreadySetUp`] — by design, not a bug: on that path the
//! `OTel` layer must live inside the existing registry.
//!
//! # Endpoint
//!
//! The OTLP/HTTP-protobuf exporter resolves its target the way the `OTel` spec
//! says `OTEL_EXPORTER_OTLP_*` env vars do: signal-specific endpoint first,
//! then `OTEL_EXPORTER_OTLP_ENDPOINT`, then `http://localhost:4318`
//! (`/v1/traces` appended). [`provider_from_endpoint`] is the code-configured
//! variant for pure-Rust consumers.
//!
//! # Client and request deadline
//!
//! The exporter owns its HTTP client and its deadline. Stock configuration
//! resolves the timeout the way the `OTel` spec does
//! (`OTEL_EXPORTER_OTLP_TRACES_TIMEOUT`, then `OTEL_EXPORTER_OTLP_TIMEOUT`,
//! then 10 s) and applies it to that client and to the export's retry budget,
//! so both entry points below are pure configuration. Delivery through the
//! stock client is pinned on both driver runtime flavors by
//! `tests/otel_export_runtime_matrix.rs`.

use degenbot_core::op_warn;
use std::sync::OnceLock;

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
/// path) can share the exact identity, and so the resource is
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
    /// and the Python path.
    #[error("{0}")]
    AlreadySetUp(&'static str),
}

/// Keeps the process-lifetime tracer provider alive (the batched exporter's
/// background thread lives while the provider does).
static HANDLE: OnceLock<OtelHandle> = OnceLock::new();

/// The process-lifetime handle installed by [`init_otel_tracing`], if any.
/// Exit paths (cooperative shutdown) flush + shut the provider down
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
    /// Create a handle owning the given provider (the
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
    // Span-event cap — BOUNDED: the SDK default 128 truncates a busy solve
    // span, silently dropping the phase events emitted AFTER the per-path
    // activation flood. The cap can be bounded rather than `u32::MAX` because
    // the data that matters does not ride events: per-path economics ride
    // `degenbot.arb.path` child-span ATTRIBUTES and the phase aggregates
    // ride `degenbot.arb.{fanout,resolve,lpt,merge}` span attributes, so
    // event truncation can no longer lose phase data. 1024 bounds the only
    // remaining known pack — `[path] activated by dirty pool` events at
    // `RUST_LOG=degenbot::engine=debug` (hundreds of activations per busy
    // cycle) — without the unbounded-memory exposure of u32::MAX.
    let span_limits = opentelemetry_sdk::trace::SpanLimits {
        // Bounded headroom, not an unbounded cap (see the note above).
        max_events_per_span: 1024,
        ..opentelemetry_sdk::trace::SpanLimits::default()
    };
    let provider = SdkTracerProvider::builder()
        .with_span_limits(span_limits)
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
    // A programmatic endpoint is used AS-IS by the exporter (only
    // env-resolved endpoints get the signal path appended), so append
    // /v1/traces here when the caller passed a bare collector base URL.
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
        .build()?;
    Ok(provider_with_exporter(exporter))
}

/// The `OTel` span layer for an arbitrary `tracing_subscriber` registry.
///
/// Registry-agnostic on purpose: [`init_otel_tracing`] composes it onto a
/// fresh registry, and the Python-driven path composes it onto
/// `degenbot-python`'s own registry as a third layer.
#[must_use]
pub fn layer(
    tracer: SdkTracer,
) -> tracing_opentelemetry::OpenTelemetryLayer<tracing_subscriber::Registry, SdkTracer> {
    // Context activation MUST stay off. With it on,
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
    // anyway (explicit roots + tree); nothing in this
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
    // Console-vs-trace split: the OTel layer gets the
    // record-level RUST_LOG filter (uncapped); stderr fmt gets the console
    // filter with the diagnostic cap, so high-frequency `degenbot::diag`
    // events stay off stdout while remaining visible in traces.
    let plan = degenbot_core::telemetry::resolve_filters(
        degenbot_core::telemetry::CONSOLE_WIRING_DEFAULT_RUST,
    );
    let console_filter = tracing_subscriber::EnvFilter::new(&plan.console);
    let record_filter = tracing_subscriber::EnvFilter::new(&plan.otel);
    let subscriber = tracing_subscriber::registry()
        .with(layer(tracer).with_filter(record_filter))
        .with(tracing_subscriber::fmt::layer().with_filter(console_filter));
    if subscriber.try_init().is_err() {
        op_warn!(domain = pump, "OTel init: a global tracing subscriber already exists (e.g. degenbot-python's registry); the OTel layer must be added to that registry");
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
