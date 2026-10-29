//! The central-matcher census drift gate (ADR-073 D6, C6 item 2): the moved
//! per-family arms may never return to the central matchers. Two properties
//! are pinned here, because the C3 lesson showed a literal-only census is
//! blind to wrapper dispatches (`match registry.pools.get` hiding a moved
//! arm behind a helper):
//!
//! 1. **the literal `match entry` census** — one number, pinned at its
//!    post-C5 value (`rg 'match entry' rust/crates -g '!*tests*'`), so a
//!    reintroduced central match row moves the count and fails here;
//! 2. **the design-math absence property** — no family design-math
//!    identifier (the families' compute leaves and the retired wrapper
//!    names) may appear at the three central sites
//!    (`pools::simulate_swap`, `substrate::swap_simulation`,
//!    `substrate::resolve`) outside tests and comments.
//!
//! Both halves fail loudly with the file and the offending line, and both
//! are ratchets: a deletion keeps them green, a reintroduction cannot stay.

use std::path::{Path, PathBuf};

use std::sync::OnceLock;

/// The pinned census value (manager pattern:
/// `rg 'match entry' rust/crates -g '!*tests*'`; the one excluded hit is the
/// cli-core exchange-arm test fixture, not a pool matcher).
///
/// ADR-073 D2 permits at most one dispatch row per family at each central site,
/// and FEWER is the direction the extraction wants: the concentrated-liquidity
/// rows left this census once the family door took over their reader and merge
/// plumbing. The entry-side tag table is not a central matcher either —
/// `BotState::pool_family` reads the typed family and renders
/// `RegisteredPoolFamily::tag`, so the only family-name spelling lives on the
/// enum.
const PINNED_MATCH_ENTRY_CENSUS: usize = 20;

/// The reintroduction marker the census counts (shared by the workspace scan
/// and the synthetic-fixture negative probe, so the probe witnesses the same
/// predicate the pin enforces).
const MATCH_ENTRY_TOKEN: &str = "match entry";

/// The door surface of the family read modules: the four intent doors, the
/// projection/calc doors the central rows legitimately call, and their
/// per-design arms' public names. Any OTHER top-level `pub fn` under
/// `families/` is a non-door export: it must be covered by
/// [`BANNED_DESIGN_MATH`] (the banned token derived from the export) or
/// classified as a door here — knowingly, via the
/// `the_families_export_only_the_door_fns` meta-test.
const DOOR_FNS: &[&str] = &[
    "read_swap",
    "read_swap_override",
    "capture_hop",
    "capture_aerodrome_hop",
    "observe",
    "get_dy",
    "calc_token_amount",
    "calc_withdraw_one_coin",
];

/// The family design-math identifiers that may never appear at the central
/// sites: the compute leaves the family read modules own, the retired
/// central-math helpers, and the retired wrapper names (the C3/C5
/// hidden-arm classes). `bpt_idx` — the hop payload's field — is NOT banned:
/// the resolve row's DTO assembly may name its own payload fields; the ban
/// targets the math (`skip_bpt`), not the data.
const BANNED_DESIGN_MATH: &[&str] = &[
    // Curve stableswap leaf chain (the family module's math).
    "stableswap_get_",
    // Balancer leaves + the BPT math helper.
    "pow_up",
    "pow_down",
    "skip_bpt",
    // The recorded TODO(extract-per-family-arms) shape.
    "calc_out_given_in",
    // Per-family compute arms (the Solidly read door's design leaves and the
    // reserve-pair closed forms) — one token covers the `calc_exact_*` family.
    "calc_exact",
    // Retired wrapper names (the hidden-arm classes C3/C5 deleted).
    "simulate_aerodrome",
    "simulate_balancer",
    "simulate_curve_stableswap",
];

/// The three central sites, relative to `rust/` (the workspace root).
const CENTRAL_SITES: &[&str] = &[
    "crates/foundation/degenbot-pools/src/simulate_swap.rs",
    "crates/foundation/degenbot-substrate/src/swap_simulation.rs",
    "crates/foundation/degenbot-substrate/src/resolve",
];

#[expect(
    clippy::expect_used,
    reason = "test helper: CARGO_MANIFEST_DIR depth is fixed at compile time; loud failure beats a dummy root"
)]
fn workspace_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        // CARGO_MANIFEST_DIR = rust/crates/foundation/degenbot-substrate; the
        // scan root is the rust/ workspace (three levels up).
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .expect("role-grouped manifest is under the rust/ workspace")
    })
}

/// One `.rs` source under `rust/crates`, excluding the test/example/bench
/// path components the manager census glob excludes (`!*tests*`).
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name();
            let name = name.to_str().unwrap_or("");
            if name == "target"
                || matches!(name, "tests" | "examples" | "benches")
                || name.starts_with('.')
            {
                continue;
            }
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Line is whole-line prose (a comment or doc comment): a historical reference
/// in prose is not a reintroduction.
fn is_comment_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//")
}

/// Inside a `#[cfg(test)] mod ... { ... }` region: fixture content is not a
/// central-matcher reintroduction (the C5 review's line-context rule,
/// encoded). Depth-tracked between lines like the companion half of the
/// config env gate.
fn cfg_test_regions(lines: &[&str]) -> Vec<bool> {
    let mut in_test = vec![false; lines.len()];
    let mut depth = 0usize;
    let mut active = false;
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if !active && trimmed.starts_with("#[cfg(test)]") {
            // The mod header may sit on this line or the next; both open a
            // brace at depth-1 from here.
            let rest = lines.get(idx + 1).copied().unwrap_or("");
            if rest.contains("mod ") || trimmed.contains("mod ") {
                active = true;
            }
        }
        in_test[idx] = active;
        for c in line.chars() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth = depth.saturating_sub(1);
                    if active && depth == 0 {
                        active = false;
                    }
                }
                _ => {}
            }
        }
    }
    in_test
}

/// The census predicate over one source text: the number of reintroduced
/// `match entry` rows.
fn match_entry_hits_in_text(text: &str) -> usize {
    text.lines()
        .filter(|line| line.contains(MATCH_ENTRY_TOKEN))
        .count()
}

/// The banned-identifier scan over one source text: every banned token in
/// CODE (a comment or a `#[cfg(test)]` region is prose/fixtures, not a
/// reintroduction). Line reports carry the relative path so a red half
/// names its site.
fn banned_token_hits_in_text(rel: &str, text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let in_test = cfg_test_regions(&lines);
    let mut violations = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if is_comment_line(line) || in_test[idx] {
            continue;
        }
        for token in BANNED_DESIGN_MATH {
            if line.contains(token) {
                violations.push(format!("{rel}:{}: {token}", idx + 1));
            }
        }
    }
    violations
}

/// The literal `match entry` census across the workspace sources.
fn match_entry_census(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    collect_rs_files(&root.join("crates"), &mut files);
    let mut hits = Vec::new();
    for path in &files {
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .display()
            .to_string();
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for (idx, line) in text.lines().enumerate() {
            if line.contains(MATCH_ENTRY_TOKEN) {
                hits.push(format!("{rel}:{}: {}", idx + 1, line.trim()));
            }
        }
    }
    hits
}

/// The design-math identifiers present at the central sites, outside tests
/// and comments.
fn design_math_at_central_sites(root: &Path) -> Vec<String> {
    let mut violations = Vec::new();
    for site in CENTRAL_SITES {
        let path = root.join(site);
        let meta = std::fs::metadata(&path);
        let paths: Vec<PathBuf> = match meta {
            Ok(m) if m.is_dir() => {
                let mut v = Vec::new();
                collect_rs_files(&path, &mut v);
                v
            }
            _ => vec![path.clone()],
        };
        for path in paths {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            violations.extend(banned_token_hits_in_text(&rel, text.as_str()));
        }
    }
    violations
}

/// live-invariant gate (ADR-087 D2): watches the live central-matcher `match entry` dispatch rows against the pinned census; not tripwire precedent.
#[test]
fn the_central_match_entry_census_is_pinned() {
    let root = workspace_root();
    let hits = match_entry_census(root);
    assert!(
        hits.len() >= PINNED_MATCH_ENTRY_CENSUS / 2,
        "the scan went dark (workspace sources unreadable?): {} hits",
        hits.len()
    );
    assert_eq!(
        hits.len(),
        PINNED_MATCH_ENTRY_CENSUS,
        "the literal `match entry` census drifted from the pinned value (a central         match row moved or reappeared — ADR-073 D2 permits at most one dispatch         row per family at each central site):
{}",
        hits.join("
")
    );
}

/// live-invariant gate (ADR-087 D2): watches the live central matcher sites for family design-math leaves that belong behind the family doors; not tripwire precedent.
#[test]
fn no_family_design_math_at_the_central_sites() {
    let root = workspace_root();
    let violations = design_math_at_central_sites(root);
    assert!(
        violations.is_empty(),
        "family design math reappeared at the central matchers (it belongs          behind the family read modules' doors — ADR-073 D1/D2):
{}",
        violations.join("
")
    );
}

/// A positive control for the scan itself: the banned tokens are the ones the
/// families actually own, so a renamed leaf that stops matching is a failing
/// table rather than a quiet gap. Each token must still exist at its family
/// home.
#[test]
fn the_banned_tokens_are_still_the_families_leaves() {
    let root = workspace_root();
    // The LIVE leaves must exist somewhere in the families/ tree.
    let live = [
        "stableswap_get_",
        "skip_bpt",
        "calc_exact_in_stable_solidly",
    ];
    let mut families_text = String::new();
    let mut files = Vec::new();
    collect_rs_files(
        &root
            .join("crates/foundation/degenbot-pools/src/families")
            .clone(),
        &mut files,
    );
    for path in &files {
        if let Ok(text) = std::fs::read_to_string(path) {
            families_text.push_str(&text);
        }
    }
    assert!(
        !files.is_empty(),
        "the families tree must be readable for the control"
    );
    for token in live {
        assert!(
            families_text.contains(token),
            "the banned token {token} no longer names a family leaf — update              BANNED_DESIGN_MATH knowingly (the gate must not go dark)"
        );
    }
}

/// The families' export surface is door-only: every top-level `pub fn` under
/// `families/` must be a named door. A NON-door export is exactly the shape
/// that can quietly resurface at the central matchers, so it must be covered
/// by `BANNED_DESIGN_MATH` (the banned token derived from the export name)
/// before it can appear there unflagged — this meta-test is that
/// derivation's ratchet.
/// live-invariant gate (ADR-087 D2): watches every top-level `pub fn` the live families tree exports against `DOOR_FNS`; not tripwire precedent.
#[test]
fn the_families_export_only_the_door_fns() {
    let root = workspace_root();
    let mut files = Vec::new();
    collect_rs_files(
        &root
            .join("crates/foundation/degenbot-pools/src/families")
            .clone(),
        &mut files,
    );
    assert!(
        !files.is_empty(),
        "the families tree must be readable for the meta-test"
    );
    let mut offenders = Vec::new();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for (idx, line) in text.lines().enumerate() {
            // Column-0 `pub fn`s only: inherent/trait-impl methods and
            // tree-internal (`pub(super)`) arms are not export surface.
            let rest = if let Some(rest) = line.strip_prefix("pub fn ") {
                rest
            } else if let Some(rest) = line.strip_prefix("pub const fn ") {
                rest
            } else {
                continue;
            };
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let is_door = DOOR_FNS.contains(&name.as_str());
            let is_banned = BANNED_DESIGN_MATH.iter().any(|token| name.contains(token));
            if !is_door && !is_banned {
                offenders.push(format!(
                    "{}:{}: {name}",
                    path.strip_prefix(root).unwrap_or(path).display(),
                    idx + 1
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the families tree exports non-door functions (classify each as a \
         door in DOOR_FNS, or derive a BANNED_DESIGN_MATH token from its \
         name so a central-matcher reintroduction cannot hide behind it):
{}",
        offenders.join(
            "
"
        )
    );
}

/// The negative probe: the gate's red path witnessed on a synthetic fixture,
/// permanently (a gate whose red path is only a one-time manual probe is not
/// a gate). The same line-context rules the workspace scan enforces — a
/// banned token in CODE goes red, the same token in a comment or inside a
/// `#[cfg(test)]` region stays green — and the census predicate moves on a
/// reintroduced row.
/// live-invariant gate (ADR-087 D2): watches the live design-math scan's own red path, demonstrated on a synthetic fixture; not tripwire precedent.
#[test]
fn the_gate_goes_red_on_a_synthetic_fixture() {
    // Red: a banned design-math leaf called in code at a "central site".
    let red = "
use degenbot_math::balancer::weighted_math;
fn row() -> u256 {
    weighted_math::pow_up(x, y)
}
";
    let hits = banned_token_hits_in_text("synthetic/red.rs", red);
    assert!(
        !hits.is_empty(),
        "a banned design-math token in code must trip the gate"
    );
    assert!(
        hits[0].contains("pow_up"),
        "the hit names the token: {hits:?}"
    );

    // Green: the same token in prose and inside a `#[cfg(test)]` region —
    // the C5 line-context rules must keep the gate quiet there.
    let clean = "
// a historical note mentions pow_up in prose
#[cfg(test)]
mod tests {
    fn fixture() {
        let _ = pow_up;
    }
}
";
    assert!(
        banned_token_hits_in_text("synthetic/clean.rs", clean).is_empty(),
        "comment and cfg(test) occurrences are not reintroductions"
    );

    // The census half: the counted predicate is `match entry`, and a
    // synthetic reintroduced row registers exactly one hit.
    assert_eq!(match_entry_hits_in_text("match entry {"), 1);
    assert_eq!(match_entry_hits_in_text("match other {"), 0);
}
