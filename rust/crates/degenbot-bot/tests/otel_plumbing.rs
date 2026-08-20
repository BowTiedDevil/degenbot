// Integration tests for the `otel` module (epic KDUED5 / DFN6FF).
//
// Compiled only under the `otel` feature so the default-feature test build
// compiles zero OpenTelemetry code (same gating as the module itself).
#![cfg(feature = "otel")]

//! Seam A: the otel plumbing — provider builder + resource + layer — is
//! verified against an in-memory span exporter on a LOCAL subscriber (a
//! per-thread default), never the process-global one.

use degenbot_bot::otel;
use opentelemetry::{Key, KeyValue};
use opentelemetry_sdk::trace::InMemorySpanExporter;
use tracing_subscriber::layer::SubscriberExt;

const SERVICE_NAME_KEY: &str = "service.name";
const SERVICE_VERSION_KEY: &str = "service.version";

/// A span emitted through the `OTel` layer arrives at the exporter with its
/// name, field mapped to an attribute, and the event attached. (The resource
/// identity is asserted separately: the SDK carries it on the provider
/// config, not per `SpanData`.)
#[test]
fn probe_span_is_exported_with_name_field_event_and_resource() {
    let exporter = InMemorySpanExporter::default();
    let (provider, tracer) = otel::provider_with_exporter(exporter.clone());

    let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));
    // Thread-local default subscriber: no global mutation, test-safe.
    let _guard = tracing::subscriber::set_default(subscriber);

    {
        let span = tracing::info_span!("degenbot.otel.probe", probe.kind = "plumbing");
        let _entered = span.enter();
        tracing::info!("probe event");
    }

    let flushed = provider.force_flush();
    assert!(flushed.is_ok(), "force_flush failed: {flushed:?}");

    let spans_res = exporter.get_finished_spans();
    assert!(
        spans_res.is_ok(),
        "get_finished_spans failed: {spans_res:?}"
    );
    let spans = match spans_res {
        Ok(spans) => spans,
        Err(_) => return, // unreachable: asserted ok above
    };

    let probe_count = spans
        .iter()
        .filter(|s| s.name.as_ref() == "degenbot.otel.probe")
        .count();
    assert_eq!(
        probe_count,
        1,
        "expected exactly one probe span, got: {:?}",
        spans.iter().map(|s| s.name.as_ref()).collect::<Vec<_>>()
    );

    // Content of the probe span: field -> attribute, event, resource keys.
    let probe_ok = spans
        .iter()
        .filter(|s| s.name.as_ref() == "degenbot.otel.probe")
        .all(|s| {
            let attr_ok = s.attributes.iter().any(|kv| {
                kv.key == Key::from_static_str("probe.kind") && kv.value.as_str() == "plumbing"
            });
            let event_ok = s
                .events
                .events
                .iter()
                .any(|e| e.name.as_ref() == "probe event");
            attr_ok && event_ok
        });
    assert!(
        probe_ok,
        "probe span content mismatch: attributes/events wrong"
    );
}

/// The layer constructor consumes the tracer and yields a registry-agnostic
/// layer that composes onto a plain `tracing_subscriber::Registry` (the
/// Python-path task, K6PCKP, composes it onto its own registry the same way).
#[test]
fn layer_builds_on_bare_registry() {
    let exporter = InMemorySpanExporter::default();
    let (_provider, tracer) = otel::provider_with_exporter(exporter.clone());
    let _subscriber = tracing_subscriber::registry().with(otel::layer(tracer));
}

/// The attribute-key comparison used above relies on `Key` equality by static
/// str — pin it so an `OTel` bump can't silently weaken the content assertion.
#[test]
fn key_value_lookup_roundtrip() {
    let kv = KeyValue::new("probe.kind", "plumbing");
    let found = kv.key == Key::from_static_str("probe.kind");
    let value = kv.value.as_str() == "plumbing";
    assert!(
        found && value,
        "KeyValue comparison changed under an OTel bump"
    );
}

/// The standard bot resource carries the fixed service identity.
#[test]
fn bot_resource_carries_service_identity() {
    let resource = otel::bot_resource();
    let name_ok = resource
        .get_ref(&Key::from_static_str(SERVICE_NAME_KEY))
        .is_some_and(|v| v.as_str() == "degenbot-bot");
    let version_ok = resource
        .get_ref(&Key::from_static_str(SERVICE_VERSION_KEY))
        .is_some_and(|v| v.as_str() == env!("CARGO_PKG_VERSION"));
    assert!(
        name_ok && version_ok,
        "bot resource missing service identity"
    );
}
