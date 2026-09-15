//! ADR-043 §9 metric-cardinality gate.
//!
//! Metric label NAMES are a closed, reviewed set: a new label requires
//! updating the allowlist below (the review moment), and any label whose name
//! looks like an unbounded value (pool/path/token/address/hash/block/tx/id)
//! fails outright — those belong in spans and logs, where cardinality is free.
//!
//! Plus the §9 self-metric: `degenbot.metric_series` exposes the live distinct
//! series count so a blowup is visible before the collector falls over.

// The workspace denies `expect_used`/`panic`; this test file needs both only
// in `(a)` `crates_root` (scoped at that fn) and (b) the otel-gated §9
// self-metric test below (scoped at that fn). A former file-level
// `#![expect(clippy::expect_used, clippy::panic)]` went UNFULFILLED under
// default-feature `clippy -p degenbot-bot --all-targets`: the only use of
// both lints outside the fn-scoped one lives in the
// `#[cfg(feature = "otel")]` test, which default-feature compiles out, and
// `crates_root`'s fn-level attribute already claims its own emission (the
// tightest enclosing expectation wins), leaving the file-level pair unused.

use std::path::{Path, PathBuf};

/// Every metric label name the workspace may attach, with its closed value
/// set. Add a name here ONLY with a `&'static` closed constant behind it.
const ALLOWED_LABELS: &[&str] = &[
    "outcome",
    "site",
    "reason",
    "mode",
    "arm",
    "verdict",
    "sink",
    "service.version",
    "role",
    "resource",
    "profile",
    "probe.kind",
    "kind",
    "binding",
];

/// Name fragments that signal an unbounded value (a cardinality bomb).
const FORBIDDEN_FRAGMENTS: &[&str] = &[
    "pool", "path", "token", "address", "hash", "block", "tx", "id", "user",
];

#[expect(clippy::expect_used)] // CARGO_MANIFEST_DIR always has a parent
fn crates_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate dir has a parent")
        .to_path_buf()
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if name == "target" || name.starts_with('.') {
                continue;
            }
            collect_rs(&path, out);
        } else if Path::new(&name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
            && !path.to_string_lossy().contains("/tests/")
        {
            out.push(path);
        }
    }
}

/// Pull `KeyValue::new("name", ...)` label names out of source text.
fn label_names(text: &str) -> Vec<(usize, String)> {
    let needle = "KeyValue::new(\"";
    let mut out = Vec::new();
    let mut idx = 0usize;
    while let Some(pos) = text[idx..].find(needle) {
        let abs = idx + pos;
        idx = abs + needle.len();
        let Some(end) = text[idx..].find('"') else {
            continue;
        };
        let name = text[idx..idx + end].to_string();
        if !name.is_empty() {
            out.push((abs, name));
        }
    }
    out
}

#[test]
fn metric_labels_are_a_closed_reviewed_set() {
    let root = crates_root();
    let mut files = Vec::new();
    collect_rs(&root, &mut files);
    assert!(!files.is_empty(), "source sweep found no files");

    let mut violations: Vec<String> = Vec::new();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let rel = path
            .strip_prefix(&root)
            .map_or_else(|_| path.display().to_string(), |p| p.display().to_string());
        for (offset, name) in label_names(&text) {
            let key = name.to_ascii_lowercase();
            if FORBIDDEN_FRAGMENTS
                .iter()
                .any(|frag| key.split(['.', '_']).any(|seg| seg == *frag))
            {
                violations.push(format!(
                    "{rel}:{offset}: label {name:?} names an unbounded value"
                ));
                continue;
            }
            if !ALLOWED_LABELS.contains(&name.as_str()) {
                violations.push(format!(
                    "{rel}:{offset}: label {name:?} is not in ALLOWED_LABELS"
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "ADR-043 §9 cardinality violations:\n{}",
        violations.join("\n")
    );
}

#[cfg(feature = "otel")]
#[expect(clippy::expect_used, clippy::panic)] // otel-gated: fires only under --features otel
#[test]
fn metric_series_self_metric_is_exported() {
    use degenbot_bot::metrics::{build_prometheus_provider, render};
    use opentelemetry::metrics::MeterProvider as _;

    let (provider, registry) = build_prometheus_provider().expect("build provider");
    let counter = provider
        .meter("degenbot-bot")
        .u64_counter("degenbot.gate.probe")
        .build();
    counter.add(1, &[]);

    let first = render(&registry);
    assert!(
        first.contains("degenbot_metric_series"),
        "the §9 self-metric must be exported; got:\n{first}"
    );
    // The reported value lags one scrape: the gauge callback must not re-enter
    // `prometheus::Registry::gather` (it runs inside the collect), so the
    // second render carries the count the first measured.
    let second = render(&registry);
    let series_line = second
        .lines()
        .find(|l| !l.starts_with('#') && l.starts_with("degenbot_metric_series"))
        .unwrap_or_else(|| panic!("no sample line for the self-metric:\n{second}"));
    let value = series_line
        .rsplit(' ')
        .next()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or_else(|| panic!("unparseable sample: {series_line}"));
    assert!(
        value >= 1.0,
        "series count must include the probe + the self-metric: {series_line}"
    );
}
