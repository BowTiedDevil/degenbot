#![cfg(feature = "otel")]
#![expect(clippy::expect_used, clippy::panic, clippy::print_stdout)]

//! ADR-043 §8/§10 golden snapshots: boot, one block, one revert.
//!
//! §10 is the point of this file: every demoted or deleted log site must name
//! the metric / span attribute / span event that preserves its tripwire, and
//! a demotion that silently blinds a detector has to show up as a DIFF. These
//! snapshots are that diff — they capture the console text the Python-owned
//! console writer would emit, the exported span set (name + attributes +
//! events), and the metric series (names + label KEYS, never values) for one
//! pass of each block-lifecycle phase.
//!
//! # Normalization (what is deliberately NOT in the snapshot)
//!
//! * timing values (`*_us`, `*_ms`, `*, age`) — wall clock, not contract;
//! * span order — the exporter's completion order is not contract, so the
//!   span lines are sorted (parent linkage rides the attributes that matter,
//!   e.g. `stage.from` / `stage.to`);
//! * metric values — only names + sorted label keys;
//! * host facts (thread counts, bind addresses) — the worker-census boot table
//!   is host- and test-order-dependent, so it is NOT part of this snapshot;
//!   its entry shapes are covered by `worker_census`'s own contract tests.
//!
//! # Regenerating
//!
//! ```text
//! UPDATE_GOLDEN=1 cargo test -p degenbot-bot --features otel \
//!     --test observability_snapshots
//! ```
//! Then READ THE DIFF before committing it: the snapshot is only a gate if a
//! changed line is a deliberate contract change.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use degenbot_bot::bot_core::epoch::Epoch;
use degenbot_bot::bot_core::stage_telemetry::StageTelemetry;
use degenbot_bot::metrics::{build_prometheus_provider, render};
use degenbot_bot::otel;
use opentelemetry_sdk::trace::InMemorySpanExporter;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Registry;

/// One captured console line: the level, the target the formatter derives its
/// area prefix from, and the rendered fields.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<String>>>);

impl<S> Layer<S> for Capture
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut fields = Fields::default();
        event.record(&mut fields);
        let line = format!(
            "{} {}: {}",
            meta.level(),
            meta.target(),
            fields.0.trim_end()
        );
        self.0.lock().expect("capture lock").push(line);
    }
}

#[derive(Default)]
struct Fields(String);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let _ = write!(self.0, "{}={value:?} ", field.name());
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        let _ = write!(self.0, "{}={value} ", field.name());
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        let _ = write!(self.0, "{}={value} ", field.name());
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        let _ = write!(self.0, "{}={value} ", field.name());
    }
}

/// Keys that are wall-clock or process-local, not contract:
///
/// * `*_us` / `*_ms` / `*_ns` and `*age` / `duration` / `elapsed` — timing;
/// * `busy_ns` / `idle_ns` — the `OTel` layer's own span timing;
/// * `thread.id` / `thread.name` — the harness thread, not the contract;
/// * `code.line.number` — an unrelated edit above an emit site would churn the
///   golden for no observability reason (the file path and module name are
///   kept: those DO catch a moved emit site).
fn is_timing_key(key: &str) -> bool {
    key.ends_with("_us")
        || key.ends_with("_ms")
        || key.ends_with("_ns")
        || key.ends_with("age")
        || key.contains("duration")
        || key.contains("elapsed")
        || key == "thread.id"
        || key == "thread.name"
        || key == "code.line.number"
}

fn normalize_span(span: &opentelemetry_sdk::trace::SpanData) -> String {
    let mut attrs: Vec<String> = span
        .attributes
        .iter()
        .filter(|kv| !is_timing_key(kv.key.as_str()))
        .map(|kv| format!("{}={}", kv.key, kv.value.to_string().trim_matches('"')))
        .collect();
    attrs.sort();
    let mut events: Vec<String> = span
        .events
        .events
        .iter()
        .map(|event| event.name.to_string())
        .collect();
    events.sort();
    format!(
        "{} attrs=[{}] events=[{}]",
        span.name,
        attrs.join(","),
        events.join(",")
    )
}

/// Metric sample lines reduced to `name{label_keys}` — names and label KEYS
/// only (values and labels' values are not contract).
fn normalize_series(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let (head, _value) = line.rsplit_once(' ')?;
            Some(match head.split_once('{') {
                Some((name, labels)) => {
                    let mut keys: Vec<&str> = labels
                        .trim_end_matches('}')
                        .split(',')
                        .filter_map(|kv| kv.split_once('=').map(|(key, _)| key.trim()))
                        .collect();
                    keys.sort_unstable();
                    format!("{name}{{{}}}", keys.join(","))
                }
                None => head.to_string(),
            })
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Run one scenario under a local capture subscriber + in-memory exporters and
/// render the three snapshot sections.
fn capture<F: FnOnce()>(scenario: F) -> String {
    let capture = Capture::default();
    let exporter = InMemorySpanExporter::default();
    let (tracer_provider, tracer) = otel::provider_with_exporter(exporter.clone());
    let (_meter_provider, registry) = build_prometheus_provider().expect("metrics provider");

    // Order matters: `otel::layer` is typed for a `Registry` subscriber, so it
    // composes first; the capture layer then stacks on top of that registry.
    let subscriber = Registry::default()
        .with(otel::layer(tracer))
        .with(capture.clone());
    tracing::subscriber::with_default(subscriber, scenario);

    tracer_provider.force_flush().expect("flush spans");
    let spans = exporter.get_finished_spans().expect("exported spans");
    let mut span_lines: Vec<String> = spans.iter().map(normalize_span).collect();
    span_lines.sort();

    let console = capture.0.lock().expect("capture lock").clone();
    let series = normalize_series(&render(&registry));

    let mut out = String::new();
    out.push_str("## console\n");
    for line in &console {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("## spans\n");
    for line in &span_lines {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("## series\n");
    for line in &series {
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/observability_snapshots")
        .join(format!("{name}.txt"))
}

fn assert_snapshot(name: &str, body: &str) {
    let path = golden_path(name);
    let text = format!(
        "# observability snapshot: {name}\n\
         # regenerate: UPDATE_GOLDEN=1 cargo test -p degenbot-bot --features otel \
         --test observability_snapshots\n{body}"
    );
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("golden dir");
        }
        std::fs::write(&path, &text).expect("write golden");
        println!("updated {}", path.display());
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "golden {} unreadable ({err}); regenerate with UPDATE_GOLDEN=1",
            path.display()
        )
    });
    assert_eq!(
        expected, text,
        "ADR-043 §8 golden snapshot drift in {name}. If the change is a \
         deliberate contract change, regenerate and READ THE DIFF; if it is a \
         lost signal, cite the metric/span-event that preserves the tripwire \
         (ADR-043 §10)."
    );
}

/// The `degenbot::<domain>` target taxonomy, exercised end to end: one facade
/// INFO per closed domain, captured through the console formatter. This is the
/// boot-phase artifact — a domain added, removed, or re-targeted is a diff.
///
/// The worker-census boot table is deliberately NOT here: it carries host and
/// test-order facts (thread counts, which resources registered first).
#[test]
fn boot_snapshot() {
    use degenbot_core::{diag, op_info};
    let body = capture(|| {
        op_info!(domain = state, "boot probe");
        op_info!(domain = path, "boot probe");
        op_info!(domain = solver, "boot probe");
        op_info!(domain = sim, "boot probe");
        op_info!(domain = pump, "boot probe");
        op_info!(domain = exec, "boot probe");
        op_info!(domain = verify, "boot probe");
        op_info!(domain = ingest, "boot probe");
        op_info!(domain = rpc, "boot probe");
        op_info!(domain = aave, "boot probe");
        diag!(domain = sim, "boot probe (debug)");
    });
    assert_snapshot("boot", &body);
}

/// One block, happy path: the epoch root opens, the Streaming interval opens
/// at the first relevant log, quiesces, then publishes.
#[test]
fn one_block_snapshot() {
    let body = capture(|| {
        let epoch = Epoch::at(21_000_000);
        let root = tracing::info_span!(
            "degenbot.epoch.run",
            block.number = 21_000_000u64,
            epoch.block = 21_000_000u64,
            epoch.seq = 0u64,
        );
        let _root_guard = root.enter();
        let mut stages = StageTelemetry::new();
        stages.new_epoch();
        stages.on_first_log(&root, epoch);
        stages.on_quiesced(&root, epoch, 7);
        stages.on_publish(&root, epoch);
    });
    assert_snapshot("one_block", &body);
}

/// One revert: the same block streams, then a reorg opens the Rewind interval
/// across an epoch-sequence bump, and the window closes at the next publish.
#[test]
fn one_revert_snapshot() {
    let body = capture(|| {
        let epoch = Epoch::at(21_000_000);
        let root = tracing::info_span!(
            "degenbot.epoch.run",
            block.number = 21_000_000u64,
            epoch.block = 21_000_000u64,
            epoch.seq = 0u64,
        );
        let _root_guard = root.enter();
        let mut stages = StageTelemetry::new();
        stages.new_epoch();
        stages.on_first_log(&root, epoch);
        stages.on_enter_reorg(&root, epoch, None);

        // The rewind generation bumps; the next epoch root carries seq + 1.
        let rewound = Epoch::with_generation(21_000_000, 1);
        let next_root = tracing::info_span!(
            "degenbot.epoch.run",
            block.number = 21_000_000u64,
            epoch.block = 21_000_000u64,
            epoch.seq = 1u64,
        );
        let _next_guard = next_root.enter();
        stages.new_epoch();
        stages.on_first_log(&next_root, rewound);
        stages.on_close_reorg(&next_root, rewound);
    });
    assert_snapshot("one_revert", &body);
}
