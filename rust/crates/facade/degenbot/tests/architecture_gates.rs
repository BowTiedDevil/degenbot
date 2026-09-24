//! Architecture invariant gates.
//!
//! The `check-*` invariant scans are cargo tests on the umbrella crate: the
//! crate list derives from `cargo metadata`, the scans run under every
//! `cargo test` track, and the justfile recipes are thin aliases
//! (ADR-043 §10-style precedent:
//! `degenbot-python/tests/gil_state_write_concurrency.rs`).

#![expect(clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    // The role directory adds one level above the former flat crate layout.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("manifest dir has rust/ ancestor")
        .to_path_buf()
}

fn repo_root() -> PathBuf {
    workspace_root()
        .parent()
        .expect("rust/ has a repo parent")
        .to_path_buf()
}

fn manifest_flag() -> PathBuf {
    workspace_root().join("Cargo.toml")
}

/// `cargo tree` of `member` resolved at the workspace manifest (default features).
fn cargo_tree_of(member: &str) -> String {
    let out = Command::new("cargo")
        .arg("tree")
        .arg("--manifest-path")
        .arg(manifest_flag())
        .arg("-p")
        .arg(member)
        .output()
        .expect("cargo tree spawn");
    assert!(
        out.status.success(),
        "cargo tree -p {member} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("cargo tree output is utf8")
}

fn workspace_packages() -> Vec<(String, String)> {
    let out = Command::new("cargo")
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--manifest-path")
        .arg(manifest_flag())
        .arg("--no-deps")
        .output()
        .expect("cargo metadata spawn");
    assert!(out.status.success(), "cargo metadata failed");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("cargo metadata json");
    v["packages"]
        .as_array()
        .expect("packages array")
        .iter()
        .map(|package| {
            (
                package["name"].as_str().expect("package name").to_owned(),
                package["manifest_path"]
                    .as_str()
                    .expect("package manifest path")
                    .to_owned(),
            )
        })
        .collect()
}

/// Workspace member names whose crate name starts with `degenbot`.
fn core_member_names() -> Vec<String> {
    workspace_packages()
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("degenbot"))
        .collect()
}

/// Walk `dir` recursively, calling `f` on every `*.rs` file path + content.
fn for_each_rust_source(dir: &Path, f: &mut dyn FnMut(&Path, &str)) {
    for entry in std::fs::read_dir(dir).expect("read_dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        let ft = entry.file_type().expect("file type");
        if ft.is_dir() {
            for_each_rust_source(&path, f);
        } else if ft.is_file() && path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).expect("read rust source");
            f(&path, &text);
        }
    }
}

#[test]
fn workspace_membership_is_exact_and_role_grouped() {
    const EXPECTED_NAMES: [&str; 33] = [
        "degenbot",
        "degenbot-aave",
        "degenbot-abi",
        "degenbot-arbitrage",
        "degenbot-bot",
        "degenbot-cli",
        "degenbot-cli-core",
        "degenbot-config",
        "degenbot-core",
        "degenbot-db",
        "degenbot-decoders",
        "degenbot-eventhub",
        "degenbot-execution",
        "degenbot-execution-sample",
        "degenbot-executor",
        "degenbot-fork",
        "degenbot-ingestion",
        "degenbot-math",
        "degenbot-order-index",
        "degenbot-pathfinding",
        "degenbot-pool-updater",
        "degenbot-pools",
        "degenbot-price",
        "degenbot-rpc",
        "degenbot-runs",
        "degenbot-settlement-bot-example",
        "degenbot-simulation",
        "degenbot-solvers",
        "degenbot-strategy",
        "degenbot-submission",
        "degenbot-uniswap",
        "degenbot-workers",
        "degenbot_rs",
    ];
    const ROLES: [&str; 5] = ["facade", "foundation", "engine", "integrations", "shells"];

    let mut packages = workspace_packages();
    packages.sort();
    let names: Vec<&str> = packages.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names, EXPECTED_NAMES,
        "workspace package identities changed"
    );

    let crates_root = workspace_root().join("crates");
    for (_, manifest) in &packages {
        let path = Path::new(manifest);
        if path.starts_with(&crates_root) {
            let relative = path.strip_prefix(&crates_root).expect("role-grouped crate");
            let mut components = relative.components();
            let role = components.next().expect("role component").as_os_str();
            assert!(
                ROLES.iter().any(|candidate| role == *candidate),
                "crate escaped the explicit role directories: {}",
                path.display()
            );
        } else {
            assert!(
                path.starts_with(workspace_root().join("examples")),
                "non-role package escaped the examples directory: {}",
                path.display()
            );
        }
    }
}

#[test]
fn cli_shell_names_only_allowlisted_externals() {
    // ADR-051 D2: the argv facade may name workspace members + the small
    // argv/sink plumbing allowlist (reads the manifest's dep tables).
    let manifest = workspace_root().join("crates/shells/degenbot-cli/Cargo.toml");
    let text = std::fs::read_to_string(&manifest).expect("read cli manifest");
    let allow = [
        "clap",
        "indicatif",
        "tokio",
        "tracing",
        "tracing-subscriber",
    ];
    let mut offenders = Vec::new();
    let mut in_deps = false;
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if matches!(
            t,
            "[dependencies]" | "[build-dependencies]" | "[dev-dependencies]"
        ) {
            in_deps = true;
            continue;
        }
        if t.starts_with('[') {
            in_deps = false;
            continue;
        }
        if !in_deps || t.starts_with('#') || t.is_empty() {
            continue;
        }
        let Some((name, _)) = t.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || t.contains("workspace") && t.contains("true") {
            continue;
        }
        if !allow.contains(&name) {
            offenders.push(format!("{}: {name}", i + 1));
        }
    }
    assert!(
        offenders.is_empty(),
        "degenbot-cli may list only workspace members + argv/sink plumbing; offenders: {offenders:?}"
    );
}

#[test]
fn core_crates_are_pyo3_free_under_default_features() {
    // The pyo3-free charter (AGENTS.md). The binding crate `degenbot_rs` is the
    // ONLY member that
    // may touch pyo3 — it is the PyO3 wrapper layer.
    let mut violations = Vec::new();
    let members = core_member_names();
    assert!(
        members.len() >= 20,
        "member census drifted suspiciously low: {} members",
        members.len()
    );
    for member in members {
        if member == "degenbot_rs" {
            continue; // the PyO3 binding layer, pyo3 by construction
        }
        let tree = cargo_tree_of(&member);
        if tree.lines().any(|l| l.contains("pyo3 v")) {
            violations.push(member);
        }
    }
    assert!(
        violations.is_empty(),
        "core crates pulling pyo3 under default features: {violations:?}"
    );
}

#[test]
fn cli_core_is_clap_and_indicatif_free() {
    // ADR-051 D2: the argv facade owns clap + indicatif; the semantics crate
    // never touches them.
    let tree = cargo_tree_of("degenbot-cli-core");
    for offender in ["clap", "clap_derive", "clap_builder", "indicatif"] {
        let bad = tree
            .lines()
            .filter(|l| {
                l.trim_start_matches(|c: char| !c.is_ascii_alphanumeric()) == offender
                    || l.starts_with(&format!("{offender} v"))
                    || l.contains(&format!(" {offender} v")) && !l.contains("degenbot")
            })
            .collect::<Vec<_>>();
        assert!(
            bad.is_empty(),
            "degenbot-cli-core pulls {offender}: {bad:?}"
        );
    }
}

#[test]
fn one_engine_impl_block() {
    // exactly one `impl ArbitrageEngine` block, in arb_engine/mod.rs.
    let bot_src = workspace_root().join("crates/engine/degenbot-bot/src");
    let mut hits: Vec<(String, usize)> = Vec::new();
    for_each_rust_source(&bot_src, &mut |path, text| {
        let clean_path = path.display().to_string().replace('\\', "/");
        for (i, line) in text.lines().enumerate() {
            let t = line.trim();
            if t.starts_with("impl ArbitrageEngine") && t.ends_with('{') {
                hits.push((clean_path.clone(), i + 1));
            }
        }
    });
    assert_eq!(
        hits.len(),
        1,
        "expected exactly 1 impl ArbitrageEngine block; found {hits:?}"
    );
    assert!(
        hits[0].0.ends_with("arb_engine/mod.rs"),
        "the sole engine impl block must live in arb_engine/mod.rs; found {}",
        hits[0].0
    );

    // The Python wrapper may name the compat pyclass string exactly once.
    let py_src = workspace_root().join("crates/shells/degenbot-python/src");
    let mut py_hits: Vec<String> = Vec::new();
    for_each_rust_source(&py_src, &mut |path, text| {
        if text.contains("ArbitrageEngine") {
            for line in text.lines().filter(|l| l.contains("ArbitrageEngine")) {
                py_hits.push(format!("{}: {}", path.display(), line.trim()));
            }
        }
    });
    assert_eq!(
        py_hits.len(),
        1,
        "degenbot-python must name ArbitrageEngine only as the pyclass compat string; found {py_hits:?}"
    );
    assert!(
        py_hits[0].contains("name = \"ArbitrageEngine\","),
        "the single mention must be the pyclass name string; got {}",
        py_hits[0]
    );
}

#[test]
fn no_inner_allow_attributes() {
    // `#![allow]` inner attributes are forbidden; reasoned outer #[allow]/#[expect]
    // is the sanctioned form.
    let mut violations = Vec::new();
    for_each_rust_source(&workspace_root().join("crates"), &mut |path, text| {
        for (i, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("#![allow(") {
                violations.push(format!("{}:{}", path.display(), i + 1));
            }
        }
    });
    assert!(
        violations.is_empty(),
        "inner #![allow] forbidden (use #[expect] or a reasoned outer #[allow]): {violations:?}"
    );
}

#[test]
fn cmd_executor_cutover_symbols_are_absent_from_strategy_callers() {
    let strategy_root = workspace_root().join("crates/engine/degenbot-strategy");
    let retired_symbols = [
        "compose_candidate",
        "build_candidate_calldata",
        "ComposeReject",
    ];
    let mut violations = Vec::new();

    for directory in ["src", "tests"] {
        for_each_rust_source(&strategy_root.join(directory), &mut |path, text| {
            for (line_number, line) in text.lines().enumerate() {
                for symbol in retired_symbols {
                    if line.contains(symbol) {
                        violations.push(format!(
                            "{}:{}: {symbol}",
                            path.display(),
                            line_number + 1
                        ));
                    }
                }
            }
        });
    }

    assert!(
        violations.is_empty(),
        "retired cmd_executor cutover symbols remain in canonical strategy callers/tests: {violations:?}"
    );
}

#[test]
fn no_alembic_references() {
    // ADR-052 D6: the in-tree Alembic chain is retired; no Python file may
    // reference it.
    let src = repo_root().join("src/degenbot");
    assert!(
        !src.join("migrations").exists(),
        "src/degenbot/migrations still exists (retired per ADR-052 D6)"
    );
    let mut violations = Vec::new();
    let mut walk = Vec::new();
    walk.push(src);
    while let Some(dir) = walk.pop() {
        for entry in std::fs::read_dir(&dir).expect("read_dir src") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            let ft = entry.file_type().expect("file type");
            if ft.is_dir() {
                walk.push(path);
            } else if ft.is_file() && path.extension().is_some_and(|e| e == "py") {
                let text = std::fs::read_to_string(&path).expect("read python source");
                for (i, line) in text.lines().enumerate() {
                    if line.to_ascii_lowercase().contains("alembic") {
                        violations.push(format!("{}:{}", path.display(), i + 1));
                    }
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "Alembic references remain in src/**/*.py (retired per ADR-052 D6): {violations:?}"
    );
}
