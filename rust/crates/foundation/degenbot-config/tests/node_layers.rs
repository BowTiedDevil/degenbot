//! ADR-062 D1/D2/D4: one operator file, four layers.
//!
//! `[nodes]`, `session.chain_id`, and `database.path` are declared typed keys,
//! so the file layer and the `DEGENBOT_RPC_{HTTP,WS,IPC}_CHAINID_<id>` env
//! families are the SAME code path, and every entry keeps the layer that
//! supplied it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use degenbot_config::{BotConfigLoader, LoadedConfig, MapEnv, Source};

fn temp_toml(name: &str, body: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "degenbot-config-node-layers-{}-{name}.toml",
        std::process::id()
    ));
    if let Err(e) = std::fs::write(&path, body) {
        unreachable!("temp toml write failed: {e}");
    }
    path
}

fn cleanup(path: &PathBuf) {
    // Temp-file cleanup is best-effort in a sandboxed test tree.
    if std::fs::remove_file(path).is_err() {}
}

fn must_ok(from: &BotConfigLoader) -> LoadedConfig {
    match from.load() {
        Ok(loaded) => loaded,
        Err(e) => unreachable!("load must succeed, refused with: {e}"),
    }
}

fn must_problems(loader: &BotConfigLoader) -> Vec<String> {
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

fn map_env(pairs: &[(&str, &str)]) -> Box<dyn degenbot_config::EnvVars> {
    Box::new(MapEnv::new(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect::<BTreeMap<_, _>>(),
    ))
}

/// The `[nodes]` inline-table form: one entry per chain per transport.
#[test]
fn nodes_tables_load_from_the_file_with_file_provenance() {
    let path = temp_toml(
        "nodes-inline",
        concat!(
            "[nodes]\n",
            "http = { 1 = \"https://a.example/rpc\", 8453 = \"https://base.example/rpc\" }\n",
            "ws = { 1 = \"wss://a.example/rpc\" }\n",
            "ipc = { 1 = \"/tmp/anvil.ipc\" }\n",
        ),
    );
    let loaded = must_ok(&BotConfigLoader::new().without_env().with_config_path(&path));
    cleanup(&path);

    let http = loaded
        .config
        .nodes
        .http
        .as_ref()
        .map_or(BTreeMap::new(), Clone::clone);
    assert_eq!(
        http.get("1").map(String::as_str),
        Some("https://a.example/rpc")
    );
    assert_eq!(
        http.get("8453").map(String::as_str),
        Some("https://base.example/rpc")
    );
    let ws = loaded
        .config
        .nodes
        .ws
        .as_ref()
        .map_or(BTreeMap::new(), Clone::clone);
    assert_eq!(ws.get("1").map(String::as_str), Some("wss://a.example/rpc"));
    let ipc = loaded
        .config
        .nodes
        .ipc
        .as_ref()
        .map_or(BTreeMap::new(), Clone::clone);
    assert_eq!(ipc.get("1").map(String::as_str), Some("/tmp/anvil.ipc"));

    for (key, entry) in [
        ("DEGENBOT_RPC_HTTP_CHAINID_", "1"),
        ("DEGENBOT_RPC_HTTP_CHAINID_", "8453"),
        ("DEGENBOT_RPC_WS_CHAINID_", "1"),
        ("DEGENBOT_RPC_IPC_CHAINID_", "1"),
    ] {
        assert_eq!(
            loaded.entry_source_of(key, entry),
            Some(Source::File),
            "{key}{entry} came from the file"
        );
        assert_eq!(loaded.source_of(key), Some(Source::File));
    }
}

/// The nested-table form (`[nodes.http]` with `1 = "..."`) is the same
/// declared key as the inline form.
#[test]
fn nested_node_tables_load_from_the_file() {
    let path = temp_toml(
        "nodes-nested",
        concat!(
            "[nodes.http]\n1 = \"https://a.example/rpc\"\n",
            "\n[nodes.ws]\n1 = \"wss://a.example/rpc\"\n",
            "\n[nodes.ipc]\n1 = \"/tmp/anvil.ipc\"\n",
        ),
    );
    let loaded = must_ok(&BotConfigLoader::new().without_env().with_config_path(&path));
    cleanup(&path);

    assert_eq!(
        loaded
            .config
            .nodes
            .http
            .as_ref()
            .and_then(|m| m.get("1"))
            .map(String::as_str),
        Some("https://a.example/rpc")
    );
    assert_eq!(
        loaded
            .config
            .nodes
            .ws
            .as_ref()
            .and_then(|m| m.get("1"))
            .map(String::as_str),
        Some("wss://a.example/rpc")
    );
    assert_eq!(
        loaded
            .config
            .nodes
            .ipc
            .as_ref()
            .and_then(|m| m.get("1"))
            .map(String::as_str),
        Some("/tmp/anvil.ipc")
    );
    assert_eq!(
        loaded.entry_source_of("DEGENBOT_RPC_HTTP_CHAINID_", "1"),
        Some(Source::File)
    );
}

/// The flat string form is the same key again: `http = "1=https://…,8453=…"`.
#[test]
fn flat_string_node_tables_load_from_the_file() {
    let path = temp_toml(
        "nodes-flat",
        "[nodes]\nhttp = \"1=https://a.example/rpc,8453=https://base.example/rpc\"\n",
    );
    let loaded = must_ok(&BotConfigLoader::new().without_env().with_config_path(&path));
    cleanup(&path);

    let http = loaded
        .config
        .nodes
        .http
        .as_ref()
        .map_or(BTreeMap::new(), Clone::clone);
    assert_eq!(http.len(), 2);
    assert_eq!(
        http.get("8453").map(String::as_str),
        Some("https://base.example/rpc")
    );
    assert_eq!(
        loaded.entry_source_of("DEGENBOT_RPC_HTTP_CHAINID_", "8453"),
        Some(Source::File)
    );
}

/// The chain id and the database path are ordinary typed keys in the same
/// cascade (D4), with the historical env names.
#[test]
fn session_chain_id_and_database_path_load_from_the_file() {
    let path = temp_toml(
        "session-database",
        "[session]\nchain_id = 1\n\n[database]\npath = \"/tmp/x.db\"\n",
    );
    let loaded = must_ok(&BotConfigLoader::new().without_env().with_config_path(&path));
    cleanup(&path);

    assert_eq!(loaded.config.session.chain_id, Some(1));
    assert_eq!(loaded.config.database.path, PathBuf::from("/tmp/x.db"));
    assert_eq!(
        loaded.source_of("DEGENBOT_DEFAULT_CHAIN_ID"),
        Some(Source::File)
    );
    assert_eq!(loaded.source_of("DEGENBOT_DB_PATH"), Some(Source::File));
}

/// The env family outranks the file for the chain it NAMES and leaves every
/// other chain's entry on the file value with the file's provenance.
#[test]
fn env_family_overrides_only_the_named_chain_entry() {
    let path = temp_toml(
        "family-override",
        "[nodes]\nhttp = { 1 = \"https://file.example/rpc\", 8453 = \"https://base.example/rpc\" }\n",
    );
    let loaded = must_ok(
        &BotConfigLoader::new()
            .with_config_path(&path)
            .with_env(map_env(&[(
                "DEGENBOT_RPC_HTTP_CHAINID_1",
                "https://env.example/rpc",
            )])),
    );
    cleanup(&path);

    let http = loaded
        .config
        .nodes
        .http
        .as_ref()
        .map_or(BTreeMap::new(), Clone::clone);
    assert_eq!(
        http.get("1").map(String::as_str),
        Some("https://env.example/rpc")
    );
    assert_eq!(
        http.get("8453").map(String::as_str),
        Some("https://base.example/rpc")
    );
    assert_eq!(
        loaded.entry_source_of("DEGENBOT_RPC_HTTP_CHAINID_", "1"),
        Some(Source::Env),
        "the overridden entry records the env layer"
    );
    assert_eq!(
        loaded.entry_source_of("DEGENBOT_RPC_HTTP_CHAINID_", "8453"),
        Some(Source::File),
        "an entry the family did not name keeps the file layer"
    );
    // The aggregate is the highest-ranked CONTRIBUTING layer, so an operator
    // reading `source_of` learns that the environment shaped this key.
    assert_eq!(
        loaded.source_of("DEGENBOT_RPC_HTTP_CHAINID_"),
        Some(Source::Env)
    );
}

/// A blank family value means "this layer supplied nothing" (the loader's
/// `non_empty` convention): it cannot win the cascade slot and then surface
/// as a malformed URL.
#[test]
fn a_blank_family_value_does_not_win_the_entry() {
    let path = temp_toml(
        "blank-family",
        "[nodes]\nhttp = { 1 = \"https://file.example/rpc\" }\n",
    );
    let loaded = must_ok(
        &BotConfigLoader::new()
            .with_config_path(&path)
            .with_env(map_env(&[("DEGENBOT_RPC_HTTP_CHAINID_1", "")])),
    );
    cleanup(&path);

    assert_eq!(
        loaded
            .config
            .nodes
            .http
            .as_ref()
            .and_then(|m| m.get("1"))
            .map(String::as_str),
        Some("https://file.example/rpc"),
        "a blank export leaves the file entry resolved"
    );
    assert_eq!(
        loaded.entry_source_of("DEGENBOT_RPC_HTTP_CHAINID_", "1"),
        Some(Source::File)
    );
}

/// A blank FILE entry is refused: the loader is fail-closed, and the message
/// names both the chain and the key so the operator can see which line to fix.
#[test]
fn a_blank_file_entry_is_refused_naming_the_chain_and_the_key() {
    let path = temp_toml("blank-file", "[nodes]\nhttp = { 1 = \"\" }\n");
    let problems = must_problems(&BotConfigLoader::new().without_env().with_config_path(&path));
    cleanup(&path);

    let hit = must_find(&problems, "nodes.http", "the blank entry is reported");
    assert!(hit.contains('1'), "the message names the chain: {hit}");
    assert!(
        hit.contains("empty"),
        "the message says the entry is empty rather than leaving it implicit: {hit}"
    );
}

/// A family name whose suffix is not a chain id is a typo, not a chain named
/// `eth`: it is refused naming the expected `PREFIX_<chain_id>` form instead of
/// becoming a map key that only fails later, at chain-id parse time.
#[test]
fn a_family_name_whose_suffix_is_not_a_chain_id_is_refused() {
    let loader = BotConfigLoader::new().with_env(map_env(&[(
        "DEGENBOT_RPC_HTTP_CHAINID_eth",
        "https://a.example/rpc",
    )]));
    let problems = must_problems(&loader);

    let hit = must_find(
        &problems,
        "DEGENBOT_RPC_HTTP_CHAINID_eth",
        "the typo'd family name is reported",
    );
    assert!(
        hit.contains("DEGENBOT_RPC_HTTP_CHAINID_<chain_id>"),
        "the message names the expected form: {hit}"
    );
}

/// An explicit override is the highest layer: it replaces the WHOLE table, so
/// every surviving entry is tagged with it.
#[test]
fn an_explicit_override_replaces_the_whole_table() {
    let path = temp_toml(
        "cli-override",
        "[nodes]\nhttp = { 1 = \"https://file.example/rpc\", 8453 = \"https://base.example/rpc\" }\n",
    );
    let loaded = must_ok(
        &BotConfigLoader::new()
            .without_env()
            .with_config_path(&path)
            .with_cli("nodes.http", "1=https://override.example/rpc"),
    );
    cleanup(&path);

    let http = loaded
        .config
        .nodes
        .http
        .as_ref()
        .map_or(BTreeMap::new(), Clone::clone);
    assert_eq!(
        http.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["1"],
        "the override replaced the table rather than merging into it"
    );
    assert_eq!(
        http.get("1").map(String::as_str),
        Some("https://override.example/rpc")
    );
    assert_eq!(
        loaded.entry_source_of("DEGENBOT_RPC_HTTP_CHAINID_", "1"),
        Some(Source::Cli)
    );
    assert_eq!(
        loaded.source_of("DEGENBOT_RPC_HTTP_CHAINID_"),
        Some(Source::Cli)
    );
}

/// A chain key that is not a number, a wrong transport scheme, and a
/// non-positive session chain id each fail the load with the remedy.
#[test]
fn invalid_node_entries_are_refused_with_the_remedy() {
    let cases: [(&str, &str, &str); 5] = [
        (
            "bad-chain-key",
            "[nodes]\nhttp = { eth = \"https://a.example/rpc\" }\n",
            "chain id",
        ),
        (
            "bad-http-scheme",
            "[nodes]\nhttp = { 1 = \"wss://a.example/rpc\" }\n",
            "http:// or https://",
        ),
        (
            "bad-ws-scheme",
            "[nodes]\nws = { 1 = \"https://a.example/rpc\" }\n",
            "ws:// or wss://",
        ),
        (
            "bad-ipc",
            "[nodes]\nipc = { 1 = \"https://a.example/rpc\" }\n",
            "ipc://",
        ),
        (
            "zero-chain-id",
            "[session]\nchain_id = 0\n",
            "session.chain_id",
        ),
    ];
    for (name, body, needle) in cases {
        let path = temp_toml(name, body);
        let problems = must_problems(&BotConfigLoader::new().without_env().with_config_path(&path));
        cleanup(&path);
        let hit = must_find(&problems, needle, &format!("{name} must name {needle}"));
        assert!(
            !hit.contains("unknown key"),
            "a semantic refusal must not masquerade as an unknown key: {hit}"
        );
    }
}
