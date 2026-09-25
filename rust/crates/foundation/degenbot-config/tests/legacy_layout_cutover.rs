//! Legacy operator config.toml layout cutover (hard cutover, no shims).
//! Acceptance criteria:
//!
//! 1. The layout items that stay retired (`[otel]`, `default_chain_id`) FAIL
//!    the load with POINTED errors that name `docs/config-migration.md` and
//!    the key that replaced them.
//! 2. `[rpc]`, `[ws]`, and `[database]` are NO LONGER retired-layout items:
//!    `[database]` is a declared section ([`database.path`]), and `[rpc]`/
//!    `[ws]` are no declared spelling, so they keep the generic unknown-section
//!    shape and must not be pointed at the migration doc. ADR-062 D2
//!    deliberately rejected restoring the pre-0.6 `[rpc]`/`[ws]` spelling, so
//!    no shim translates them: the endpoint tables live at `[nodes]`.
//! 3. `[failure_policy]` is NOT retired: ADR-040 reads it as a free-form
//!    per-bucket table through `BotConfigLoader::file_path()` — the loader
//!    must SKIP it (not type it, not reject it) so the wired file layer and
//!    the raw-table reader can share one file.
//! 4. Genuinely unknown sections keep the generic "unknown section" error
//!    and must NOT be mislabeled as retired-layout problems.

use std::collections::BTreeMap;
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

/// (1) The layout items that stay retired: one pointed problem each naming the
/// migration doc — never a bare "unknown section".
#[test]
fn still_retired_layout_items_fail_with_pointed_errors() {
    let path = temp_toml(
        "still-retired",
        concat!(
            "default_chain_id = 1\n",
            "\n[otel]\nendpoint = \"http://localhost:4318\"\nenabled = true\n",
        ),
    );
    let problems = must_err_problems(&BotConfigLoader::new().without_env().with_config_path(&path));
    cleanup(&path);

    let retired = ["otel", "default_chain_id"];
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

/// (1b) The pointed messages name the keys that replaced them:
/// `default_chain_id` -> `session.chain_id`, `[otel]` -> the `telemetry`
/// section.
#[test]
fn pointed_errors_name_replacements() {
    let path = temp_toml(
        "replacements",
        concat!(
            "default_chain_id = 1\n",
            "\n[otel]\nendpoint = \"http://localhost:4318\"\n",
        ),
    );
    let problems = must_err_problems(&BotConfigLoader::new().without_env().with_config_path(&path));
    cleanup(&path);

    let chain = must_find(&problems, "default_chain_id", "chain id");
    assert!(
        chain.contains("session.chain_id"),
        "the chain-id problem must name its replacement key: {chain}"
    );
    let otel = must_find(&problems, "[otel]", "otel");
    assert!(
        otel.contains("telemetry"),
        "otel problem must name the modern telemetry section: {otel}"
    );
}

/// (2) `[rpc]`/`[ws]` are no longer a retired-layout item: they are not a
/// declared spelling either (the endpoint tables live at `[nodes]`), so they
/// keep the generic shape and are never pointed at the migration doc.
#[test]
fn rpc_and_ws_are_no_longer_retired_layout_items() {
    let path = temp_toml(
        "rpc-ws",
        concat!(
            "[rpc]\n1 = \"http://localhost:8545\"\n",
            "\n[ws]\n1 = \"ws://localhost:8546\"\n",
        ),
    );
    let problems = must_err_problems(&BotConfigLoader::new().without_env().with_config_path(&path));
    cleanup(&path);

    for section in ["[rpc]", "[ws]"] {
        let hit = must_find(&problems, section, &format!("no problem names {section}"));
        assert!(
            !hit.contains("retired config-layout item"),
            "{section} is not a retired-layout refusal any more: {hit}"
        );
        assert!(
            !hit.contains("config-migration"),
            "{section} must not be pointed at the migration doc: {hit}"
        );
        assert!(
            hit.contains("unknown section"),
            "{section} is an undeclared spelling and keeps the generic shape: {hit}"
        );
    }
}

/// (2b) `[database]` is a DECLARED section now: the path key loads, and the
/// pre-0.6 `filepath` spelling inside it is an unknown key (the cutover is
/// per-key, not a shim).
#[test]
fn database_section_carries_the_declared_path_key() {
    let path = temp_toml("database-path", "[database]\npath = \"/tmp/x.db\"\n");
    let loaded = match BotConfigLoader::new()
        .without_env()
        .with_config_path(&path)
        .load()
    {
        Ok(loaded) => loaded,
        Err(e) => unreachable!("the declared [database].path must load, refused with: {e}"),
    };
    assert_eq!(loaded.config.database.path, PathBuf::from("/tmp/x.db"));
    cleanup(&path);

    let old = temp_toml(
        "database-filepath",
        "[database]\nfilepath = \"./degenbot.db\"\n",
    );
    let problems = must_err_problems(&BotConfigLoader::new().without_env().with_config_path(&old));
    cleanup(&old);
    let hit = must_find(&problems, "filepath", "the pre-0.6 key is reported");
    assert!(
        !hit.contains("retired config-layout item"),
        "the pre-0.6 key is an unknown key, not a retired-layout item: {hit}"
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

/// (2) `[deployments]` is the Python deployment-registry overlay read through
/// `file_path()` — the same shared-file contract as `[failure_policy]`
/// (ADR-062 D7). A file that declares `[nodes]` endpoints, the `[deployments]`
/// overlay, and the `[failure_policy]` table must load typed-clean. The skip is
/// a SANCTIONED-LIST entry, not a blanket amnesty: a genuinely unknown section
/// in the same file still gets the generic "unknown section" error, so this
/// test pins the DISTINCTION rather than only the happy path.
#[test]
fn deployments_overlay_section_is_skipped_but_unknown_section_still_refused() {
    let path = temp_toml(
        "deployments",
        concat!(
            "[nodes]\n",
            "http = { 1 = \"https://file.example/rpc\" }\n",
            "\n[deployments]\noverlay = \"/tmp/overlay.json\"\n",
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

    let http = loaded
        .config
        .nodes
        .http
        .as_ref()
        .map_or_else(BTreeMap::new, Clone::clone);
    assert_eq!(
        http.get("1").map(String::as_str),
        Some("https://file.example/rpc"),
        "the typed [nodes] table next to [deployments] still loads"
    );

    // The distinction: a section outside the sanctioned list is still refused
    // with the generic shape, never silently skipped.
    let unknown = temp_toml(
        "deployments-unknown",
        concat!(
            "[nodes]\nhttp = { 1 = \"https://file.example/rpc\" }\n",
            "\n[not_a_sanctioned_section]\nkey = \"v\"\n",
        ),
    );
    let problems = must_err_problems(
        &BotConfigLoader::new()
            .without_env()
            .with_config_path(&unknown),
    );
    cleanup(&unknown);
    let hit = must_find(
        &problems,
        "[not_a_sanctioned_section]",
        "unknown section reported",
    );
    assert!(
        hit.contains("unknown section"),
        "generic shape preserved next to a sanctioned section: {hit}"
    );
    assert!(
        !hit.contains("config-migration"),
        "generic path must not name the migration doc: {hit}"
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
