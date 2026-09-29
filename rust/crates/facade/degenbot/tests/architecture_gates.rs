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

/// One `cargo tree` spawn covering every named member, resolved at the
/// workspace manifest.
fn batched_cargo_tree(members: &[String]) -> String {
    let mut command = Command::new("cargo");
    command
        .arg("tree")
        .arg("--manifest-path")
        .arg(manifest_flag());
    for member in members {
        command.arg("-p").arg(member);
    }
    let out = command.output().expect("cargo tree spawn");
    assert!(
        out.status.success(),
        "cargo tree for {members:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("cargo tree output is utf8")
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

/// Every `` Python|Python's `symbol` [noun] <verb> `` occurrence in a comment
/// line, as `(symbol, following_lowercase_noun, next_word)`.
///
/// Returned ungated so the CALLER decides: the same phrase is a definition in
/// one sentence and a parity note in another, and which one it is depends on
/// the verb, not the phrase.
fn python_phrases(line: &str) -> Vec<(String, String, String)> {
    let bytes: Vec<char> = line.chars().collect();
    let mut found = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if index + 6 > bytes.len() || !bytes[index..].starts_with(&['P', 'y', 't', 'h', 'o', 'n']) {
            index += 1;
            continue;
        }
        let mut cursor = index + 6;
        if bytes.get(cursor) == Some(&'\'') {
            cursor += 1;
            if bytes.get(cursor) == Some(&'s') {
                cursor += 1;
            }
        }
        if bytes.get(cursor) != Some(&' ') {
            index += 1;
            continue;
        }
        while bytes.get(cursor) == Some(&' ') {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&'`') {
            index += 1;
            continue;
        }
        let symbol_start = cursor + 1;
        let Some(symbol_end) = bytes[symbol_start..].iter().position(|c| *c == '`') else {
            index += 1;
            continue;
        };
        let symbol: String = bytes[symbol_start..symbol_start + symbol_end]
            .iter()
            .collect();
        cursor = symbol_start + symbol_end + 1;

        // An optional lowercase noun travels with the phrase ("the Python
        // `X` enum uses"); it is part of what names the symbol, so the test
        // artifact exemption can see it.
        let mut look = cursor;
        while look < bytes.len() && bytes[look] == ' ' {
            look += 1;
        }
        let noun_start = look;
        while look < bytes.len() && bytes[look].is_ascii_lowercase() {
            look += 1;
        }
        let noun: String = bytes[noun_start..look].iter().collect();

        // The next word is the verb, when the phrase names the agent.
        while look < bytes.len() && bytes[look] == ' ' {
            look += 1;
        }
        let verb_start = look;
        while look < bytes.len() && (bytes[look].is_ascii_alphanumeric() || bytes[look] == '_') {
            look += 1;
        }
        let verb: String = bytes[verb_start..look].iter().collect();
        found.push((symbol, noun, verb));
        index = cursor;
    }
    found
}

#[test]
fn workspace_membership_is_exact_and_role_grouped() {
    const EXPECTED_NAMES: [&str; 35] = [
        "degenbot",
        "degenbot-aave",
        "degenbot-abi",
        "degenbot-arbitrage",
        "degenbot-batch-executor",
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
        "degenbot-substrate",
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
    let members: Vec<String> = core_member_names()
        .into_iter()
        .filter(|name| name != "degenbot_rs") // the PyO3 binding layer, pyo3 by construction
        .collect();
    assert!(
        members.len() >= 20,
        "member census drifted suspiciously low: {} members",
        members.len()
    );
    // One spawn settles a clean census: feature unification across the
    // selected members can only ADD edges, so a batch without a `pyo3 v` line
    // proves every member's default-features closure pyo3-free. A dirty batch
    // pays the per-member attribution loop — the only exact semantics, since
    // any workspace-wide resolution (metadata included) false-flags everyone:
    // the binding layer enables a `pyo3` feature on every core crate.
    if batched_cargo_tree(&members)
        .lines()
        .any(|l| l.contains("pyo3 v"))
    {
        for member in &members {
            if cargo_tree_of(member).lines().any(|l| l.contains("pyo3 v")) {
                violations.push(member.clone());
            }
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
        object_defs[0].contains("degenbot-substrate/src/session_registry/path.rs"),
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
        workspace_root().join("crates/foundation/degenbot-substrate/src/session_registry/path.rs");
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
        workspace_root().join("crates/foundation/degenbot-substrate/src/session_registry/mod.rs"),
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
        workspace_root().join("crates/foundation/degenbot-substrate/src/session_registry/path.rs");
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

/// The position seam has exactly ONE observer implementation, and it lives in
/// the lending integration.
///
/// A second implementation is a second answer for one identity from two
/// sources, which is the drift the session registry exists to remove — the
/// same reasoning as the single path-identity owner. The census is a
/// DECLARATION check over the shipped sources; a test double under a `tests`
/// path is not a second reader, and a session actually reaching this observer is
/// behavioral and is proved where a consumer installs it.
#[test]
fn one_position_observer_implementation_is_declared_once() {
    let crates_root = workspace_root().join("crates");
    let mut impls: Vec<String> = Vec::new();
    for_each_rust_source(&crates_root, &mut |path, text| {
        let clean = path.display().to_string().replace('\\', "/");
        if clean.contains("/tests/") || clean.ends_with("/tests.rs") {
            return;
        }
        for (line_number, line) in text.lines().enumerate() {
            if line.trim().starts_with("impl PositionObserver for ") {
                impls.push(format!("{clean}:{}", line_number + 1));
            }
        }
    });
    assert_eq!(
        impls.len(),
        1,
        "the position observer must be implemented exactly once; found {impls:?}"
    );
    assert!(
        impls[0].contains("integrations/degenbot-aave/src/positions.rs"),
        "the observer belongs to the lending integration that owns the domain; found {}",
        impls[0]
    );
}

/// Neither the seam's own module nor the session's position surface holds a
/// collection.
///
/// A position is a perishable READ, so a map here would be a cache the session
/// never asked for: it would hide freshness from a strategy about to act on a
/// position, and a failed read would have a remembered value to fall back on —
/// the two failures the seam exists to prevent. (The pool/token kinds
/// legitimately keep maps in `session_registry.rs` itself; only the position
/// modules must be collection-free.)
#[test]
fn the_session_registers_no_position_store_of_its_own() {
    let modules = [
        workspace_root().join("crates/foundation/degenbot-core/src/session_positions.rs"),
        workspace_root()
            .join("crates/foundation/degenbot-substrate/src/session_registry/position.rs"),
    ];
    let mut offenders = Vec::new();
    for module in &modules {
        let read_error = format!("read {}", module.display());
        let text = std::fs::read_to_string(module).expect(&read_error);
        for (line_number, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            for collection in ["DashMap<", "HashMap<", "BTreeMap<", "HashSet<", "BTreeSet<"] {
                if trimmed.contains(collection) {
                    offenders.push(format!("{}:{line_number}: {collection}", module.display()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the position seam must stay collection-free: {offenders:?}"
    );
}

/// The seam adds NO dependency edge between a lending integration and the
/// engine, in either direction.
///
/// Both edges are wrong for a different reason: an integration that depends on
/// the engine inverts the layering, and an engine that depends on an integration
/// makes the core own a domain it does not have. The seam therefore sits in the
/// layer both already share, and this gate is what keeps a future "just add the
/// dep" from quietly restoring either edge.
#[test]
fn the_position_seam_adds_no_lending_edge_to_the_engine_or_the_core() {
    let mut offenders = Vec::new();
    for (crate_dir, forbidden) in [
        ("foundation/degenbot-core", "degenbot-aave"),
        ("engine/degenbot-bot", "degenbot-aave"),
        ("engine/degenbot-strategy", "degenbot-aave"),
    ] {
        let manifest = workspace_root()
            .join("crates")
            .join(crate_dir)
            .join("Cargo.toml");
        let read_error = format!("read {}", manifest.display());
        let text = std::fs::read_to_string(&manifest).expect(&read_error);
        if text.contains(forbidden) {
            offenders.push(format!("{crate_dir} -> {forbidden}"));
        }
    }
    // And the other direction: the lending integration may not reach the engine.
    let aave_manifest = workspace_root().join("crates/integrations/degenbot-aave/Cargo.toml");
    let aave_read_error = format!("read {}", aave_manifest.display());
    let aave_text = std::fs::read_to_string(&aave_manifest).expect(&aave_read_error);
    if aave_text.contains("degenbot-bot") {
        offenders.push(String::from("degenbot-aave -> degenbot-bot"));
    }
    assert!(
        offenders.is_empty(),
        "the position seam must not add a dependency edge to a lending integration: {offenders:?}"
    );
}

/// The layer the seam sits in is one BOTH sides already depend on — the property
/// that lets the integration implement the observer and the session hold a
/// handle to it with no new edge at all.
///
/// If either manifest stops naming `degenbot-core`, the seam is unreachable from
/// that side and the "no new edge" claim above is no longer what holds it up.
#[test]
fn the_position_seam_sits_in_the_layer_both_sides_depend_on() {
    for crate_dir in [
        "integrations/degenbot-aave",
        "engine/degenbot-bot",
        "foundation/degenbot-core",
    ] {
        let manifest = workspace_root()
            .join("crates")
            .join(crate_dir)
            .join("Cargo.toml");
        let read_error = format!("read {}", manifest.display());
        let text = std::fs::read_to_string(&manifest).expect(&read_error);
        if crate_dir != "foundation/degenbot-core" {
            assert!(
                text.contains("degenbot-core"),
                "{crate_dir} must already depend on degenbot-core, or the position seam needs a new edge"
            );
        }
    }
    let seam = workspace_root().join("crates/foundation/degenbot-core/src/session_positions.rs");
    assert!(
        seam.exists(),
        "the position seam must be declared in degenbot-core, the layer both sides share"
    );
}
/// A core-crate PUBLIC item may not define its own contract by pointing at a
/// Python-side symbol, and this gate polices the narrowest decidable form of
/// that: a doc comment attached to a `pub` item in which a Python-qualified
/// symbol is the semantic AGENT of a definitional verb ("the 1-based wire code
/// the Python `PoolProbe` enum uses"), so the core value's meaning is whatever
/// the Python side happens to say.
///
/// The existing pyo3-free gate polices the IMPORT, which is why a reverted
/// `PoolFamily::probe_wire_code` carrying the doc "The 1-based wire code the
/// Python `PoolProbe` enum uses" passed it: nothing wrote `use pyo3`.
///
/// Why this is not a ban on the word "Python": the core docs are full of
/// legitimate mentions, and they use Python as a PARITY TARGET ("mirrors the
/// Python oracle"), never as the DEFINITION. A word ban would report ~331
/// pre-existing sites. The discriminator here is the GRAMMATICAL ROLE of the
/// Python phrase, and it is deliberately the decidable subset of the real
/// rule, not the whole of it:
///
///   - the item is `pub` (a private helper's comment is nobody's contract);
///   - the Python phrase is `Python`/`Python's` + a backticked symbol;
///   - the symbol is not a TEST artifact — a Python test suite is a witness
///     that a value agrees, never the definition of one, and this exemption is
///     load-bearing rather than cosmetic: `degenbot-aave/src/analysis.rs` reads
///     "tolerance the Python `test_core.py` suite uses", which is
///     grammatically indistinguishable from the rejected probe and entirely
///     legitimate;
///   - the next word is a definitional verb, i.e. the Python symbol is the one
///     doing the defining.
///
/// KNOWN LIMIT, stated rather than papered over: this catches the definitional
/// shape only. A core comment that defines a value by a Python symbol without
/// one of these verbs ("mirrors the Python `X` numbering", "as in the Python
/// `X`") still passes, as does a contract-source comment on a non-`pub` item.
/// The full rule is a judgment about which entity holds a definition, and no
/// purely lexical gate can decide it without failing the same-shaped
/// legitimate comments above — the alternative measured here was a call-graph
/// rule (no public core API used only by the binding shell), which fails on 48
/// pre-existing legitimate APIs. Treat this as a real subset of coverage, not
/// the whole class.
/// In the RUST CORE, a map keyed by one of the session's four canonical
/// identity types lives in the session registry and NOWHERE ELSE.
///
/// SCOPE, stated because the name used to hide it: this gate is Rust-only. It
/// walks `for_each_rust_source` over `rust/crates`, so it says nothing about
/// the Python side, where address-keyed dedup state actually still lives in
/// this repo. The Python surface is policed by review plus the removal of the
/// maps that were authority; the maps that remain there are memos and caches
/// that cannot mint identity (ADR-064 accounts for all five, and
/// `retired_python_dedup_maps_do_not_reappear` holds the narrow decidable
/// part).
///
/// Identity types are the one key the registry owns outright: a second map
/// keyed by `PoolIdentity`, `TokenIdentity`, `PathIdentity`, or
/// `PositionIdentity` is a second answer to "is this the same object?" — a
/// second id space — which is the exact failure the registry exists to remove,
/// and it is invisible to a type system because the two maps have the same key
/// type. All FOUR are listed, not the two this gate shipped with: a list that
/// omits a kind is not a weaker rule, it is a hole, and the path and position
/// kinds were unproved until a probe outside the registry showed the gate
/// passing a second path store.
///
/// The rule is deliberately scoped to the registry's OWN identity types rather
/// than to address-keyed maps generally. An address-keyed map is not
/// automatically a shadow identity: the core holds 48 pre-existing ones that
/// are caches, ledgers, and indices with a different lifetime (executor
/// encoders, the grammar ledger, the connector index, pool-ingress tick maps,
/// the sim anchor). A gate that flagged all of them would be reporting the
/// codebase rather than this rule. A map keyed by the registry's identity
/// types, by contrast, is unambiguously a claim about session object identity,
/// and there are none outside the registry today.
#[test]
fn no_second_map_in_the_rust_core_is_keyed_by_a_session_identity_type() {
    let identity_keys = [
        "PoolIdentity",
        "TokenIdentity",
        "PathIdentity",
        "PositionIdentity",
    ];
    let collections = [
        "DashMap<",
        "HashMap<",
        "BTreeMap<",
        "HashSet<",
        "BTreeSet<",
        "RwLock<HashMap<",
        "Mutex<HashMap<",
    ];
    let registry_module = "degenbot-substrate/src/session_registry";

    let mut violations = Vec::new();
    let crates_root = workspace_root().join("crates");
    for_each_rust_source(&crates_root, &mut |path, text| {
        let clean = path.display().to_string().replace('\\', "/");
        if clean.contains("/tests/") || clean.contains("/benches/") {
            return;
        }
        for (line_number, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            let Some(collection) = collections.iter().find(|c| trimmed.contains(**c)) else {
                continue;
            };
            // The KEY type is the first generic argument, not any type in the
            // line: `HashMap<u64, (V3PoolIdentity, V3PoolState)>` is keyed by a
            // pool ID whose VALUE mentions an identity, and matching the whole
            // line flags that unrelated type.
            let after = &trimmed[trimmed
                .find(collection)
                .map_or(0, |i| i + collection.len() - 1)..];
            let Some(key_type) = after
                .trim_start()
                .trim_start_matches('<')
                .split(',')
                .next()
                .map(|k| k.trim().trim_start_matches('&').trim_start_matches('*'))
            else {
                continue;
            };
            // The FINAL path segment names the type, so a fully-qualified key
            // (`HashMap<crate::substrate::session_registry::PathIdentity, u8>`)
            // is caught too. Comparing the whole key type missed exactly that
            // spelling, which is the spelling an out-of-registry caller reaches
            // for when the registry is not in scope unqualified — so the gate
            // would still have passed the probe written to prove it.
            let named = key_type.rsplit("::").next().unwrap_or(key_type);
            if !identity_keys
                .iter()
                .any(|k| named == *k || key_type.starts_with(&format!("{k}::")))
            {
                continue;
            }
            // The registry's own tables are the sanctioned home.
            if clean.contains(registry_module) {
                continue;
            }
            violations.push(format!("{clean}:{}: {trimmed}", line_number + 1));
        }
    });
    assert!(
        violations.is_empty(),
        "only the session registry may key a map by a session identity type: {violations:?}"
    );
}

/// The five Python dedup maps this cutover deleted must not come back.
///
/// The Rust sibling gate above is Rust-only — it walks `*.rs` — and the Python
/// side is where address-keyed dedup state actually still lives in this repo,
/// so the task's "no new private dedup map outside the session registry"
/// criterion has no automated half on that side. A general Python text gate
/// cannot decide the real rule (a map that CANNOT mint identity is legitimate,
/// and there are five), but THIS subset is decidable and false-positive-free:
/// these five names were the maps that WERE authority, they were removed
/// because get-or-create replaced them, and no future change may legitimately
/// reintroduce any of them. Rejecting a name that must never return is not a
/// judgment call, so there is nothing here to tune.
///
/// KNOWN LIMIT, not papered over: this forbids the five RETIRED names, not
/// dedup maps in general. A new Python map under a fresh name, or an
/// address-keyed memo of registry-issued handles, is still not caught here —
/// see ADR-064 for the five maps that legitimately remain and why none of them
/// can mint identity. The Python surface beyond these names is policed by
/// review, which is a weaker instrument than a gate and is recorded as such.
///
/// Deliberately scoped to `src/degenbot/`, not `tests/`: the regression tests
/// that PROVE the removal still name these identifiers in prose (they have to,
/// to document what is gone), and a test witness is not a second owner.
#[test]
fn retired_python_dedup_maps_do_not_reappear() {
    let retired = [
        "_v2_keys",
        "_v3_keys",
        "_v4_keys",
        "_v3_inflight",
        "_v4_inflight",
    ];

    let src = repo_root().join("src/degenbot");

    assert!(
        src.exists(),
        "the Python source root moved; this gate's walk would vacuously pass"
    );

    let mut violations = Vec::new();
    let mut walk = vec![src];
    while let Some(dir) = walk.pop() {
        for entry in std::fs::read_dir(&dir).expect("read_dir src/degenbot") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            let ft = entry.file_type().expect("file type");
            if ft.is_dir() {
                walk.push(path);
            } else if ft.is_file() && path.extension().is_some_and(|e| e == "py") {
                let text = std::fs::read_to_string(&path).expect("read python source");
                for (line_number, line) in text.lines().enumerate() {
                    for name in retired {
                        if line.contains(name) {
                            violations.push(format!(
                                "{}:{}: {name}",
                                path.display(),
                                line_number + 1
                            ));
                        }
                    }
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "retired Python dedup maps must not reappear under src/degenbot (ADR-064): {violations:?}"
    );
}

#[test]
fn core_public_docs_do_not_define_contracts_through_python_symbols() {
    const DEFINITIONAL_VERBS: [&str; 14] = [
        "uses",
        "define",
        "defines",
        "means",
        "determines",
        "enumerates",
        "numbers",
        "indexes",
        "expects",
        "requires",
        "produces",
        "assigns",
        "encodes",
        "corresponds",
    ];
    // A Python test file witnesses that a core value agrees; it never defines
    // one. See the note on `analysis.rs` above.
    const TEST_ARTIFACT_MARKERS: [&str; 6] = ["test_", "_test", "conftest", ".py", "spec", "test"];

    let mut violations = Vec::new();
    for role in ["foundation", "engine", "integrations", "facade"] {
        let dir = workspace_root().join("crates").join(role);
        for_each_rust_source(&dir, &mut |path, text| {
            let clean = path.display().to_string().replace('\\', "/");
            if clean.contains("/tests/") || clean.contains("/benches/") {
                return;
            }
            let lines: Vec<&str> = text.lines().collect();
            for (index, line) in lines.iter().enumerate() {
                let trimmed = line.trim();
                if !(trimmed.starts_with("///") || trimmed.starts_with("//!")) {
                    continue;
                }
                // Only a `pub` item publishes a contract. Skip past the
                // remaining doc lines, attributes, and blanks to reach it.
                let mut next = index + 1;
                while let Some(candidate) = lines.get(next) {
                    let t = candidate.trim();
                    let continues = t.is_empty()
                        || t.starts_with("#[")
                        || t.starts_with("//")
                        || t.starts_with("#!");
                    if !continues {
                        break;
                    }
                    next += 1;
                }
                let Some(item) = lines.get(next) else {
                    continue;
                };
                if !item.trim_start().starts_with("pub ") {
                    continue;
                }
                for (symbol, noun, after_verb) in python_phrases(trimmed) {
                    if TEST_ARTIFACT_MARKERS
                        .iter()
                        .any(|marker| symbol.contains(marker) || noun.contains(marker))
                    {
                        continue;
                    }
                    if DEFINITIONAL_VERBS.contains(&after_verb.as_str()) {
                        violations.push(format!(
                            "{clean}:{}: the Python `{symbol}` is the agent of `{after_verb}`",
                            index + 1
                        ));
                    }
                }
            }
        });
    }
    assert!(
        violations.is_empty(),
        "a core public item must not define its contract through a Python-side symbol: {violations:?}"
    );
}
