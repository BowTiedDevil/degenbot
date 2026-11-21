//! The closed `resource` id set, pinned to the tree's registration surface.
//!
//! [`worker_census::REGISTERED_RESOURCE_IDS`] is the closed set the `resource`
//! label of `degenbot_worker_census{resource=...}` draws from. It is a metric
//! cardinality contract, so two drifts matter and this test catches both: an id
//! the tree registers that the set does not declare (a label value appearing out
//! of nowhere) and a declared id with no registration site left (a phantom in the
//! contract).
//!
//! A test binary cannot hold the production rows: each id is registered by the
//! crate that owns the resource, at its own boot, so the live table here carries
//! only what this binary drives. The test therefore pins both halves:
//!
//! * the live table -- `runtime::get_runtime()` performs this crate's own
//!   registration, and every non-test row the snapshot then holds must be in the
//!   closed set;
//! * the surface that table is built from -- every `resource:` literal at a
//!   `WorkerCensusEntry` construction in the crate tree, plus every id a
//!   `census_resource()` fn returns (the fleet roles), compared with the closed
//!   set in both directions.
//!
//! `cfg(test)`-only rows are classified rather than exempted: an id spelled
//! `census-*` or `*_test` is a test-only registration, and a test-only
//! registration is never part of a production scrape's label set.
//!
//! The walk skips this file: its matcher vocabulary and assertion messages spell
//! the very shapes it searches for, so reading it would report the scanner as a
//! registration site.

#![expect(
    clippy::expect_used,
    reason = "test: a fixed CARGO_MANIFEST_DIR walk and the pinned canary ids fail loud rather than degrade"
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use degenbot_core::worker_census::{self, REGISTERED_RESOURCE_IDS};

/// The naming a `cfg(test)`-only registration carries, so the classifier can
/// tell a test row from a production id without reading the build flags.
const TEST_ONLY_PREFIXES: [&str; 1] = ["census-"];
const TEST_ONLY_SUFFIXES: [&str; 1] = ["_test"];

fn crates_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = rust/crates/foundation/degenbot-core
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .expect("the core manifest sits under rust/crates")
}

const SELF_FILE: &str = "worker_census_closed_set.rs";

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_str().unwrap_or_default();
        if path.is_dir() {
            if matches!(name, "target" | "vendor") || name.starts_with('.') {
                continue;
            }
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") && name != SELF_FILE {
            out.push(path);
        }
    }
}

/// `text` with line and block comments blanked, string literals intact.
fn strip_comments(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    let mut in_block = false;
    let mut in_string = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_block {
            if byte == b'*' && bytes.get(index + 1) == Some(&b'/') {
                in_block = false;
                index += 2;
            } else {
                if byte == b'\n' {
                    out.push(byte);
                }
                index += 1;
            }
            continue;
        }
        if in_string {
            out.push(byte);
            if byte == b'\\' {
                if let Some(next) = bytes.get(index + 1) {
                    out.push(*next);
                }
                index += 2;
                continue;
            }
            if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            in_block = true;
            index += 2;
            continue;
        }
        if byte == b'"' {
            in_string = true;
        }
        out.push(byte);
        index += 1;
    }
    String::from_utf8(out).unwrap_or_default()
}

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn string_literal(text: &str) -> Option<String> {
    let rest = text.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Every literal that stands in a `resource:` field (`resource: "id"`).
fn resource_field_literals(text: &str) -> Vec<String> {
    const FIELD: &str = "resource";
    let mut ids = Vec::new();
    let mut cursor = 0;
    while let Some(found) = text[cursor..].find(FIELD) {
        let at = cursor + found;
        cursor = at + FIELD.len();
        let before_ok = text.as_bytes()[..at]
            .last()
            .is_none_or(|byte| !is_ident(*byte));
        let after_ok = text
            .as_bytes()
            .get(cursor)
            .is_none_or(|byte| !is_ident(*byte));
        if !before_ok || !after_ok {
            continue;
        }
        let Some(value) = text[cursor..].trim_start().strip_prefix(':') else {
            continue;
        };
        if let Some(id) = string_literal(value.trim_start()) {
            ids.push(id);
        }
    }
    ids
}

fn string_literals(text: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('"') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('"') else {
            break;
        };
        literals.push(after[..end].to_string());
        rest = &after[end + 1..];
    }
    literals
}

fn brace_end(text: &str, at: usize) -> usize {
    let mut depth = 0_u32;
    for (offset, byte) in text[at..].bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return at + offset + 1;
                }
            }
            _ => {}
        }
    }
    text.len()
}

/// Every id a `census_resource()` fn returns (the fleet roles).
fn census_resource_literals(text: &str) -> Vec<String> {
    const DECLARATION: &str = "fn census_resource";
    let mut ids = Vec::new();
    let mut cursor = 0;
    while let Some(found) = text[cursor..].find(DECLARATION) {
        let at = cursor + found;
        let Some(body) = text[at..].find('{').map(|open| at + open) else {
            cursor = at + DECLARATION.len();
            continue;
        };
        let end = brace_end(text, body);
        ids.extend(string_literals(&text[body..end]));
        cursor = end;
    }
    ids
}

fn is_test_only(id: &str) -> bool {
    TEST_ONLY_PREFIXES
        .iter()
        .any(|prefix| id.starts_with(prefix))
        || TEST_ONLY_SUFFIXES.iter().any(|suffix| id.ends_with(suffix))
}

/// The ids the crate tree registers: production ones, `cfg(test)`-only ones, and
/// how many sources the walk read.
fn tree_ids() -> (BTreeSet<String>, BTreeSet<String>, usize) {
    let root = crates_root();
    let mut files = Vec::new();
    collect_rs_files(&root, &mut files);
    let mut production = BTreeSet::new();
    let mut test_only = BTreeSet::new();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let stripped = strip_comments(&text);
        let ids = resource_field_literals(&stripped)
            .into_iter()
            .chain(census_resource_literals(&stripped));
        for id in ids {
            if id.is_empty() || is_test_only(&id) {
                test_only.insert(id);
            } else {
                production.insert(id);
            }
        }
    }
    (production, test_only, files.len())
}

/// live-invariant gate (ADR-087 D2): watches the tree's worker-census registration surface and this crate's live table against the closed `resource` id set; not tripwire precedent.
#[test]
fn every_snapshot_resource_is_in_the_closed_set() {
    let closed: BTreeSet<&str> = REGISTERED_RESOURCE_IDS.iter().copied().collect();
    assert_eq!(
        closed.len(),
        REGISTERED_RESOURCE_IDS.len(),
        "REGISTERED_RESOURCE_IDS repeats an id",
    );
    let mut sorted = REGISTERED_RESOURCE_IDS.to_vec();
    sorted.sort_unstable();
    assert_eq!(
        sorted, REGISTERED_RESOURCE_IDS,
        "REGISTERED_RESOURCE_IDS must stay sorted, so a duplicate or a reorder is a diff",
    );

    let _runtime = degenbot_core::runtime::get_runtime();
    let live: Vec<&str> = worker_census::snapshot()
        .into_iter()
        .filter(|entry| !is_test_only(entry.resource))
        .map(|entry| entry.resource)
        .collect();
    assert!(
        live.contains(&"io_runtime_workers"),
        "the live table must carry this crate's ambient-runtime row: {live:?}",
    );
    let undeclared_live: Vec<&&str> = live.iter().filter(|id| !closed.contains(**id)).collect();
    assert!(
        undeclared_live.is_empty(),
        "an undeclared live `resource` id (add it in worker_census.rs): {undeclared_live:?}",
    );

    let (production, test_only, examined) = tree_ids();
    assert!(
        examined > 100,
        "the scan must reach the crate tree, examined {examined} sources",
    );
    assert!(
        production.contains("io_runtime_workers")
            && production.contains("fleet_solver_slots")
            && production.contains("fork_pubsub_workers"),
        "the scan must reach the registration sites: {production:?}",
    );

    let undeclared: Vec<&String> = production
        .iter()
        .filter(|id| !closed.contains(id.as_str()))
        .collect();
    assert!(
        undeclared.is_empty(),
        "a `resource` id the tree registers is not in REGISTERED_RESOURCE_IDS: {undeclared:?}",
    );
    let phantom: Vec<&&str> = closed
        .iter()
        .filter(|id| !production.contains(**id))
        .collect();
    assert!(
        phantom.is_empty(),
        "a REGISTERED_RESOURCE_IDS entry has no registration site left (delete it): {phantom:?}",
    );
    let leaked: Vec<&String> = test_only
        .iter()
        .filter(|id| closed.contains(id.as_str()))
        .collect();
    assert!(
        leaked.is_empty(),
        "a cfg(test)-only registration id must stay out of the metric's closed set: {leaked:?}",
    );
}
