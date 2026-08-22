//! Live end-to-end probe: one real span through the production provider
//! constructor (`provider_from_endpoint`) into a REAL Jaeger. Ignored by
//! default - needs `DEGENBOT_JAEGER_E2E=1` and a reachable OTLP endpoint
//! (`DEGENBOT_JAEGER_ENDPOINT`, default `http://127.0.0.1:4318`).
//!
//! Born from the 2026-08-22 diagnosis: the default otlp client (reqwest
//! builder `.timeout()`) fails instantly on the `BatchSpanProcessor`'s
//! current-thread runtime, so nothing ever reached Jaeger. This test is the
//! regression tripwire for that failure mode - it exercises the exact
//! provider path the bot uses.
#![cfg(feature = "otel")]
#![expect(clippy::expect_used, clippy::panic)] // live probe: panic with context is the point
#![allow(clippy::print_stderr)] // eprintln is the report channel for an ignored probe

use tracing_subscriber::layer::SubscriberExt;

#[test]
fn span_reaches_real_jaeger_over_otlp_http() {
    if std::env::var("DEGENBOT_JAEGER_E2E").as_deref() != Ok("1") {
        eprintln!(
            "skipped: set DEGENBOT_JAEGER_E2E=1 with Jaeger reachable at \
             DEGENBOT_JAEGER_ENDPOINT"
        );
        return;
    }
    let endpoint = std::env::var("DEGENBOT_JAEGER_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:4318".into());
    let (provider, tracer) =
        degenbot_bot::otel::provider_from_endpoint(&endpoint).expect("exporter build");

    let subscriber = tracing_subscriber::registry().with(degenbot_bot::otel::layer(tracer));
    let _guard = tracing::subscriber::set_default(subscriber);

    {
        let span = tracing::info_span!("degenbot.jaeger.e2e", probe = "live");
        let _entered = span.enter();
        tracing::info!("e2e probe event");
    }
    match provider.force_flush() {
        Ok(()) => eprintln!(
            "flush OK to {endpoint}; expect service degenbot-bot in Jaeger within seconds"
        ),
        Err(e) => panic!("flush failed: {e:?}"),
    }
}
