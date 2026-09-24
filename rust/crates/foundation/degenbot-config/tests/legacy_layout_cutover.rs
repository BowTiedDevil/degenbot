//! Legacy operator config.toml layout cutover (Option B — hard cutover,
//! no shims). Acceptance criteria:
//!
//! 1. The five retired-layout sections (`[rpc]`, `[ws]`, `[database]`,
//!    `[otel]`) and the top-level `default_chain_id` key FAIL the load
//!    with POINTED errors that name `docs/config-migration.md` and, where a
//!    replacement exists, the env/modern-key replacement.
//! 2. `[failure_policy]` is NOT retired: ADR-040 reads it as a free-form
//!    per-bucket table through `BotConfigLoader::file_path()` — the loader
//!    must SKIP it (not type it, not reject it) so the wired file layer and
//!    the raw-table reader can share one file.
//! 3. Genuinely unknown sections keep the generic "unknown section" error
//!    and must NOT be mislabeled as retired-layout problems.

use std::path::PathBuf;

use degenbot_config::BotConfigLoader;

fn temp_toml(name: &str, body: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "degenbot-config-legacy-cutover-{}-{name}.toml",
        std::process::id()
    ));
    if let Err(e) = std::fs::write(&path, body) {
        unreachable!("temp toml write failed: {e}");
    }
    path
}

fn cleanup(path: &PathBuf) {
    if std::fs::remove_file(path).is_err() {}
}

fn must_err_problems(loader: &BotConfigLoader) -> Vec<String> {
    match loader.load() {
        Ok(_) => unreachable!("load constructed to fail"),
        Err(err) => err.problems,
    }
}

#[expect(
    clippy::expect_used,
    reason = "test assertion helper: a missing problem entry is a hard test failure; loud panic beats a dummy value"
)]
fn must_find<'a>(problems: &'a [String], needle: &str, why: &str) -> &'a String {
    problems.iter().find(|p| p.contains(needle)).expect(why)
}

/// (1) The original production boot refusal: all six legacy-layout items in
/// one file. Every one of them must produce a pointed problem naming the
/// migration doc — never a bare "unknown section".
#[test]
fn legacy_layout_items_fail_with_pointed_errors() {
    let path = temp_toml(
        "all-legacy",
        concat!(
            "default_chain_id = 1\n",
            "\n[rpc]\n1 = \"http://localhost:8545\"\n",
            "\n[ws]\n1 = \"ws://localhost:8546\"\n",
            "\n[database]\nfilepath = \"./degenbot.db\"\n",
            "\n[otel]\nendpoint = \"http://localhost:4318\"\nenabled = true\n",
        ),
    );
    let problems = must_err_problems(&BotConfigLoader::new().without_env().with_config_path(&path));
    cleanup(&path);

    let retired = ["rpc", "ws", "database", "otel", "default_chain_id"];
    assert_eq!(
        problems.len(),
        retired.len(),
        "exactly one problem per retired item; got: {problems:#?}"
    );
    for name in retired {
        let hit = must_find(&problems, name, &format!("no problem names {name}"));
        assert!(
            hit.contains("docs/config-migration.md"),
            "problem for {name} must point at the migration doc: {hit}"
        );
        assert!(
            !hit.contains("unknown section"),
            "pointed message must not fall back to the generic shape: {hit}"
        );
    }
}

/// (1b) The pointed messages name the concrete replacements: rpc/ws per-chain
/// endpoints -> `DEGENBOT_RPC_HTTP_CHAINID_*/DEGENBOT_RPC_WS_CHAINID_*` env,
/// database path -> the Python config cascade, otel -> the `telemetry`
/// section.
#[test]
fn pointed_errors_name_replacements() {
    let path = temp_toml(
        "replacements",
        concat!(
            "[rpc]\n1 = \"http://localhost:8545\"\n",
            "\n[database]\nfilepath = \"./degenbot.db\"\n",
            "\n[otel]\nendpoint = \"http://localhost:4318\"\n",
        ),
    );
    let problems = must_err_problems(&BotConfigLoader::new().without_env().with_config_path(&path));
    cleanup(&path);

    let rpc = must_find(&problems, "[rpc]", "rpc");
    assert!(
        rpc.contains("DEGENBOT_RPC_HTTP_CHAINID_"),
        "rpc problem must name the per-chain env replacement: {rpc}"
    );
    let db = must_find(&problems, "[database]", "db");
    assert!(
        db.contains("config.py") || db.contains("Python"),
        "database problem must name the Python-side replacement: {db}"
    );
    let otel = must_find(&problems, "[otel]", "otel");
    assert!(
        otel.contains("telemetry"),
        "otel problem must name the modern telemetry section: {otel}"
    );
}

/// (2) `[failure_policy]` is contractually a free-form ADR-040 table read
/// through `file_path()` — the loader must skip it without error so the
/// wired file layer and the raw-table reader can share one file.
#[test]
fn failure_policy_section_is_skipped_not_rejected() {
    let path = temp_toml(
        "failure-policy",
        concat!(
            "[telemetry]\notel = false\n",
            "\n[failure_policy]\nrpc.ratelimit = \"halt\"\n",
        ),
    );
    let loaded = match BotConfigLoader::new()
        .without_env()
        .with_config_path(&path)
        .load()
    {
        Ok(loaded) => loaded,
        Err(e) => unreachable!("must be permitted, boot refused with: {e}"),
    };
    cleanup(&path);
    assert!(
        !loaded.config.telemetry.otel,
        "typed keys in the same file still load"
    );
    assert!(
        !loaded.provenance.is_empty(),
        "provenance still complete for typed keys"
    );
}

/// (3) A genuinely unknown section keeps the generic error shape and must
/// not be redirected at the legacy-layout migration doc.
#[test]
fn genuinely_unknown_section_keeps_generic_error() {
    let path = temp_toml("unknown", "[totally_bogus]\nkey = \"v\"\n");
    let problems = must_err_problems(&BotConfigLoader::new().without_env().with_config_path(&path));
    cleanup(&path);

    let hit = must_find(&problems, "[totally_bogus]", "unknown section reported");
    assert!(
        hit.contains("unknown section"),
        "generic shape preserved: {hit}"
    );
    assert!(
        !hit.contains("config-migration"),
        "generic path must not name the migration doc: {hit}"
    );
}
