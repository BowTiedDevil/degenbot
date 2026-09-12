//! ADR-043 §7 naming gate: spans and metric instruments are normalized.
//!
//! * span    — \`degenbot.<area>.<verb>\` (the \`degenbot.\` root plus at least two
//!   non-empty snake segments);
//! * metric  — \`degenbot.<area>.<noun>\` instrument name (the Prometheus
//!   renderer maps \`.\` to \`_\`, so the exposed series stays
//!   \`degenbot_<area>_<noun>_<unit>\`).
//!
//! The sweep is source-based because the rule is about what maintainers
//! write: a name that is only normalized at the export boundary drifts back.
//! \`#[cfg(test)]\` modules and \`tests/\` trees are exempt — they name spans to
//! MATCH the production names they assert on, not to be discovered in Jaeger.

use std::path::{Path, PathBuf};

/// Byte ranges covered by a \`#[cfg(test)]\` (or \`#[cfg(all(test, ...))]\`)
/// module: the attribute, its module, and everything inside its braces.
fn test_regions(text: &str) -> Vec<(usize, usize)> {
    let mut regions: Vec<(usize, usize)> = Vec::new();
    let mut idx = 0usize;
    // Matches `#[cfg(test)]`, `#[cfg(all(test, ...))]`, `#[cfg(any(test, ...))]`
    // — every attribute form that makes a module test-only.
    while let Some(pos) = text[idx..].find("(test") {
        let attr = idx + pos;
        idx = attr + "(test".len();
        let Some(mod_rel) = text[idx..].find("mod ") else {
            break;
        };
        let mod_abs = idx + mod_rel;
        let Some(brace_rel) = text[mod_abs..].find('{') else {
            continue;
        };
        let open = mod_abs + brace_rel;
        let mut depth = 0i32;
        let mut in_str = false;
        let mut prev_backslash = false;
        let mut end = text.len();
        for (offset, ch) in text[open..].char_indices() {
            let abs = open + offset;
            if in_str {
                if ch == '"' && !prev_backslash {
                    in_str = false;
                }
                prev_backslash = ch == '\\' && !prev_backslash;
                continue;
            }
            if ch == '"' {
                in_str = true;
                prev_backslash = false;
                continue;
            }
            if ch == '{' {
                depth += 1;
            } else if ch == '}' {
                depth -= 1;
                if depth == 0 {
                    end = abs + 1;
                    break;
                }
            }
        }
        regions.push((attr, end));
        idx = end;
    }
    regions
}

fn in_test_region(regions: &[(usize, usize)], offset: usize) -> bool {
    regions
        .iter()
        .any(|(start, end)| offset >= *start && offset < *end)
}

/// Span names in \`text\`, paired with their byte offset.
fn span_names(text: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let needle = "span!(";
    let mut idx = 0usize;
    while let Some(pos) = text[idx..].find(needle) {
        let abs = idx + pos;
        idx = abs + needle.len();
        let after = text[idx..].trim_start();
        let Some(rest) = after.strip_prefix('"') else {
            continue;
        };
        let Some(end) = rest.find('"') else { continue };
        let name = rest[..end].to_string();
        if !name.is_empty() {
            out.push((abs, name));
        }
    }
    out
}

/// Metric instrument names in \`text\`: the literal passed to an
/// \`opentelemetry\` instrument builder.
fn instrument_names(text: &str) -> Vec<(usize, String)> {
    const BUILDERS: &[&str] = &[
        ".u64_counter(",
        ".i64_counter(",
        ".f64_counter(",
        ".u64_histogram(",
        ".i64_histogram(",
        ".f64_histogram(",
        ".u64_gauge(",
        ".i64_gauge(",
        ".f64_gauge(",
        ".u64_up_down_counter(",
        ".i64_up_down_counter(",
    ];
    let mut out: Vec<(usize, String)> = Vec::new();
    for builder in BUILDERS {
        let mut idx = 0usize;
        while let Some(pos) = text[idx..].find(builder) {
            let abs = idx + pos;
            idx = abs + builder.len();
            let after = text[idx..].trim_start();
            let Some(rest) = after.strip_prefix('"') else {
                continue;
            };
            let Some(end) = rest.find('"') else { continue };
            let name = rest[..end].to_string();
            if !name.is_empty() {
                out.push((abs, name));
            }
        }
    }
    out
}

/// Every \`.\`-separated segment is non-empty lowercase snake.
fn snake_segments(rest: &str) -> bool {
    let segs: Vec<&str> = rest.split('.').collect();
    !segs.is_empty()
        && segs.iter().all(|s| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
}

/// Span: \`degenbot.\` + at least two more snake segments.
fn span_conforms(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("degenbot.") else {
        return false;
    };
    rest.split('.').count() >= 2 && snake_segments(rest)
}

/// Metric: \`degenbot.\` + at least one more snake segment.
fn metric_conforms(name: &str) -> bool {
    name.strip_prefix("degenbot.").is_some_and(snake_segments)
}

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

#[test]
fn production_spans_and_metrics_are_normalized() {
    let root = crates_root();
    let mut files = Vec::new();
    collect_rs(&root, &mut files);
    assert!(!files.is_empty(), "source sweep found no files");

    let mut violations: Vec<String> = Vec::new();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let regions = test_regions(&text);
        let rel = path
            .strip_prefix(&root)
            .map_or_else(|_| path.display().to_string(), |p| p.display().to_string());
        for (offset, name) in span_names(&text) {
            if in_test_region(&regions, offset) {
                continue;
            }
            if !span_conforms(&name) {
                violations.push(format!("{rel}:{offset}: span name {name:?}"));
            }
        }
        for (offset, name) in instrument_names(&text) {
            if in_test_region(&regions, offset) {
                continue;
            }
            if !metric_conforms(&name) {
                violations.push(format!("{rel}:{offset}: metric name {name:?}"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "ADR-043 §7 naming violations (span degenbot.<area>.<verb>, \
         metric degenbot.<area>.<noun>):\n{}",
        violations.join("\n")
    );
}
