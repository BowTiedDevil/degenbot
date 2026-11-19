//! `provider_from_env_endpoint` contract, in a dedicated process: the export
//! endpoint AND the request deadline both come from the `OTel` spec env vars,
//! so this binary owns the process environment (the matrix and Jaeger tests
//! pass explicit endpoints instead).
#![cfg(feature = "otel")]
#![expect(clippy::expect_used, clippy::print_stderr)]

#[path = "support/otlp_http_receiver.rs"]
mod otlp_http_receiver;

use std::net::TcpListener;
use std::time::{Duration, Instant};

use otlp_http_receiver::OtlpHttpReceiver;
use tracing_subscriber::layer::SubscriberExt;

/// Emit one span through the env-configured provider and flush on the given
/// driver runtime; returns the flush result.
fn export_through_env(runtime: &tokio::runtime::Runtime, probe: &str) -> Result<(), String> {
    let (provider, tracer) =
        degenbot_bot::otel::provider_from_env_endpoint().expect("stock exporter build");
    runtime.block_on(async {
        tracing::subscriber::with_default(
            tracing_subscriber::registry().with(degenbot_bot::otel::layer(tracer)),
            || {
                let span = tracing::info_span!("degenbot.otel.env", probe = probe);
                let _entered = span.enter();
                tracing::info!("env probe event");
            },
        );
    });
    provider.force_flush().map_err(|error| error.to_string())
}

/// A bound-but-never-accepting listener: connects succeed from the backlog and
/// the request is never answered, so only the configured deadline can end the
/// export.
struct StalledEndpoint {
    _listener: TcpListener,
    base_url: String,
}

impl StalledEndpoint {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stalled endpoint");
        let addr = listener.local_addr().expect("stalled address");
        Self {
            _listener: listener,
            base_url: format!("http://{addr}"),
        }
    }
}

#[test]
fn env_endpoint_delivers_on_both_runtimes_and_env_timeout_bounds_a_stall() {
    let receiver = OtlpHttpReceiver::start();
    std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", receiver.base_url());

    let current_thread = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    export_through_env(&current_thread, "current-thread").expect("flush on current-thread runtime");
    assert_eq!(
        receiver.request_targets(),
        vec!["/v1/traces".to_string()],
        "the env endpoint must deliver one batch to the appended signal path"
    );

    let multi_thread = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("multi-thread runtime");
    export_through_env(&multi_thread, "multi-thread").expect("flush on multi-thread runtime");
    assert_eq!(
        receiver.request_targets().len(),
        2,
        "the env endpoint must deliver one batch per flush on both runtime flavors"
    );

    // Deadline: same env-configured path, now against an endpoint that accepts
    // the connection and never answers. `OTEL_EXPORTER_OTLP_TIMEOUT` is the
    // supported knob (milliseconds, spec signal-specific var wins over it).
    let stalled = StalledEndpoint::start();
    std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", &stalled.base_url);
    std::env::set_var("OTEL_EXPORTER_OTLP_TIMEOUT", "100");

    let started = Instant::now();
    let flushed = export_through_env(&current_thread, "stalled");
    let elapsed = started.elapsed();
    eprintln!("stalled export: flush={flushed:?} elapsed={elapsed:?}");

    assert!(
        flushed.is_err(),
        "an unanswered export must fail rather than succeed"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the configured export deadline must bound a stalled request, took {elapsed:?}"
    );
}
