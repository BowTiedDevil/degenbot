//! Standalone-core reachability gate (ADR-005 standalone claim, mechanically
//! enforced — the Tier-1 static check).
//!
//! ADR-005's standalone constraint: every core capability the `PyO3` binding
//! (`degenbot-python`, the `degenbot_rs` cdylib) delegates into MUST also be
//! reachable by a standalone Rust consumer via `cargo add degenbot` — i.e.
//! the umbrella crate must `pub use` (re-export) it. If a contributor adds a
//! `#[pyfunction]` that calls `degenbot_foo::bar` but the umbrella forgets to
//! `pub use degenbot_foo`, a standalone consumer cannot reach it: capability
//! has accreted on the Python side of the FFI boundary. This test makes that
//! drift a CI failure instead of a silent regression.
//!
//! This is the *reachability* (static) tier: it proves the symbol is
//! *resolvable* from the standalone consumer. It does not prove the FFI
//! delegation is lossless — that is the Tier-2 behavioral dual-driver gate
//! (fixture parity corpus, separate). Both tiers compose: reachability catches
//! "stranded capability" at compile time; parity catches a lossy FFI seam.
//!
//! ## How it works
//!
//! 1. Scan the binding crate (`../degenbot-python/src/**/*.rs`) for every
//!    `degenbot_<crate>::` reference — the set of core crates the Python
//!    driver actually delegates into.
//! 2. Scan this crate's `src/lib.rs` for every `pub use degenbot_<crate>`
//!    re-export — the set a standalone consumer can reach.
//! 3. Assert: every consumed core crate is re-exported OR is on the
//!    `INTENTIONALLY_NOT_STANDALONE` allowlist. An allowlist entry is a
//!    *deliberate, documented* decision (a crate with a "stays-python" /
//!    "partial" disposition per the three-layer rubric) — adding one is an
//!    explicit act that forces the contributor to record *why* the
//!    standalone consumer doesn't need it yet. An undocumented gap fails.
//!
//! ## What this does NOT catch
//!
//! - Fine symbol granularity (a specific `fn`, not just the crate). Tier-2
//!   parity tests cover individual symbols.
//! - Excess re-exports (umbrella exposes a core the binding doesn't use) —
//!   that's harmless surplus, not a standalone claim violation.

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Binding crate source root, resolved off this crate's manifest dir.
fn binding_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("degenbot-python")
        .join("src")
}

/// This umbrella crate's lib source — the re-export surface.
fn umbrella_lib() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("lib.rs")
}

/// Crates deliberately *not* re-exported by the umbrella yet, with a reason.
///
/// Each entry records the *disposition* decision (per
/// `docs/migration-guides/three-layer-transition.md`): a core crate the
/// binding reaches today but that is intentionally on the Python side of the
/// boundary until its migration slice lands. Adding an entry here is the
/// documented alternative to `pub use`-ing the crate — it forces the
/// contributor to name the migration task / rubric disposition that owns the
/// eventual standalone port, rather than letting the gap persist silently.
// NOTE: every entry here is a KNOWN reachability gap surfaced by the first
// run of this gate — the PyO3 binding reaches it, the umbrella does not yet
// `pub use` it. Each is triage-pending: the disposition is either (a) drop-in
// re-export (the crate is already standalone-by-design — removing the entry
// + adding the `pub use` is the fix), or (b) a genuine "stays-python for now"
// migration-track item that earns its place here permanently until its port
// lands. Removing an entry without adding the corresponding `pub use` makes
// this test fail — that is the debt-shrinking forcing function.
const INTENTIONALLY_NOT_STANDALONE: &[(&str, &str)] = &[];

/// Walk `dir` recursively for `.rs` files.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for ent in entries.flatten() {
        let p = ent.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

/// Extract every `degenbot_<crate>::` crate name referenced in `src`.
///
/// Matches the leading crate segment of any path expression rooted at a
/// degenbot core crate (`degenbot_pools::…`, `degenbot_db::…`, …). It does
/// NOT depend on `use`-tree form — an inline `degenbot_pools::spec_bounds::
/// narrow_v2_reserve(...)` call-site is captured too, which is the actual
/// delegation signal.
fn consumed_core_crates(src_dir: &Path) -> BTreeSet<String> {
    let mut files = Vec::new();
    rust_files(src_dir, &mut files);

    let mut crates = BTreeSet::new();
    for f in files {
        let Ok(text) = fs::read_to_string(&f) else {
            continue;
        };
        // Strip line comments so a crate name mentioned only in a comment
        // doesn't count as a real delegation (and doesn't get a false-clean
        // bill of health either).
        let stripped: String = text
            .lines()
            .map(|l| {
                if let Some(idx) = l.find("//") {
                    &l[..idx]
                } else {
                    l
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        for m in stripped
            .match_indices("degenbot_")
            .map(|(i, _)| &stripped[i..])
        {
            // Take [a-z0-9_]+ after the `degenbot_` prefix as the crate id.
            let tail = &m["degenbot_".len()..];
            let id: String = tail
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
                .collect();
            if id.is_empty() || id == "python" {
                continue; // `degenbot_python::` is the binding itself, not a core.
            }
            // Require an immediately-following `::` so bare identifiers
            // (e.g. a local var named `degenbot_foo`) don't trip it.
            let after = &tail[id.len()..];
            if after.starts_with("::") {
                crates.insert(id);
            }
        }
    }
    crates
}

/// Extract every `pub use degenbot_<crate>` (re-exported) crate from lib.rs.
fn reexported_core_crates(lib_rs: &Path) -> BTreeSet<String> {
    let Ok(text) = fs::read_to_string(lib_rs) else {
        return BTreeSet::new();
    };
    let stripped: String = text
        .lines()
        .map(|l| {
            if let Some(idx) = l.find("//") {
                &l[..idx]
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    let mut crates = BTreeSet::new();
    for m in stripped
        .match_indices("pub use degenbot_")
        .map(|(i, _)| &stripped[i + "pub use ".len()..])
    {
        let tail = &m["degenbot_".len()..];
        let id: String = tail
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
            .collect();
        if !id.is_empty() {
            crates.insert(id);
        }
    }
    crates
}

#[test]
fn every_binding_core_crate_is_reachable_from_standalone() {
    let consumed = consumed_core_crates(&binding_src());
    let reexported = reexported_core_crates(&umbrella_lib());

    let allowlist: HashSet<&str> = INTENTIONALLY_NOT_STANDALONE
        .iter()
        .map(|(c, _)| *c)
        .collect();

    let mut missing: BTreeSet<&str> = BTreeSet::new();
    for c in &consumed {
        if reexported.contains(c) {
            continue;
        }
        if allowlist.contains(c.as_str()) {
            continue;
        }
        missing.insert(c.as_str());
    }

    assert!(
        missing.is_empty(),
        "ADR-005 standalone reachability violated: the PyO3 binding reaches \
         core crate(s) the `degenbot` umbrella does NOT `pub use`, so a \
         `cargo add degenbot` standalone consumer cannot reach them. For \
         each missing crate either:\n  (a) add `pub use degenbot_<crate>;` \
         to `rust/crates/degenbot/src/lib.rs` (and the path dep to its \
         Cargo.toml), OR\n  (b) add it to INTENTIONALLY_NOT_STANDALONE in \
         this test with the migration-task / rubric disposition that owns \
         the eventual standalone port.\n\nMissing re-exports: {missing:?}\n\n\
         Consumed (binding) : {consumed:?}\nRe-exported (umbrella): \
         {reexported:?}\nAllowlisted     : {:?}",
        INTENTIONALLY_NOT_STANDALONE
            .iter()
            .map(|(c, _)| *c)
            .collect::<Vec<_>>(),
    );
}

#[test]
fn allowlist_entries_are_still_actually_consumed() {
    // Guard against stale allowlist entries: if a crate lands its standalone
    // port (gets `pub use`-ed) but is *also* still on the allowlist, the
    // allowlist is lying. Each allowlisted crate must still be consumed by
    // the binding AND still NOT re-exported (an allowlisted-AND-reexported
    // crate is contradictory).
    let consumed = consumed_core_crates(&binding_src());
    let reexported = reexported_core_crates(&umbrella_lib());

    let mut stale = Vec::new();
    let mut contradictory = Vec::new();
    for (c, _reason) in INTENTIONALLY_NOT_STANDALONE {
        if !consumed.contains(*c) {
            stale.push(*c);
        }
        if reexported.contains(*c) {
            contradictory.push(*c);
        }
    }
    assert!(
        stale.is_empty() && contradictory.is_empty(),
        "stale/redundant allowlist entries (crate no longer consumed, or now \
         re-exported — remove from INTENTIONALLY_NOT_STANDALONE):\n  stale \
         (not consumed): {stale:?}\n  contradictory (now re-exported): \
         {contradictory:?}",
    );
}
