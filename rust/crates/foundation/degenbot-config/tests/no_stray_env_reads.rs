//! Acceptance gate for ADR-062 D7: `degenbot-config` is the one env-reading
//! owner, on both sides of the language seam. `std::env::var` / `env::var_os`
//! may appear only inside its loader, and `os.environ` / `os.getenv` /
//! `os.environb` may appear in the Python companion only at enumerated sites.
//!
//! Both halves share one allowlist, one report format, and one message
//! vocabulary, so a key cannot be migrated out of the Rust schema and into
//! Python unnoticed. A stray re-introduced read fails this test loudly with
//! the file and the variable name (Q6 fail-closed posture: those keys must
//! load through `BotConfig` instead).
//!
//! The allowlist is the ratchet: a deletion has to keep this test green, so a
//! read that comes back cannot stay.
//!
//! ## Known limits of the companion scanner
//!
//! The Python half is a hand-rolled lexer, not a parser, so its silence is only
//! as good as its mode tracking. Two measured gaps are worth reading before
//! trusting a green run:
//!
//! - **A file that opens a triple-quoted literal and never closes it silences
//!   every later read in that file.** The unterminated literal leaves the walk
//!   stuck in string mode, so the rest of the file reads as prose and the gate
//!   passes — a syntax error fails the gate *open*. Silence therefore means
//!   "no read was recognised", not "no read exists"; the shape table pins the
//!   behaviour.
//! - **Module and name aliasing are invisible.** `import os as o` followed by
//!   `o.environ.get(...)`, and `from os import environ` followed by
//!   `environ[...]`, are not detected. That is a naming limit, not a lexing
//!   one: the read is recognised, its spelling is not, because the scanner
//!   matches the literal `os.environ` / `os.getenv` tokens.
//!
//! A replacement by an interpreter-backed `ast` walk was evaluated; the
//! comparison and the decision to keep this scanner are recorded in
//! `docs/architecture/env-read-gate-scanner-evaluation.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Files outside degenbot-config where env reads are permitted, with the
/// exact names allowed per file. A NEW env read in a library file must add
/// an entry here (with justification) or be migrated onto `BotConfig`.
///
/// One map holds both languages. Rust sources are keyed relative to `rust/`
/// (the workspace root the Rust scan normalizes to) and Python companion
/// sources relative to the repository root, so the two key spaces cannot
/// collide and neither half needs its own copy of this list.
///
/// A name is reported as written at the call site, so a read whose name is
/// computed is enumerated by the identifier it computes from -- the loader's
/// `env::var(name)` seam and the companion's retired-key identifiers alike.
/// An expression
/// no name is reducible from is reported `<computed>`, which no file may be
/// enumerated for by accident.
fn allowed() -> &'static BTreeMap<&'static str, &'static [&'static str]> {
    static MAP: OnceLock<BTreeMap<&'static str, &'static [&'static str]>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = BTreeMap::new();
        // (1) Infra — OS/tooling signal variables no static schema can own.
        // (TOKIO_WORKER_THREADS was retired — the ambient runtime is
        // sized from the cgroup budget via the typed `runtime.io_workers`
        // key, and the legacy name is rejected by the loader.)
        m.insert(
            "crates/shells/degenbot-python/src/python_log_layer.rs",
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
        m.insert(
            "crates/engine/degenbot-bot/src/telemetry.rs",
            &["RUST_LOG"][..],
        );
        // Build scripts: cargo-provided manifest dir + the build-receipt
        // override (ADR-051 D2 build identity). Compile-time plumbing, not
        // runtime config.
        m.insert(
            "crates/shells/degenbot-cli/build.rs",
            &["CARGO_MANIFEST_DIR", "DEGENBOT_BUILD_NUMBER_FILE"][..],
        );
        // (2) Dev-test knobs in library code (offline tooling only).
        m.insert(
            "crates/engine/degenbot-bot/src/profiling.rs",
            &["HOTPATH_SHUTDOWN_MS"][..],
        );
        // the CH-hop clamp (and its CLAMP_MARGIN dev-margin
        // override) moved out of the deleted solver_dispatch.rs onto the
        // cycle's solve_cycle.rs home. The probe-fixture names below are
        // already enumerated on the executor_ab_probe entry.
        m.insert(
            "crates/engine/degenbot-bot/src/arb_engine/solve_cycle.rs",
            &["CLAMP_MARGIN"][..], // dev wei-margin override (cl-hop clamp lab)
        );
        m.insert(
            // the probe fixture loader moved out of solver_dispatch
            // into its own test-only module (corpus path override, cfg(test)).
            "crates/engine/degenbot-bot/src/arb_engine/executor_ab_probe.rs",
            &["DEGENBOT_PROBE_FIXTURE"][..],
        );
        m.insert(
            "crates/engine/degenbot-bot/src/arb_engine/epoch_delta_parity.rs",
            &["DBENCH_CAPTURES"][..], // offline parity fixture dir
        );
        m.insert(
            "crates/foundation/degenbot-executor/src/grammar_walker/shapes/two_hop_v4_led.rs",
            &["T1_CAPTURE"][..], // executor shape dev-capture gate
        );
        m.insert(
            "crates/foundation/degenbot-executor/src/grammar_walker/shapes/two_hop_seed_v4.rs",
            &["T1_CAPTURE"][..], // executor shape dev-capture gate
        );
        m.insert(
            // (3) stance: the reset-lock flock test spawns a child test
            // binary that takes the real lock before the parent kills it —
            // cross-process coordination, not operator config.
            "crates/shells/degenbot-cli-core/src/aave.rs",
            &["RESET_LOCK_TEST_DB", "RESET_LOCK_TEST_READY"][..],
        );
        // (3) Test-only stances inside src: the parent test sets these to
        // drive the CHILD test binary (no config loader in the harness).
        m.insert(
            "crates/engine/degenbot-bot/src/bot_core/block_pump.rs",
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
            "crates/shells/degenbot-python/src/diagnostics/thread_registry.rs",
            &["DEGENBOT_STATE_LOCK_DIAG"][..], // test stance (feature-gated)
        );
        // (4) The ADR-043 §5 retired-name boot detection: reads the env by
        // iterating a closed list (`RETIRED_ENV_NAMES`), so the name is
        // computed at the call site. Detection only — no alias is honored.
        m.insert(
            "crates/foundation/degenbot-core/src/telemetry.rs",
            &["name"][..],
        );
        // (5) The ADR-052 D1 heal-at-open killswitch. A DB-open toggle read at
        // the open path itself (NOT bot config): a `cargo add degenbot-db`
        // consumer must be able to pin the pre-D1 posture without a BotConfig,
        // and the name is a DB-open contract, not a tunable. Unset => heal on;
        // only an explicit falsey word disables. (The scanner reports the
        // computed-name const identifier, hence "AUTO_HEAL_ENV".)
        m.insert(
            "crates/foundation/degenbot-db/src/migrate.rs",
            &["AUTO_HEAL_ENV"][..],
        );
        m.insert(
            "crates/shells/degenbot-python/build.rs",
            // The build-receipt work (1e1c0ddf7): the build script locates
            // the repo receipt file itself (a build-time path, not config).
            &["CARGO_MANIFEST_DIR", "DEGENBOT_BUILD_NUMBER_FILE"][..],
        );
        // The backrun sidecar's configuration now loads through the typed
        // per-ecosystem backrun facets; the one
        // remaining env read is the pre-typed executor-owner fallback the
        // facet's `operator` key documents.
        m.insert(
            "crates/engine/degenbot-strategy/src/backrun_driver/driver_loop.rs",
            &["EXECUTOR_OWNER_ADDRESS"][..],
        );

        insert_python_entries(&mut m);

        m
    })
}

/// The Python companion's entries, inserted into the same map the Rust scan
/// reads. A separate builder only so each stays readable: the key spaces are
/// disjoint, so the two cannot collide and the lookup stays single.
fn insert_python_entries(map: &mut BTreeMap<&'static str, &'static [&'static str]>) {
    // (6) The Python companion, which is a driver over the Rust core and
    // not a second config authority. Every entry here is a name the typed
    // schema does not own, read at a site that predates the cascade, or a
    // tooling posture the cascade does not express.
    //
    // The companion owns two classes of env read:
    //   - refusals: `name` is the element of the closed retired-knob list
    //     `src/degenbot/runner/config.py::_RETIRED_SHELL_KNOBS` (presence of
    //     any list member fails the config load, and nothing would consume a
    //     value), and
    //     `_RETIRED_INJECTION_KEY` is the retired bare spelling of the
    //     injection stance, refused everywhere; the honored spelling is the
    //     declared `simulation.inject_executor_code` key, which arrives
    //     through the verdict.
    //   - operator/executor identity, which the typed schema does not
    //     declare: the launch shell exports it from `bot.env`, and `build`
    //     reads it so a live run signs with the operator's key. Live mode
    //     additionally refuses a placeholder key the repository publishes.
    map.insert(
        "src/degenbot/runner/config.py",
        &[
            "name",
            "_RETIRED_INJECTION_KEY",
            "OPERATOR_ADDRESS",
            "OPERATOR_PRIVATE_KEY",
            "EXECUTOR_CONTRACT_ADDRESS",
            "INJECTED_EXECUTOR_ADDRESS",
            "EXECUTOR_OWNER_ADDRESS",
            "EXECUTOR_RUNTIME",
        ][..],
    );
    // The console level for the Python side of the log pipeline, the same
    // class as the Rust `RUST_LOG` tooling signal: output plumbing, not
    // config. It is read by `base_log_level()` when the base level is
    // applied -- at import, and again for any caller that re-applies it --
    // rather than frozen into a module constant.
    map.insert("src/degenbot/logging.py", &["DEGENBOT_DEBUG"][..]);
}

#[expect(
    clippy::expect_used,
    reason = "test helper: CARGO_MANIFEST_DIR depth is fixed at compile time; loud failure beats a dummy root"
)]
fn workspace_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        // CARGO_MANIFEST_DIR = rust/crates/foundation/degenbot-config
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .expect("role-grouped manifest is under the rust/ workspace")
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
        if rel.starts_with("crates/foundation/degenbot-config/") {
            continue;
        }
        // Test-only stances: every tests/, examples/, and benches/
        // directory file, wherever it sits in the tree - a crate-nested
        // `crates/*/tests/` path OR a workspace-level
        // `examples/`/`tests/` leading component (e.g.
        // `examples/settlement_bot/`). Matching on path components rather
        // than a `/tests/` substring keeps the leading-component case from
        // slipping through.
        let is_test_or_example = Path::new(rel.as_str()).components().any(|c| {
            matches!(
                c.as_os_str().to_str(),
                Some("tests" | "examples" | "benches")
            )
        });
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
    let crate_dir = workspace_root().join("crates/foundation/degenbot-config");
    let mut files = Vec::new();
    collect_rs_files(&crate_dir, &mut files);
    assert!(
        files.iter().any(|path| path.ends_with("src/loader.rs")),
        "config production scan must include src/loader.rs"
    );
    for path in files {
        // Test harnesses may read their own regeneration controls. This gate
        // protects production configuration surfaces, not test fixtures.
        if path.components().any(|component| {
            matches!(
                component.as_os_str().to_str(),
                Some("tests" | "examples" | "benches")
            )
        }) {
            continue;
        }
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

// ---------------------------------------------------------------------------
// The Python companion half of the same rule
// ---------------------------------------------------------------------------

/// Byte length of the environment-read token starting at `at`, if any.
///
/// Longest-first, so `os.environb` is not truncated to `os.environ`; the caller
/// requires a non-identifier byte after the token, so a token that would
/// truncate to a half-name is a non-match rather than a wrong name.
#[must_use]
fn env_token_len(bytes: &[u8], at: usize) -> Option<usize> {
    const TOKENS: [&[u8]; 3] = [b"os.environb", b"os.environ", b"os.getenv"];
    if at > 0 && (is_ident_byte(bytes[at - 1]) || bytes[at - 1] == b'.') {
        return None;
    }
    TOKENS
        .iter()
        .find(|token| {
            bytes.len() >= at + token.len()
                && bytes[at..].starts_with(token)
                && !is_ident_byte(bytes[at + token.len()])
        })
        .map(|token| token.len())
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// What a companion source is inside at a byte offset.
///
/// `Code` is either top-level code or an f-string hole; `hole` tells the two
/// apart so a `}` in a hole ends the hole instead of being read as an
/// operator.
#[derive(Clone, Copy)]
enum PyMode {
    Code {
        hole: bool,
    },
    Str {
        quote: u8,
        triple: bool,
        fstring: bool,
    },
}

/// A line-at-a-time walk of one Python source, as code and string.
///
/// The mode stack is state *between* lines, not within one: a string is not a
/// line, because a triple-quoted literal spans line breaks and an f-string hole
/// spans them inside one. So a line that begins inside a string is string on
/// that line too — prose stays prose on every line a docstring spans, and a
/// read interpolated into a message is a read on every line its hole spans.
///
/// The walk fails open on an unterminated triple-quoted literal: nothing pops
/// the string mode, so every later line reads as string and its reads go
/// unreported. The file header states the limit and the shape table pins it.
struct PythonWalk {
    /// Innermost last: an f-string hole is a code region nested in a string.
    modes: Vec<PyMode>,
}

impl Default for PythonWalk {
    fn default() -> Self {
        Self {
            modes: vec![PyMode::Code { hole: false }],
        }
    }
}

impl PythonWalk {
    /// The environment names this line of the source reads.
    #[must_use]
    fn reads(&mut self, line: &str) -> Vec<String> {
        let bytes = line.as_bytes();
        let mut reads = Vec::new();
        let mut i = 0usize;
        while i < bytes.len() {
            let Some(mode) = self.modes.last().copied() else {
                break;
            };
            let b = bytes[i];
            match mode {
                PyMode::Code { hole } => {
                    if b == b'#' {
                        // A comment ends at the newline, but the region it sits
                        // in does not: a hole or a triple-quoted string spans
                        // the break, so the stack stands as it is.
                        break;
                    }
                    if hole && b == b'}' && self.modes.len() > 1 {
                        self.modes.pop();
                        i += 1;
                        continue;
                    }
                    if b == b'\'' || b == b'"' {
                        let triple = bytes.len() >= i + 3 && bytes[i + 1] == b && bytes[i + 2] == b;
                        let fstring = i > 0
                            && matches!(bytes[i - 1], b'f' | b'F')
                            && (i < 2 || !is_ident_byte(bytes[i - 2]));
                        self.modes.push(PyMode::Str {
                            quote: b,
                            triple,
                            fstring,
                        });
                        i += usize::from(triple) * 2 + 1;
                        continue;
                    }
                    match env_token_len(bytes, i) {
                        Some(len) => {
                            reads.push(python_read_name(line, i, i + len));
                            i += len;
                        }
                        None => i += 1,
                    }
                }
                PyMode::Str {
                    quote,
                    triple,
                    fstring,
                } => {
                    if b == b'\\' {
                        i += 2;
                        continue;
                    }
                    // `{{` and `}}` are the f-string spellings of a literal
                    // brace, so a hole opens at an unpaired brace only.
                    if fstring && matches!(b, b'{' | b'}') && bytes.get(i + 1) == Some(&b) {
                        i += 2;
                        continue;
                    }
                    if fstring && b == b'{' {
                        self.modes.push(PyMode::Code { hole: true });
                        i += 1;
                        continue;
                    }
                    if b == quote
                        && (!triple
                            || (bytes.len() >= i + 3
                                && bytes[i + 1] == quote
                                && bytes[i + 2] == quote))
                    {
                        self.modes.pop();
                        i += if triple { 3 } else { 1 };
                        continue;
                    }
                    // A `}` with no hole open is text, and a hole that began on
                    // an earlier line closes in the code mode above.
                    i += 1;
                }
            }
        }
        reads
    }
}

/// The environment names a whole source reads, line by line, in walk order.
#[must_use]
fn python_env_reads(lines: &[&str]) -> Vec<String> {
    let mut walk = PythonWalk::default();
    lines.iter().flat_map(|line| walk.reads(line)).collect()
}

/// The name the environment read whose token spans `[at, after)` denotes.
///
/// A call or subscript argument is read as written, so a computed read is
/// reported by the identifier it computes from — the convention the Rust half
/// already uses for the loader's `env::var(name)` seam. An expression that is
/// not reducible to a name is `<computed>`, the same sentinel the Rust half
/// reports, so a whole-mapping read is loud and can never be enumerated by
/// accident.
#[must_use]
fn python_read_name(line: &str, at: usize, after: usize) -> String {
    const COMPUTED: &str = "<computed>";
    if let Some(name) = membership_name(line, at) {
        return name;
    }
    let rest = line[after..].trim_start();
    // `os.environ.get(...)` is the call form; handing the mapping to any other
    // attribute reads every name, so nothing is reducible.
    let opened = if let Some(after_dot) = rest.strip_prefix('.') {
        let attr: String = after_dot
            .chars()
            .take_while(|c| is_ident_char(*c))
            .collect();
        if attr != "get" {
            return COMPUTED.into();
        }
        after_dot[attr.len()..].trim_start()
    } else {
        rest
    };
    let Some(argument) = opened
        .strip_prefix('[')
        .or_else(|| opened.strip_prefix('('))
    else {
        return COMPUTED.into();
    };
    arg_name(argument)
}

/// The name an `in` test names, when `at` is the right operand: `"NAME" in
/// os.environ` tests one name without calling anything.
///
/// `for <name> in os.environ.get(...)` is a loop, not a test, and the keyword
/// is the only thing that tells the two apart in a line scanner.
fn membership_name(line: &str, at: usize) -> Option<String> {
    let tested = line[..at].trim_end().strip_suffix("in")?;
    // `in` must stand alone, so `within os.environ` is not a test.
    if tested.ends_with(is_ident_char) {
        return None;
    }
    let (name, before) = split_trailing_expression(tested);
    if before.trim_end().ends_with("for") {
        return None;
    }
    Some(name)
}

/// The name the trailing expression of `text` denotes, and the text before it.
fn split_trailing_expression(text: &str) -> (String, &str) {
    const COMPUTED: &str = "<computed>";
    let trimmed = text.trim_end();
    let Some(last) = trimmed.chars().last() else {
        return (COMPUTED.into(), trimmed);
    };
    if matches!(last, '"' | '\'') {
        let head = &trimmed[..trimmed.len() - last.len_utf8()];
        return match head.rfind(last) {
            Some(open) if !head[open + last.len_utf8()..].is_empty() => {
                (head[open + last.len_utf8()..].to_string(), head)
            }
            _ => (COMPUTED.into(), head),
        };
    }
    if last == ']' {
        return match trimmed.rfind('[') {
            Some(open) => (arg_name(&trimmed[open + 1..]), &trimmed[..open]),
            None => (COMPUTED.into(), trimmed),
        };
    }
    let start = trimmed
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_ident_char(*c))
        .last()
        .map_or(trimmed.len(), |(i, _)| i);
    if start == trimmed.len() {
        (COMPUTED.into(), trimmed)
    } else {
        (trimmed[start..].to_string(), &trimmed[..start])
    }
}

/// The name a read argument denotes, or `<computed>`.
#[must_use]
fn arg_name(arg: &str) -> String {
    const COMPUTED: &str = "<computed>";
    let trimmed = arg.trim_start();
    // A bytes literal (`os.environb[b"NAME"]`) prefixes its quote with `b`/`rb`.
    let unprefixed = trimmed.trim_start_matches(['b', 'B', 'r', 'R']);
    if unprefixed.len() != trimmed.len() {
        if let Some(literal) = string_literal(unprefixed) {
            return literal;
        }
    }
    if let Some(literal) = string_literal(trimmed) {
        return literal;
    }
    let ident: String = trimmed.chars().take_while(|c| is_ident_char(*c)).collect();
    if ident.is_empty() || matches!(ident.as_str(), "f" | "F") {
        return COMPUTED.into();
    }
    ident
}

fn string_literal(s: &str) -> Option<String> {
    let quote = s.chars().next()?;
    if !matches!(quote, '"' | '\'') {
        return None;
    }
    let name: String = s[quote.len_utf8()..]
        .chars()
        .take_while(|c| *c != quote)
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Every environment read in one Python source the allowlist does not sanction,
/// as `path:line: NAME`.
fn python_violations(rel: &str, text: &str, allow: Option<&[&str]>) -> Vec<String> {
    let mut violations = Vec::new();
    let mut walk = PythonWalk::default();
    for (idx, line) in text.lines().enumerate() {
        for name in walk.reads(line) {
            if allow.is_some_and(|names| names.contains(&name.as_str())) {
                continue;
            }
            violations.push(format!("{rel}:{}: {name}", idx + 1));
        }
    }
    violations
}

fn collect_py_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // `__pycache__` holds byte-compiled copies of the sources already
            // scanned; hidden directories are tooling state.
            let name = entry.file_name();
            let name = name.to_str().unwrap_or("");
            if name == "__pycache__" || name.starts_with('.') {
                continue;
            }
            collect_py_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("py") {
            out.push(path);
        }
    }
}

fn python_violations_under(repo: &Path, files: &[PathBuf]) -> Vec<String> {
    let mut violations = Vec::new();
    for path in files {
        let rel = path
            .strip_prefix(repo)
            .map_or_else(|_| path.display().to_string(), |p| p.display().to_string());
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        violations.extend(python_violations(
            &rel,
            &text,
            allowed().get(rel.as_str()).copied(),
        ));
    }
    violations
}

fn repo_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        // workspace_root() is `<repo>/rust`; the Python companion is a sibling
        // tree of the workspace, not a crate inside it.
        workspace_root().parent().map_or_else(
            || PathBuf::from("."),
            |repo| {
                assert!(
                    repo.join("src").join("degenbot").is_dir(),
                    "the repository root is the parent of the rust workspace: {}",
                    repo.display()
                );
                repo.to_path_buf()
            },
        )
    })
}

#[test]
fn no_stray_python_env_reads() {
    let repo = repo_root();
    let driver = repo.join("src").join("degenbot");
    assert!(
        driver.is_dir(),
        "the gate must reach the Python companion at {}",
        driver.display()
    );
    let mut files = Vec::new();
    collect_py_files(&driver, &mut files);
    assert!(
        files.len() > 1,
        "the companion walk must reach src/degenbot sources, found {} files",
        files.len()
    );
    let violations = python_violations_under(repo, &files);
    assert!(
        violations.is_empty(),
        "stray Python env reads in the companion (route them through degenbot-config \
         or add an enumerated, justified entry in this test):\n{}",
        violations.join("\n")
    );
}

/// The read shapes the companion half recognises, and the prose it must not
/// mistake for one, in one line or across a line break. Every form a reader can
/// write is here, so a token spelling that stops matching is a failing table
/// rather than a quiet gap.
#[test]
fn python_env_read_shapes_are_classified() {
    type Case = (&'static [&'static str], &'static [&'static str]);
    const CASES: &[Case] = &[
        (
            &["raw = os.environ.get(\"DEGENBOT_GATE_CANARY\")\n"],
            &["DEGENBOT_GATE_CANARY"],
        ),
        (
            &["raw = os.getenv(\"DEGENBOT_GATE_CANARY\")\n"],
            &["DEGENBOT_GATE_CANARY"],
        ),
        (
            &["raw = os.environ[\"DEGENBOT_GATE_CANARY\"]\n"],
            &["DEGENBOT_GATE_CANARY"],
        ),
        (
            &["if \"DEGENBOT_GATE_CANARY\" in os.environ:\n"],
            &["DEGENBOT_GATE_CANARY"],
        ),
        (
            &["raw = os.environb[b\"DEGENBOT_GATE_CANARY\"]\n"],
            &["DEGENBOT_GATE_CANARY"],
        ),
        (
            &["if \"DEGENBOT_GATE_CANARY\" in os.environb:\n"],
            &["DEGENBOT_GATE_CANARY"],
        ),
        // A `for` over an `in os.environ.get(...)` is a loop, not a test, so the
        // name is the one the call reads.
        (
            &["for b in os.environ.get(\"DEGENBOT_GATE_CANARY\", \"\").split(\",\"):\n"],
            &["DEGENBOT_GATE_CANARY"],
        ),
        // Computed reads report the identifier they compute from, as the Rust
        // half reports the loader's `env::var(name)` seam.
        (&["raw = os.environ.get(name)\n"], &["name"]),
        // A retired-knob sweep is a closed list walked for PRESENCE, so the
        // name is the loop variable the membership test names, not a literal.
        (
            &[
                "for name in _RETIRED:\n",
                "    if name in os.environ:\n",
                "        raise ValueError\n",
            ],
            &["name"],
        ),
        // No name is reducible from these, so the whole mapping is reported.
        (&["env = os.environ\n"], &["<computed>"]),
        (
            &["raw = os.environ.get(f\"DEGENBOT_GATE_{SUFFIX}\")\n"],
            &["<computed>"],
        ),
        (&["msg = f\"{os.environ}\"\n"], &["<computed>"]),
        // Prose about a read is not a read.
        (&["# os.environ.get(\"DEGENBOT_GATE_CANARY\")\n"], &[]),
        (
            &["raw = value  # not os.environ.get(\"DEGENBOT_GATE_CANARY\")\n"],
            &[],
        ),
        (
            &["\"\"\"The example's raw os.environ.get is retired.\"\"\"\n"],
            &[],
        ),
        // A literal is a literal on every line it spans, and a hole in an
        // f-string is code on every line it spans.
        (
            &[
                "msg = f\"\"\"",
                "    {os.environ.get(\"DEGENBOT_GATE_CANARY\")}",
                "\"\"\"",
                "",
            ],
            &["DEGENBOT_GATE_CANARY"],
        ),
        (
            &[
                "note = \"\"\"",
                "    it isn't a read, and it isn't os.environ either",
                "\"\"\"",
                "raw = os.environ.get(\"DEGENBOT_GATE_CANARY\")",
            ],
            &["DEGENBOT_GATE_CANARY"],
        ),
        // A triple-quote that never closes leaves the walk stuck in string
        // mode, so the read after it is silenced and the gate fails OPEN.
        // Pinned here so a future scanner replacement is made knowingly.
        (
            &[
                "note = \"\"\"",
                "    the literal never closes",
                "raw = os.environ.get(\"DEGENBOT_GATE_CANARY\")",
            ],
            &[],
        ),
        // A comment ends at the newline; the hole it sits in does not.
        (
            &[
                "msg = f\"\"\"",
                "    {value  # and carries on",
                "    }\"\"\"",
                "raw = os.environ.get(\"DEGENBOT_GATE_CANARY\")",
            ],
            &["DEGENBOT_GATE_CANARY"],
        ),
        // `{{` and `}}` are the spellings of a literal brace, not a hole.
        (&["msg = f\"{{ {SUFFIX} }}\"", ""], &[]),
        (&["raw = os.environx.get(\"DEGENBOT_GATE_CANARY\")\n"], &[]),
    ];
    for (lines, expected) in CASES {
        let found = python_env_reads(lines);
        let want: Vec<String> = expected.iter().map(ToString::to_string).collect();
        assert_eq!(&found, &want, "lines: {lines:?}");
    }
}

/// The `path:line: NAME` the gate must report for each genuine read of `name`
/// in `text`, located without the scanner under test: a read is a line that
/// calls the mapping AND names the variable, so the prose these fixtures carry
/// (which names a different variable) is excluded by construction.
fn genuine_read_violations(rel: &str, text: &str, name: &str) -> Vec<String> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| line.contains("os.environ.get(") && line.contains(name))
        .map(|(idx, _)| format!("{rel}:{}: {name}", idx + 1))
        .collect()
}

/// Prose about a read is prose whichever line it falls on: this docstring
/// mentions the read on two interior lines, and one of those lines carries the
/// contraction prose actually has.
const MULTILINE_PROSE: &str = r#"
def _probe():
    """A probe docstring.

    It mentions os.environ across a line break, and the worker's budget
    isn't read from os.environ either.
    """
"#;

/// A multi-line string is a string on every line it spans, so the read on the
/// line that closes the quotes is a read and the mention inside the string
/// beside it is prose — and a contraction in that prose is an apostrophe the
/// walk must read as string content, not as a quote that opens one. The second
/// read sits after a whole docstring, where the walk must be back in code.
const MULTILINE_STRING_THEN_READ: &str = r#"
import os

_MESSAGE = """
The owner isn't set, so the message spans a line break: """ + os.environ.get("DEGENBOT_GATE_READ")

_OWNER = """
A docstring.

It mentions os.environ across a line break, and the worker's budget
isn't read from os.environ either.
"""


def _probe():
    value = os.environ.get("DEGENBOT_GATE_READ")
    return value
"#;

#[test]
fn a_multiline_docstring_mentioning_env_is_not_a_read() {
    const REL: &str = "src/degenbot/runner/multiline_prose_probe.py";
    assert!(
        MULTILINE_PROSE.matches("os.environ").count() >= 2,
        "the fixture must actually mention the read, or it proves nothing"
    );
    assert_eq!(
        python_violations(REL, MULTILINE_PROSE, None),
        Vec::<String>::new(),
        "a mention inside a docstring is prose, on every line the docstring spans"
    );
}

#[test]
fn a_read_around_a_multiline_string_is_still_caught() {
    const REL: &str = "src/degenbot/runner/multiline_read_probe.py";
    let expected = genuine_read_violations(REL, MULTILINE_STRING_THEN_READ, "DEGENBOT_GATE_READ");
    assert_eq!(
        expected.len(),
        2,
        "the fixture must hold the read on the line that closes the string and \
         the read after the docstring, or it proves nothing"
    );
    assert_eq!(
        python_violations(REL, MULTILINE_STRING_THEN_READ, None),
        expected,
        "both genuine reads are reads, and neither docstring line is"
    );
}

/// The negative control for the companion half: a file the allowlist does not
/// name must fail the same walk the gate runs. A scanner that quietly stopped
/// matching would leave this test green, so the walk, the parse, and the
/// report are all driven from a real source tree.
#[test]
#[expect(
    clippy::expect_used,
    reason = "test helper: the canary tree is this test's own fixture and a write failure invalidates the control"
)]
fn python_env_gate_fails_on_an_unlisted_read() {
    const CANARY_REL: &str = "src/degenbot/runner/gate_canary.py";
    const CANARY_SOURCE: &str =
        "import os\n\nDEGENBOT_GATE_CANARY = os.environ.get(\"DEGENBOT_GATE_CANARY\")\n";

    let repo = std::env::temp_dir().join(format!("degenbot-env-gate-{}", std::process::id()));
    let canary = repo.join(CANARY_REL);
    std::fs::create_dir_all(canary.parent().expect("canary has a parent directory"))
        .expect("canary fixture tree is creatable");
    std::fs::write(&canary, CANARY_SOURCE).expect("canary fixture is writable");

    let mut files = Vec::new();
    collect_py_files(&repo.join("src").join("degenbot"), &mut files);
    let violations = python_violations_under(&repo, &files);
    let _ = std::fs::remove_dir_all(&repo);

    assert_eq!(
        violations,
        vec![format!("{CANARY_REL}:3: DEGENBOT_GATE_CANARY")],
        "an unlisted read in the companion must fail the walk with its path and name"
    );
    // The complement, so the control is not satisfied by a scanner that reports
    // everything: the same line is silent once the name is enumerated.
    assert!(
        python_violations(CANARY_REL, CANARY_SOURCE, Some(&["DEGENBOT_GATE_CANARY"])).is_empty()
    );
    assert_eq!(
        python_violations(CANARY_REL, CANARY_SOURCE, None),
        vec![format!("{CANARY_REL}:3: DEGENBOT_GATE_CANARY")]
    );
}
