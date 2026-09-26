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
/// The body of the first `header` block in `text` — the lines up to the brace
/// that closes it, matched by DEPTH so a nested block cannot end it early —
/// or `None` when the header is absent.
fn block_body(text: &str, header: &str) -> Option<String> {
    let mut body = String::new();
    let mut inside = false;
    let mut depth = 0_usize;
    for line in text.lines() {
        if !inside {
            if line.trim() == header {
                // The header line is the opening brace; start counting there so
                // a body line that holds no brace is not mistaken for the end.
                inside = true;
                depth = 1;
            }
            continue;
        }
        for ch in line.split("//").next().unwrap_or_default().chars() {
            match ch {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
        }
        if depth == 0 {
            return Some(body);
        }
        body.push_str(line);
        body.push('\n');
    }
    None
}

/// The declared field names of a struct body, in declaration order.
fn field_names(body: &str) -> Vec<String> {
    body.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .filter_map(|line| line.split_once(':').map(|(name, _)| name.trim().to_owned()))
        .collect()
}

/// Every `impl` block in `text`, as `(header, body)`.
///
/// Located by brace depth rather than by a hand-listed set of headers: a fixed
/// list silently skips any `impl` it did not name, and a scanner that cannot
/// see a block cannot report the policy inside it. Comment-only lines
/// contribute no depth (no doc comment in the scanned file holds a brace).
fn impl_blocks(text: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks: Vec<(String, String)> = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let header = lines[index].trim();
        if !(header.starts_with("impl ") && header.ends_with('{')) {
            index += 1;
            continue;
        }
        let mut depth = 1_usize;
        let mut body = String::new();
        index += 1;
        while index < lines.len() && depth > 0 {
            for ch in lines[index].split("//").next().unwrap_or_default().chars() {
                match ch {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
            }
            body.push_str(lines[index]);
            body.push('\n');
            index += 1;
        }
        blocks.push((header.to_owned(), body));
    }
    blocks
}

#[test]
fn one_path_identity_owner_is_declared_once() {
    // A DECLARATION census, not a wiring check: a session can reach canonical
    // path identity only through one owner type, one adapter implementation,
    // and one dedup index, because a second of any of them is a second
    // path-id space. That the live owner is actually BOUND at the production
    // boot is a behavioral fact, not a textual one, and it is proved by
    // booting the real composition (degenbot-bot's `EngineDriver`-backed
    // session-path tests assert `has_path_owner` without hand-installing).
    let crates_root = workspace_root().join("crates");
    let mut object_defs: Vec<String> = Vec::new();
    let mut adapter_impls: Vec<String> = Vec::new();
    let mut signature_indexes: Vec<String> = Vec::new();
    for_each_rust_source(&crates_root, &mut |path, text| {
        let clean = path.display().to_string().replace('\\', "/");
        for (line_number, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed == "pub struct PathObject {" {
                object_defs.push(format!("{clean}:{}", line_number + 1));
            }
            if trimmed.starts_with("impl PathObjectAdapter for ") {
                adapter_impls.push(format!("{clean}:{}", line_number + 1));
            }
            // The field DECLARATION, not the constructor initializer: the
            // type is spelled out only where the index is declared.
            if trimmed.starts_with("path_signatures: HashMap<") {
                signature_indexes.push(format!("{clean}:{}", line_number + 1));
            }
        }
    });
    assert_eq!(
        object_defs.len(),
        1,
        "the canonical path object must be declared exactly once; found {object_defs:?}"
    );
    assert!(
        object_defs[0].contains("bot_core/session_registry/path.rs"),
        "the canonical path object belongs to the session registry; found {}",
        object_defs[0]
    );
    assert_eq!(
        adapter_impls.len(),
        1,
        "the path-object adapter must be implemented exactly once; found {adapter_impls:?}"
    );
    assert!(
        adapter_impls[0].contains("arb_engine/path_objects.rs"),
        "the adapter belongs to the path-identity owner; found {}",
        adapter_impls[0]
    );
    assert_eq!(
        signature_indexes.len(),
        1,
        "the path-signature dedup index must be declared exactly once; found {signature_indexes:?}"
    );
    assert!(
        signature_indexes[0].contains("arb_engine/path_registry.rs"),
        "the path-signature dedup index belongs to the engine's path registry; found {}",
        signature_indexes[0]
    );
}

#[test]
fn the_session_registers_no_path_store_of_its_own() {
    // The session registry reaches path identity through the adapter, so the
    // module that DEFINES the canonical path object must hold no collection
    // at all: a map keyed by hop signature here would be the second store
    // that can disagree with the engine's PathRegistry about which route is
    // which. (The pool/token kinds legitimately keep maps in
    // `session_registry.rs` itself; only the path module must be collection-free.)
    let path_module =
        workspace_root().join("crates/engine/degenbot-bot/src/bot_core/session_registry/path.rs");
    let text = std::fs::read_to_string(&path_module).expect("read the path module");
    let mut offenders = Vec::new();
    for (line_number, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            continue;
        }
        for collection in ["DashMap<", "HashMap<", "BTreeMap<", "HashSet<", "BTreeSet<"] {
            if trimmed.contains(collection) {
                offenders.push(format!(
                    "{}:{line_number}: {collection}",
                    path_module.display()
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the canonical path module must hold no collection (it is reached, not stored): {offenders:?}"
    );

    // And the registry itself stores the owner as a handle, not a map of paths.
    let registry = std::fs::read_to_string(
        workspace_root().join("crates/engine/degenbot-bot/src/bot_core/session_registry.rs"),
    )
    .expect("read the session registry");
    let struct_body = block_body(&registry, "pub struct SessionObjectRegistry {")
        .expect("the session registry struct");
    let path_fields: Vec<String> = field_names(&struct_body)
        .into_iter()
        .filter(|name| name.contains("path"))
        .collect();
    assert_eq!(
        path_fields,
        vec![String::from("paths")],
        "the registry's only path member is the owner handle"
    );
    assert!(
        struct_body.contains("OnceLock<Arc<dyn PathObjectAdapter>>"),
        "the path owner is a once-installed handle; got {struct_body}"
    );
}

#[test]
fn the_canonical_path_object_carries_no_strategy_policy() {
    // Identity only: chain, the hop signature, and the owner's id. Solver
    // choice, dispatch priority, submission posture, and admission rules are
    // derived per strategy, so a field here would hand one arm's policy to
    // every other arm that trades the same route.
    let path_module =
        workspace_root().join("crates/engine/degenbot-bot/src/bot_core/session_registry/path.rs");
    let text = std::fs::read_to_string(&path_module).expect("read the path module");
    let struct_body =
        block_body(&text, "pub struct PathObject {").expect("the canonical path object");
    assert_eq!(
        field_names(&struct_body),
        vec!["chain_id", "identity", "path_id"],
        "the canonical path object declares identity fields only"
    );

    // The same rule for the WHOLE read surface, which is every `impl` block in
    // the module: a named subset would leave an accessor nobody listed free to
    // hand out one arm's policy, and the subset a gate names is exactly the
    // coverage it is credited with. The scan reaches all of them, and the
    // count assertion below fails rather than passing vacuously if the
    // extractor ever stops finding blocks.
    let policy_tokens = [
        "solver",
        "dispatch",
        "priority",
        "submission",
        "submit",
        "bid",
        "budget",
        "relay",
        "nonce",
        "retry",
        "admission",
    ];
    let blocks = impl_blocks(&text);
    let declared = text
        .lines()
        .filter(|line| line.trim_start().starts_with("impl "))
        .count();
    assert_eq!(
        blocks.len(),
        declared,
        "every `impl` in the path module must be scanned, or this gate is not the coverage it claims"
    );
    assert!(
        !blocks.is_empty(),
        "the path module has impl blocks; a scan that found none proves nothing"
    );
    let mut offenders = Vec::new();
    for (header, accessor) in &blocks {
        for (line_number, line) in accessor.lines().enumerate() {
            let trimmed = line.trim();
            if !trimmed.starts_with("pub") {
                continue;
            }
            for token in policy_tokens {
                if trimmed.contains(token) {
                    offenders.push(format!("{header} +{line_number}: {trimmed}"));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the canonical path surface must not expose strategy policy: {offenders:?}"
    );
}
