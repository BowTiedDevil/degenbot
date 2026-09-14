//! KAHU5W acceptance gate: `std::env::var` / `env::var_os` must appear ONLY
//! inside the degenbot-config loader (the one env-reading site) plus
//! explicitly enumerated infra + test-only stances.
//!
//! A stray re-introduced env read fails this test loudly with the file and
//! the variable name, so config drift cannot sneak back in (Q6 fail-closed
//! posture: those keys must load through `BotConfig` instead).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Files outside degenbot-config where env reads are permitted, with the
/// exact names allowed per file. A NEW env read in a library file must add
/// an entry here (with justification) or be migrated onto `BotConfig`.
fn allowed() -> &'static BTreeMap<&'static str, &'static [&'static str]> {
    static MAP: OnceLock<BTreeMap<&'static str, &'static [&'static str]>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = BTreeMap::new();
        // (1) Infra — OS/tooling signal variables no static schema can own.
        // (SMTH6M: TOKIO_WORKER_THREADS was retired — the ambient runtime is
        // sized from the cgroup budget via the typed `runtime.io_workers`
        // key, and the legacy name is rejected by the loader.)
        m.insert(
            "crates/degenbot-python/src/python_log_layer.rs",
            &[
                "HOME",
                "RUST_LOG",
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                // Logging-infra tooling signal (same class as RUST_LOG): the
                // stderr fmt-layer mirror gate. Output plumbing, not config.
                "DEGENBOT_LOG_FMT",
            ][..],
        );
        // The ADR-043 §4 filter branch: RUST_LOG is the tracing-convention
        // override (it wins verbatim on every sink; the typed
        // `telemetry.log_level`/`telemetry.diag` knobs apply otherwise).
        // Output plumbing, not config (same class as the Python layer's).
        m.insert("crates/degenbot-bot/src/telemetry.rs", &["RUST_LOG"][..]);
        // (2) Dev-test knobs in library code (offline tooling only).
        m.insert(
            "crates/degenbot-bot/src/profiling.rs",
            &["HOTPATH_SHUTDOWN_MS"][..],
        );
        // 5WCRWZ T7: the CH-hop clamp (and its CLAMP_MARGIN dev-margin
        // override) moved out of the deleted solver_dispatch.rs onto the
        // cycle's solve_cycle.rs home. The probe-fixture names below are
        // already enumerated on the executor_ab_probe entry.
        m.insert(
            "crates/degenbot-bot/src/arb_engine/solve_cycle.rs",
            &["CLAMP_MARGIN"][..], // dev wei-margin override (cl-hop clamp lab)
        );
        m.insert(
            // 5WCRWZ T6: the probe fixture loader moved out of solver_dispatch
            // into its own test-only module (corpus path override, cfg(test)).
            "crates/degenbot-bot/src/arb_engine/executor_ab_probe.rs",
            &["DEGENBOT_PROBE_FIXTURE"][..],
        );
        m.insert(
            "crates/degenbot-bot/src/arb_engine/epoch_delta_parity.rs",
            &["DBENCH_CAPTURES"][..], // offline parity fixture dir
        );
        m.insert(
            "crates/degenbot-executor/src/grammar_walker/shapes/two_hop_v4_led.rs",
            &["T1_CAPTURE"][..], // executor shape dev-capture gate
        );
        m.insert(
            "crates/degenbot-executor/src/grammar_walker/shapes/two_hop_seed_v4.rs",
            &["T1_CAPTURE"][..], // executor shape dev-capture gate
        );
        // (3) Test-only stances inside src: the parent test sets these to
        // drive the CHILD test binary (no config loader in the harness).
        m.insert(
            "crates/degenbot-bot/src/bot_core/block_pump.rs",
            &[
                "DEGENBOT_SELF_ABORT_TEST",
                "DEGENBOT_DESYNC_TEST_STANCE",
                // Reviewer note (3): DEGENBOT_RPC_WS_CHAINID_<chain_id> is
                // INTENTIONALLY dynamic (non-schema); the suffix is the
                // numeric chain id. Documented next to SCHEMA.
                "DEGENBOT_RPC_WS_CHAINID_1",
            ][..],
        );
        m.insert(
            "crates/degenbot-python/src/diagnostics/thread_registry.rs",
            &["DEGENBOT_STATE_LOCK_DIAG"][..], // test stance (feature-gated)
        );
        // (4) The ADR-043 §5 retired-name boot detection: reads the env by
        // iterating a closed list (`RETIRED_ENV_NAMES`), so the name is
        // computed at the call site. Detection only — no alias is honored.
        m.insert("crates/degenbot-core/src/telemetry.rs", &["name"][..]);
        m.insert(
            "crates/degenbot-python/build.rs",
            // The build-receipt work (1e1c0ddf7): the build script locates
            // the repo receipt file itself (a build-time path, not config).
            &["CARGO_MANIFEST_DIR", "DEGENBOT_BUILD_NUMBER_FILE"][..],
        );
        m
    })
}

#[expect(
    clippy::expect_used,
    reason = "test helper: CARGO_MANIFEST_DIR depth is fixed at compile time; loud failure beats a dummy root"
)]
fn workspace_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        // CARGO_MANIFEST_DIR = rust/crates/degenbot-config
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .expect("manifest two levels above crates/")
    })
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Skip non-source trees so a scan of the repo root sees only this
            // checkout's crate sources:
            //   - `target/` carries vendored crate copies that mirror crate
            //     sources and double-report;
            //   - hidden dirs are tooling/worktree state (`.git/`, and `.pi/`
            //     per-branch checkouts of this same repo, pre-cutover surfaces
            //     included);
            //   - `autoresearch/` is the gitignored agent-harness scratch tree
            //     holding full repo snapshots (same sources at an older
            //     revision), which would otherwise double-report every hit.
            let fname = entry.file_name();
            let name = fname.to_str().unwrap_or("");
            if name == "target"
                || name == "autoresearch"
                || name == "node_modules"
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

/// Extract the env variable name from a call like `std::env::var("NAME")`.
#[must_use]
fn env_var_name(line: &str) -> Option<String> {
    // Anchor the scan AFTER the env::var call token so a preceding
    // initializer like `if let Ok(raw) = ...` cannot shadow the parse.
    let pos = line.find("env::var")?;
    let rest = &line[pos + "env::var".len()..];
    let rest = rest.trim_start_matches(|c: char| c == '_' || c.is_ascii_alphanumeric());
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('(')?;
    let rest = rest.trim_start();
    if let Some(q) = rest.strip_prefix('"') {
        let name: String = q.chars().take_while(|c| *c != '"').collect();
        return (!name.is_empty()).then_some(name);
    }
    // Computed name (a const), e.g. env::var(ENV_VAR): report the identifier.
    let ident: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if ident == "name" {
        // The loader's EnvVars seam (env::var(name)) is sanctioned.
        return Some("name".into());
    }
    (!ident.is_empty()).then_some(ident)
}

#[test]
fn no_stray_env_reads_outside_the_config_loader() {
    let root = workspace_root();
    let mut files = Vec::new();
    collect_rs_files(root, &mut files);
    assert!(
        files
            .iter()
            .any(|p| p.ends_with("degenbot-config/src/loader.rs")),
        "workspace scan must include the config crate itself"
    );

    let mut violations: Vec<String> = Vec::new();
    for path in &files {
        let rel = path
            .strip_prefix(root)
            .map(|p| p.display().to_string())
            .unwrap_or_default()
            // When cargo runs from rust/ the manifest parents land on the
            // repo root; normalize to a crate-relative path either way.
            .strip_prefix("rust/")
            .map_or_else(
                || {
                    path.strip_prefix(root)
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                },
                str::to_string,
            );
        // The one sanctioned env-reading crate.
        if rel.starts_with("crates/degenbot-config/") {
            continue;
        }
        // Test-only stances: every tests/ and examples/ directory file,
        // wherever it sits in the tree - a crate-nested `crates/*/tests/`
        // path OR a workspace-level `examples/`/`tests/` leading component
        // (e.g. `examples/settlement_bot/`). Matching on path components
        // rather than a `/tests/` substring keeps the leading-component
        // case from slipping through.
        let is_test_or_example = Path::new(rel.as_str())
            .components()
            .any(|c| matches!(c.as_os_str().to_str(), Some("tests" | "examples")));
        if is_test_or_example {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let allow = allowed().get(rel.as_str());
        for (idx, line) in text.lines().enumerate() {
            if !line.contains("env::var") {
                continue;
            }
            // Whole-line comments documenting historical reads are fine.
            if line.trim_start().starts_with("//") {
                continue;
            }
            let name = env_var_name(line)
                .or_else(|| {
                    // Multi-line call: the name may sit on one of the next
                    // two lines (fn arg on its own line).
                    text.lines().skip(idx + 1).take(2).find_map(|l| {
                        let t = l.trim_start().trim_end_matches(',').trim();
                        let inner = t
                            .strip_prefix('"')
                            .and_then(|r| r.chars().position(|c| c == '"'))
                            .map(|p| &t[1..p]);
                        inner.filter(|s| !s.is_empty()).map(str::to_string)
                    })
                })
                .unwrap_or_else(|| "<computed>".into());
            let permitted = allow.is_some_and(|names| names.contains(&name.as_str()));
            if !permitted {
                violations.push(format!("{rel}:{}: {name}", idx + 1));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "stray env reads outside degenbot-config (route through BotConfig or add
         an enumerated, justified entry in this test):\n{}",
        violations.join("\n")
    );
}

/// The loader is the ONLY env-reading surface inside degenbot-config.
#[test]
fn config_crate_env_reads_confined_to_the_loader() {
    let crate_dir = workspace_root().join("crates/degenbot-config");
    let mut files = Vec::new();
    collect_rs_files(&crate_dir, &mut files);
    for path in files {
        let rel = path
            .strip_prefix(workspace_root())
            .map(|p| p.display().to_string())
            .unwrap_or_default()
            .strip_prefix("rust/")
            .map_or_else(|| path.display().to_string(), str::to_string);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (idx, line) in text.lines().enumerate() {
            if line.contains("env::var") && !line.trim_start().starts_with("//") {
                assert!(
                    rel.ends_with("loader.rs"),
                    "degenbot-config env read outside loader.rs: {rel}:{}",
                    idx + 1
                );
            }
        }
    }
}
