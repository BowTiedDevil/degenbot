//! ADR-051 D8 driver-domain resolver acceptance: per-layer precedence over a
//! LOADED config, `Source` provenance, `~` expansion, and the unresolved-value
//! error text. The four-layer node matrix (capability scopes, transport
//! preference, layers-outrank-transport) lives in `resolver_layers.rs`.
//!
//! Every case builds a `MapEnv`, so no test mutates the process environment.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr as _;

use degenbot_config::{
    expand_state_path_with, node_http_env_name, node_ipc_env_name, node_ws_env_name,
    resolve_chain_id, resolve_database_path, resolve_database_path_with, resolve_node_uri,
    BotConfig, BotConfigLoader, ConfigError, LoadedConfig, MapEnv, NodeOverrides, NodeScope,
    Source, DB_PATH_ENV, DEFAULT_CHAIN_ID_ENV, XDG_STATE_HOME_ENV,
};

const HOME: &str = "/home/tester";

fn map_env(pairs: &[(&str, &str)]) -> MapEnv {
    MapEnv::new(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect::<BTreeMap<_, _>>(),
    )
}

/// A loaded config over `env`, with `file` (when given) as the operator-file
/// layer. The temp file is removed before the load result is returned.
fn load(file: Option<&str>, env: &[(&str, &str)]) -> Result<LoadedConfig, ConfigError> {
    let mut loader = BotConfigLoader::new().with_env(Box::new(map_env(env)));
    let path = file.map(|body| {
        // The body is TOML text, not a filename: a counter names the fixture.
        let id = TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "degenbot-config-resolvers-{}-{id}.toml",
            std::process::id()
        ));
        if let Err(e) = std::fs::write(&path, body) {
            unreachable!("temp toml write failed: {e}");
        }
        path
    });
    if let Some(path) = &path {
        loader = loader.with_config_path(path);
    }
    let result = loader.load();
    if let Some(path) = path {
        // Best-effort cleanup in a sandboxed test tree.
        if std::fs::remove_file(&path).is_err() {}
    }
    result
}

/// The loaded config every resolver reads, over `file` and `env`.
fn loaded(file: Option<&str>, env: &[(&str, &str)]) -> LoadedConfig {
    must_ok(load(file, env))
}

/// A temp-fixture counter: the operator file bodies are TOML text, not names.
static TEMP_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn must_ok<T>(result: Result<T, ConfigError>) -> T {
    match result {
        Ok(value) => value,
        Err(e) => unreachable!("resolution constructed to succeed: {e}"),
    }
}

fn must_err(result: Result<impl std::fmt::Debug, ConfigError>) -> ConfigError {
    match result {
        Ok(value) => unreachable!("resolution constructed to fail, got {value:?}"),
        Err(e) => e,
    }
}

/// A load that MUST fail; panics when it succeeds.
fn must_fail_to_load(file: Option<&str>, env: &[(&str, &str)]) -> ConfigError {
    must_err(load(file, env))
}

#[test]
fn database_path_precedence_runs_explicit_env_file_then_default() {
    let file = loaded(Some("[database]\npath = \"/file/degenbot.db\"\n"), &[]);
    let from_file = resolve_database_path(&file, None);
    assert_eq!(from_file.value, PathBuf::from("/file/degenbot.db"));
    assert_eq!(from_file.source, Source::File);

    let with_env = loaded(
        Some("[database]\npath = \"/file/degenbot.db\"\n"),
        &[(DB_PATH_ENV, "/env/degenbot.db")],
    );
    let from_env = resolve_database_path(&with_env, None);
    assert_eq!(from_env.value, PathBuf::from("/env/degenbot.db"));
    assert_eq!(from_env.source, Source::Env);

    let from_cli = resolve_database_path(&with_env, Some("/cli/degenbot.db"));
    assert_eq!(from_cli.value, PathBuf::from("/cli/degenbot.db"));
    assert_eq!(from_cli.source, Source::Cli);

    // No layer named a path: the schema default under the state home.
    let defaults = loaded(None, &[]);
    let dflt = resolve_database_path_with(&defaults, None, &map_env(&[("HOME", HOME)]));
    assert_eq!(
        dflt.value,
        PathBuf::from(format!("{HOME}/.local/state/degenbot/db/degenbot.db"))
    );
    assert_eq!(dflt.source, Source::Default, "absent layers -> default");
}

#[test]
fn database_path_expands_leading_tilde_against_home() {
    let cfg = loaded(None, &[(DB_PATH_ENV, "~/data/custom.db")]);
    let home_env = map_env(&[("HOME", HOME)]);
    let expanded = resolve_database_path_with(&cfg, None, &home_env);
    assert_eq!(
        expanded.value,
        PathBuf::from(format!("{HOME}/data/custom.db"))
    );

    let bare_cfg = loaded(None, &[(DB_PATH_ENV, "~")]);
    let bare = resolve_database_path_with(&bare_cfg, None, &home_env);
    assert_eq!(bare.value, PathBuf::from(HOME));

    // No HOME layer: the text is left literal (never resolved against cwd).
    let no_home = resolve_database_path_with(&cfg, None, &MapEnv::default());
    assert_eq!(no_home.value, PathBuf::from("~/data/custom.db"));
}

/// A `database.path` an operator wrote in the file layer resolves the same way
/// the environment's does: a leading `~` expands against `$HOME`, and the
/// absent-key default roots under `$XDG_STATE_HOME`. Neither consults the
/// process cwd, so the file a boot opens does not move with the launch
/// directory.
#[test]
fn operator_file_database_path_roots_at_the_state_home_not_the_cwd() {
    let cfg = loaded(
        Some("[database]\npath = \"~/state/degenbot/db/degenbot.db\"\n"),
        &[("HOME", HOME)],
    );
    let from_file = resolve_database_path_with(&cfg, None, &map_env(&[("HOME", HOME)]));
    assert_eq!(
        from_file.value,
        PathBuf::from(format!("{HOME}/state/degenbot/db/degenbot.db"))
    );
    assert_eq!(from_file.source, Source::File);

    let defaults = loaded(None, &[("HOME", HOME), (XDG_STATE_HOME_ENV, "/xdg/state")]);
    let dflt = resolve_database_path_with(
        &defaults,
        None,
        &map_env(&[("HOME", HOME), (XDG_STATE_HOME_ENV, "/xdg/state")]),
    );
    assert_eq!(
        dflt.value,
        PathBuf::from("/xdg/state/degenbot/db/degenbot.db")
    );
    assert_eq!(dflt.source, Source::Default);
}

#[test]
fn empty_database_layers_are_indistinguishable_from_absent() {
    let cfg = loaded(None, &[(DB_PATH_ENV, "")]);
    let resolved = resolve_database_path_with(&cfg, Some(""), &map_env(&[("HOME", HOME)]));
    assert_eq!(resolved.source, Source::Default);
    assert_eq!(
        resolved.value,
        PathBuf::from(format!("{HOME}/.local/state/degenbot/db/degenbot.db"))
    );
}

#[test]
fn xdg_state_home_absolute_drives_the_database_default() {
    let cfg = loaded(None, &[]);
    let env = map_env(&[("HOME", HOME), (XDG_STATE_HOME_ENV, "/xdg/state")]);
    let resolved = resolve_database_path_with(&cfg, None, &env);
    assert_eq!(
        resolved.value,
        PathBuf::from("/xdg/state/degenbot/db/degenbot.db"),
        "absolute XDG_STATE_HOME rebases the built-in database default"
    );
    assert_eq!(resolved.source, Source::Default);

    // Explicit layers are NOT rebased: only the schema default consults XDG.
    let explicit = resolve_database_path_with(&cfg, Some("/cli/custom.db"), &env);
    assert_eq!(explicit.value, PathBuf::from("/cli/custom.db"));
}

#[test]
fn empty_or_relative_xdg_state_home_is_ignored() {
    let cfg = loaded(None, &[]);
    for xdg in ["", "relative/state"] {
        let env = map_env(&[("HOME", HOME), (XDG_STATE_HOME_ENV, xdg)]);
        let resolved = resolve_database_path_with(&cfg, None, &env);
        assert_eq!(
            resolved.value,
            PathBuf::from(format!("{HOME}/.local/state/degenbot/db/degenbot.db")),
            "XDG_STATE_HOME={xdg:?} is not absolute and must be ignored"
        );
    }
}

/// The state-rooted schema keys resolve under the sandboxed state home:
/// `$XDG_STATE_HOME` when absolute, else `$HOME/.local/state`.
#[test]
fn state_rooted_schema_defaults_resolve_under_the_state_home() {
    let cfg = BotConfig::default();
    assert_eq!(
        cfg.logging.runs_dir,
        PathBuf::from("~/.local/state/degenbot/logs")
    );
    assert_eq!(
        cfg.persistence.state_dir,
        PathBuf::from("~/.local/state/degenbot/state")
    );

    let home_env = map_env(&[("HOME", HOME)]);
    assert_eq!(
        expand_state_path_with(&home_env, &cfg.logging.runs_dir.to_string_lossy()),
        PathBuf::from(format!("{HOME}/.local/state/degenbot/logs"))
    );
    assert_eq!(
        expand_state_path_with(&home_env, &cfg.persistence.state_dir.to_string_lossy()),
        PathBuf::from(format!("{HOME}/.local/state/degenbot/state"))
    );

    let xdg_env = map_env(&[("HOME", HOME), (XDG_STATE_HOME_ENV, "/xdg/state")]);
    assert_eq!(
        expand_state_path_with(&xdg_env, &cfg.logging.runs_dir.to_string_lossy()),
        PathBuf::from("/xdg/state/degenbot/logs")
    );
    assert_eq!(
        expand_state_path_with(&xdg_env, &cfg.persistence.state_dir.to_string_lossy()),
        PathBuf::from("/xdg/state/degenbot/state")
    );
}

#[test]
fn chain_id_precedence_runs_explicit_env_then_file() {
    let from_file = must_ok(resolve_chain_id(
        &loaded(Some("[session]\nchain_id = 8453\n"), &[]),
        None,
    ));
    assert_eq!(from_file.value, 8453);
    assert_eq!(from_file.source, Source::File);

    let cfg = loaded(
        Some("[session]\nchain_id = 8453\n"),
        &[(DEFAULT_CHAIN_ID_ENV, "10")],
    );
    let from_env = must_ok(resolve_chain_id(&cfg, None));
    assert_eq!(from_env.value, 10, "env beats the file");
    assert_eq!(from_env.source, Source::Env);

    let from_cli = must_ok(resolve_chain_id(&cfg, Some("1")));
    assert_eq!(from_cli.value, 1, "cli beats env");
    assert_eq!(from_cli.source, Source::Cli);
}

#[test]
fn chain_id_unresolved_and_invalid_name_their_layers() {
    let err = must_err(resolve_chain_id(&loaded(None, &[]), None));
    let text = err.problems.join("\n");
    assert!(
        text.contains("--chain-id"),
        "names the explicit layer: {text}"
    );
    assert!(
        text.contains(DEFAULT_CHAIN_ID_ENV),
        "names the env layer: {text}"
    );
    assert!(
        text.contains("session.chain_id"),
        "names the file layer: {text}"
    );
    assert!(text.contains("unset"), "marks every layer unset: {text}");

    // A non-integer env value is refused by the LOAD, naming the declared key
    // and the value, rather than reaching the resolver as a string.
    let bad_env = must_fail_to_load(None, &[(DEFAULT_CHAIN_ID_ENV, "base")]);
    let text = bad_env.problems.join("\n");
    assert!(text.contains("session.chain_id"), "names the key: {text}");
    assert!(text.contains("base"), "names the value: {text}");

    let bad_cli = must_err(resolve_chain_id(&loaded(None, &[]), Some("nope")));
    assert!(
        bad_cli.problems[0].contains("--chain-id"),
        "{}",
        bad_cli.problems[0]
    );
}

#[test]
fn per_chain_env_names_embed_the_chain_id() {
    assert_eq!(node_http_env_name(8453), "DEGENBOT_RPC_HTTP_CHAINID_8453");
    assert_eq!(node_ws_env_name(8453), "DEGENBOT_RPC_WS_CHAINID_8453");
    assert_eq!(node_ipc_env_name(8453), "DEGENBOT_RPC_IPC_CHAINID_8453");
}

/// An empty layer value is "this layer supplied nothing": a blank export and a
/// blank explicit value both leave the file entry standing, and a
/// blank-LOOKING export is refused by the load rather than becoming a
/// whitespace endpoint.
#[test]
fn an_empty_node_layer_is_absent_not_a_value() {
    let empty_export = loaded(
        Some("[nodes]\nhttp = { 1 = \"https://file.example/rpc\" }\n"),
        &[("DEGENBOT_RPC_HTTP_CHAINID_1", "")],
    );
    let resolved = must_ok(resolve_node_uri(
        &empty_export,
        1,
        NodeScope::Request,
        &NodeOverrides::new().with_http(""),
    ));
    assert_eq!(
        (resolved.value.as_str(), resolved.source),
        ("https://file.example/rpc", Source::File),
        "blank layers never win the cascade slot"
    );

    let blank = must_fail_to_load(None, &[("DEGENBOT_RPC_WS_CHAINID_1", " ")]);
    let text = blank.problems.join("\n");
    assert!(
        text.contains("nodes.ws") && text.contains("chain 1"),
        "a blank-looking export is refused, naming the table and the chain: {text}"
    );
}

#[test]
fn expand_state_path_sibling_of_state_home_is_not_rebased() {
    let xdg_env = map_env(&[("HOME", "/home/tester"), ("XDG_STATE_HOME", "/xdg/state")]);

    // Component-aware prefix: "~/.local/stateful" is not the state home.
    assert_eq!(
        expand_state_path_with(&xdg_env, "~/.local/stateful"),
        PathBuf::from("/home/tester/.local/stateful")
    );
    // The state home itself rebases to the XDG root.
    assert_eq!(
        expand_state_path_with(&xdg_env, "~/.local/state"),
        PathBuf::from("/xdg/state")
    );
    // Unrelated "~" paths expand plain.
    assert_eq!(
        expand_state_path_with(&xdg_env, "~/.cache/foo"),
        PathBuf::from("/home/tester/.cache/foo")
    );
}

/// A surface that carries the capability as text (a console flag, a foreign-
/// language binding argument) must accept exactly the names the refusal
/// prints, or one scope ends up with two vocabularies.

#[test]
fn scope_parses_from_its_own_refusal_spelling() {
    for scope in [NodeScope::Request, NodeScope::Subscription] {
        assert_eq!(
            NodeScope::from_str(scope.as_str()),
            Ok(scope),
            "every scope must round-trip through the name its refusal prints"
        );
    }
}

/// The refusal names both capabilities: a caller that guessed wrong learns the
/// closed set from the error rather than from the source.

#[test]
fn unknown_scope_refusal_names_every_capability() {
    let message = match NodeScope::from_str("polling") {
        Ok(scope) => unreachable!("{scope:?} is not a node scope"),
        Err(e) => e.to_string(),
    };
    assert!(
        message.contains(NodeScope::Request.as_str())
            && message.contains(NodeScope::Subscription.as_str()),
        "the refusal must name both capabilities, got: {message}"
    );
}
