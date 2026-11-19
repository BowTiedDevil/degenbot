//! Runtime-flavor matrix for the OTLP/HTTP export path.
//!
//! The production provider constructor must deliver a span through the stock
//! `opentelemetry-otlp` client on BOTH driver runtime flavors: the
//! `BatchSpanProcessor(TokioCurrentThread)` worker exports on its own
//! current-thread runtime, while the driver that emits spans may itself be a
//! current-thread runtime (the Python path, the settlement-bot example) or a
//! multi-thread runtime (the bot's dedicated runtimes).
//!
//! The receiver is an in-process OTLP/HTTP endpoint, so the proof is a real
//! POST on the wire with no external collector.
#![cfg(feature = "otel")]
#![expect(clippy::expect_used)]

#[path = "support/otlp_http_receiver.rs"]
mod otlp_http_receiver;

use otlp_http_receiver::OtlpHttpReceiver;
use tracing_subscriber::layer::SubscriberExt;

/// Emit one span through the production constructor and flush the batch.
fn export_one_span(base_url: &str, probe: &str) {
    let (provider, tracer) =
        degenbot_bot::otel::provider_from_endpoint(base_url).expect("stock exporter build");
    {
        let subscriber = tracing_subscriber::registry().with(degenbot_bot::otel::layer(tracer));
        let _default = tracing::subscriber::set_default(subscriber);
        let span = tracing::info_span!("degenbot.otel.matrix", probe = probe);
        let _entered = span.enter();
        tracing::info!("matrix probe event");
    }
    provider.force_flush().expect("flush");
}

#[tokio::test]
async fn stock_export_delivers_on_a_current_thread_runtime() {
    let receiver = OtlpHttpReceiver::start();
    export_one_span(&receiver.base_url(), "current-thread");
    assert_eq!(
        receiver.request_targets(),
        vec!["/v1/traces".to_string()],
        "one batch must reach the signal path appended to a bare collector URL"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stock_export_delivers_on_a_multi_thread_runtime() {
    let receiver = OtlpHttpReceiver::start();
    export_one_span(&receiver.base_url(), "multi-thread");
    assert_eq!(
        receiver.request_targets(),
        vec!["/v1/traces".to_string()],
        "the emitted span must reach the collector from a multi-thread driver runtime"
    );
}
