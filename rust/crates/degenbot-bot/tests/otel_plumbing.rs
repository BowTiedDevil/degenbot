// Integration tests for the `otel` module (epic KDUED5 / DFN6FF).
//
// Compiled only under the `otel` feature so the default-feature test build
// compiles zero OpenTelemetry code (same gating as the module itself).
#![cfg(feature = "otel")]
#![expect(clippy::expect_used)]

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
    let Ok(spans) = spans_res else {
        return; // unreachable: asserted ok above
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

/// MQUKB6-T0: work handed across the drain pipe must not orphan into root
/// traces. The drainer task enters the span that was current at
/// `dispatch()` time, so spans emitted while processing a `DrainWork` item
/// (today: `degenbot.arb.solve` via the sink) parent under the pump's
/// per-block span instead of becoming disconnected Jaeger roots.
#[tokio::test]
async fn drain_work_parents_under_the_dispatching_span() {
    use degenbot_bot::bot_core::drain_sink::DrainSink;
    use degenbot_bot::bot_core::event_dispatch::{DispatchOwner, DrainWork};
    use degenbot_bot::bot_core::BlockMetadata;
    use std::sync::Arc;

    /// A sink whose `on_drain` emits a probe span — the stand-in for the
    /// solve-path spans the real sink fires under the drainer.
    struct SpanEmittingSink;
    impl DrainSink for SpanEmittingSink {
        fn has_dirty_paths(&self) -> bool {
            false
        }
        fn on_drain(&self, _block: u64, _metadata: &BlockMetadata) {
            let probe = tracing::info_span!("drain.work");
            let _probe = probe.enter();
        }
        fn on_send(&self, _metadata: &BlockMetadata) {}
        fn finalize_block(&self, _block: u64, _metadata: &BlockMetadata) {}
        fn set_last_solved_block(&self, _block: u64) {}
        fn set_solve_anchor(&self, _block: u64) {}
        fn record_logs_this_block(&self) {}
        fn last_processed_block(&self) -> Option<u64> {
            None
        }
        fn notify_block(&self, _block: u64, _metadata: &BlockMetadata) {}
    }

    let exporter = InMemorySpanExporter::default();
    let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
    let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));
    // Thread-local default + current-thread tokio runtime (the #[tokio::test]
    // default): the spawned drainer task is polled on THIS thread, so it sees
    // the same subscriber and the exported parent linkage is observable.
    let _guard = tracing::subscriber::set_default(subscriber);

    let sink: Arc<dyn DrainSink> = Arc::new(SpanEmittingSink);
    let owner = DispatchOwner::new(sink, &None);

    {
        let outer = tracing::info_span!("test.dispatch.outer");
        let _entered = outer.enter();
        owner.dispatch(DrainWork::Drain {
            block: 1,
            metadata: BlockMetadata::default(),
        });
    }

    // Settle the drainer (current-thread runtime: yield until it makes progress).
    for _ in 0..10_000 {
        if owner.health().processed() > 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        owner.health().processed() > 0,
        "drainer never processed the dispatched work"
    );

    provider.force_flush().expect("force_flush failed");
    let spans = exporter
        .get_finished_spans()
        .expect("get_finished_spans failed");

    let outer_id = spans
        .iter()
        .find(|s| s.name.as_ref() == "test.dispatch.outer")
        .map(|s| s.span_context.span_id())
        .expect("outer dispatch span not exported");

    let work = spans
        .iter()
        .find(|s| s.name.as_ref() == "drain.work")
        .expect("drain.work span not exported");

    assert_eq!(
        work.parent_span_id, outer_id,
        "drain.work must parent under the span current at dispatch() time"
    );
}

/// T1 seam: a recorded counter is readable from the Prometheus registry text —
/// the contract the Grafana scrape depends on.
#[test]
fn recorded_counter_is_rendered_in_prometheus_text() {
    use degenbot_bot::metrics;
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry::KeyValue;

    let (provider, registry) = metrics::build_prometheus_provider().expect("provider");
    let meter = provider.meter("degenbot.test");
    let counter = meter.u64_counter("probe_counter_total_check").build();
    counter.add(7, &[KeyValue::new("verdict", "profitable")]);

    let text = metrics::render(&registry);
    // The exporter always attaches at least the otel_scope_name label, so
    // assert on the family name and the sampled value, not a bare `name 7`.
    let sample = text
        .lines()
        .find(|l| l.starts_with("probe_counter_total_check_total"))
        .expect("probe counter family missing from prometheus text");
    assert!(
        sample.ends_with(" 7"),
        "expected counter value 7, got: {sample}"
    );
}

/// T1 seam: the /metrics HTTP server serves the registry text over TCP.
#[test]
fn metrics_http_server_serves_registry_text() {
    use degenbot_bot::metrics;
    use opentelemetry::metrics::MeterProvider;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    let (provider, registry) = metrics::build_prometheus_provider().expect("provider");
    let meter = provider.meter("degenbot.test");
    let counter = meter.u64_counter("http_probe_counter").build();
    counter.add(3, &[]);

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let addr = listener.local_addr().expect("local_addr");
    let server_registry = registry.clone();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server_stop = std::sync::Arc::clone(&stop);
    let handle = std::thread::spawn(move || {
        let body = move || metrics::render(&server_registry);
        metrics::serve_on_listener(&listener, &body, &server_stop);
    });

    // Give the accept loop a beat, then scrape it like Prometheus would.
    std::thread::sleep(Duration::from_millis(50));
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    stream
        .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .expect("write request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");

    assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");
    let sample = response
        .lines()
        .find(|l| l.starts_with("http_probe_counter_total"))
        .expect("counter family missing from scraped body");
    assert!(
        sample.ends_with(" 3"),
        "expected counter value 3, got: {sample}"
    );

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    // Nudge the accept loop with a final connection so it observes `stop`.
    let _ = std::net::TcpStream::connect(addr);
    handle.join().expect("server thread");
}

/// T2 seam: the named drain-path instruments render the expected Prometheus
/// families with recorded values.
#[test]
fn pipeline_instruments_render_expected_families() {
    use degenbot_bot::instruments::PipelineInstruments;
    use degenbot_bot::metrics;
    use opentelemetry::metrics::MeterProvider;

    let (provider, registry) = metrics::build_prometheus_provider().expect("provider");
    let meter = provider.meter("degenbot.test");
    let p = PipelineInstruments::new(&meter);

    p.observe_header_to_solved(0.25);
    p.count_block();
    p.count_log_received();
    p.count_log_applied();
    p.count_backfill();
    p.set_drain_queue_depth(3);
    p.set_state_head_lag(-2);

    let text = metrics::render(&registry);
    for family in [
        "degenbot_block_header_to_solved",
        "degenbot_blocks_observed_total",
        "degenbot_logs_received_total",
        "degenbot_logs_applied_total",
        "degenbot_backfills_executed_total",
        "degenbot_drain_queue_depth",
        "degenbot_state_head_lag_blocks",
    ] {
        assert!(
            text.contains(family),
            "expected family {family} in prometheus text, got:\n{text}"
        );
    }
    let lag = text
        .lines()
        .find(|l| l.starts_with("degenbot_state_head_lag_blocks"))
        .expect("lag gauge missing");
    assert!(lag.ends_with("-2"), "expected -2 lag, got: {lag}");
}

/// T3 seam: solver/simulate instruments render, including the labeled verdict
/// counter.
#[test]
fn solver_instruments_render_with_verdict_labels() {
    use degenbot_bot::instruments::PipelineInstruments;
    use degenbot_bot::metrics;
    use opentelemetry::metrics::MeterProvider;

    let (provider, registry) = metrics::build_prometheus_provider().expect("provider");
    let meter = provider.meter("degenbot.test");
    let p = PipelineInstruments::new(&meter);

    p.observe_solve_duration(0.012);
    p.count_solves_executed();
    p.set_registered_paths(42);
    p.count_candidates_found(7);
    p.observe_simulate_duration(0.003);
    p.count_simulate_verdict("profitable");
    p.count_simulate_verdict("profitable");
    p.count_simulate_verdict("not_profitable");

    let text = metrics::render(&registry);
    for family in [
        "degenbot_solve_duration",
        "degenbot_solves_executed_total",
        "degenbot_engine_registered_paths",
        "degenbot_candidates_found_total",
        "degenbot_simulate_duration",
        "degenbot_simulate_verdicts_total",
    ] {
        assert!(
            text.contains(family),
            "expected family {family} in prometheus text, got:\n{text}"
        );
    }
    let profitable = text
        .lines()
        .find(|l| l.starts_with("degenbot_simulate_verdicts_total") && l.contains("\"profitable\""))
        .expect("profitable verdict series missing");
    assert!(
        profitable.ends_with('2'),
        "expected 2 profitable, got: {profitable}"
    );
}

/// T4 seam: dispatch economics instruments render, including labeled
/// submit/monitor outcomes and cumulative P&L counters.
#[test]
fn dispatch_economics_instruments_render() {
    use degenbot_bot::instruments::PipelineInstruments;
    use degenbot_bot::metrics;
    use opentelemetry::metrics::MeterProvider;

    let (provider, registry) = metrics::build_prometheus_provider().expect("provider");
    let meter = provider.meter("degenbot.test");
    let p = PipelineInstruments::new(&meter);

    p.observe_dispatch_profits(1.0e18, 3.0e17);
    p.observe_dispatch_gas(250_000);
    p.count_submit_outcome("submitted");
    p.count_submit_outcome("skipped_dry_run");
    p.observe_submit_latency(0.042);
    p.add_profit_realized(2.5e17);
    p.add_profit_missed(1.0e17);
    p.count_monitor_outcome("confirmed");

    let text = metrics::render(&registry);
    for family in [
        "degenbot_dispatch_gross_profit",
        "degenbot_dispatch_net_profit",
        "degenbot_dispatch_gas_used",
        "degenbot_submit_outcomes_total",
        "degenbot_submit_latency",
        "degenbot_profit_realized",
        "degenbot_profit_missed",
        "degenbot_monitor_outcomes_total",
    ] {
        assert!(
            text.contains(family),
            "expected family {family} in prometheus text, got:\n{text}"
        );
    }
    let realized = text
        .lines()
        .find(|l| l.starts_with("degenbot_profit_realized"))
        .expect("realized profit series missing");
    // Prometheus renders the f64 sum as a plain integer at this magnitude.
    assert!(realized.ends_with("250000000000000000"), "got: {realized}");
}
