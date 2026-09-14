//! ADR-051 D8: driver-domain resolver acceptance — per-layer precedence,
//! `Source` provenance, `~` expansion, and the unresolved-endpoint error text.
//!
//! Every case builds a `MapEnv`, so no test mutates the process environment.

use std::collections::BTreeMap;
use std::path::PathBuf;

use degenbot_config::{
    node_http_env_name, node_ws_env_name, resolve_chain_id, resolve_database_path,
    resolve_node_http_uri, resolve_node_uris, resolve_node_ws_uri, ConfigError, MapEnv, Source,
    DB_PATH_ENV, DEFAULT_CHAIN_ID_ENV,
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

#[test]
fn database_path_precedence_cli_beats_env_beats_default() {
    let dflt = resolve_database_path(&map_env(&[("HOME", HOME)]), None);
    assert_eq!(
        dflt.value,
        PathBuf::from(format!("{HOME}/.config/degenbot/degenbot.db"))
    );
    assert_eq!(dflt.source, Source::Default, "absent layers -> default");

    let env = map_env(&[("HOME", HOME), (DB_PATH_ENV, "/env/degenbot.db")]);
    let from_env = resolve_database_path(&env, None);
    assert_eq!(from_env.value, PathBuf::from("/env/degenbot.db"));
    assert_eq!(from_env.source, Source::Env);

    let from_cli = resolve_database_path(&env, Some("/cli/degenbot.db"));
    assert_eq!(from_cli.value, PathBuf::from("/cli/degenbot.db"));
    assert_eq!(from_cli.source, Source::Cli);
}

#[test]
fn database_path_expands_leading_tilde_against_home() {
    let env = map_env(&[("HOME", HOME), (DB_PATH_ENV, "~/data/custom.db")]);
    let expanded = resolve_database_path(&env, None);
    assert_eq!(
        expanded.value,
        PathBuf::from(format!("{HOME}/data/custom.db"))
    );

    let bare = resolve_database_path(&map_env(&[("HOME", HOME), (DB_PATH_ENV, "~")]), None);
    assert_eq!(bare.value, PathBuf::from(HOME));

    // No HOME layer: the text is left literal (never resolved against cwd).
    let no_home = resolve_database_path(&map_env(&[(DB_PATH_ENV, "~/x.db")]), None);
    assert_eq!(no_home.value, PathBuf::from("~/x.db"));
}

#[test]
fn empty_database_layers_are_indistinguishable_from_absent() {
    let env = map_env(&[("HOME", HOME), (DB_PATH_ENV, "")]);
    let resolved = resolve_database_path(&env, Some(""));
    assert_eq!(resolved.source, Source::Default);
    assert_eq!(
        resolved.value,
        PathBuf::from(format!("{HOME}/.config/degenbot/degenbot.db"))
    );
}

#[test]
fn chain_id_precedence_cli_beats_env() {
    let env = map_env(&[(DEFAULT_CHAIN_ID_ENV, "8453")]);
    let from_env = must_ok(resolve_chain_id(&env, None));
    assert_eq!(from_env.value, 8453);
    assert_eq!(from_env.source, Source::Env);

    let from_cli = must_ok(resolve_chain_id(&env, Some("1")));
    assert_eq!(from_cli.value, 1, "cli beats env");
    assert_eq!(from_cli.source, Source::Cli);
}

#[test]
fn chain_id_unresolved_and_invalid_name_their_layers() {
    let empty = MapEnv::default();
    let err = must_err(resolve_chain_id(&empty, None));
    let text = err.problems.join("\n");
    assert!(text.contains("--chain-id"), "names the CLI layer: {text}");
    assert!(
        text.contains(DEFAULT_CHAIN_ID_ENV),
        "names the env layer: {text}"
    );
    assert!(text.contains("unset"), "marks both layers unset: {text}");

    let bad_env = must_err(resolve_chain_id(
        &map_env(&[(DEFAULT_CHAIN_ID_ENV, "base")]),
        None,
    ));
    assert!(
        bad_env.problems[0].contains(DEFAULT_CHAIN_ID_ENV),
        "{}",
        bad_env.problems[0]
    );
    let bad_cli = must_err(resolve_chain_id(&empty, Some("nope")));
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
}

#[test]
fn node_uri_precedence_cli_beats_env() {
    let chain = 1_u64;
    let http_env = node_http_env_name(chain);
    let ws_env = node_ws_env_name(chain);
    let env = map_env(&[
        (http_env.as_str(), "http://env.example"),
        (ws_env.as_str(), "ws://env.example"),
    ]);

    let from_env = must_ok(resolve_node_uris(&env, chain, None, None));
    assert_eq!(from_env.http.value, "http://env.example");
    assert_eq!(from_env.http.source, Source::Env);
    assert_eq!(from_env.ws.value, "ws://env.example");
    assert_eq!(from_env.ws.source, Source::Env);

    let from_cli = must_ok(resolve_node_uris(
        &env,
        chain,
        Some("http://cli.example"),
        Some("ws://cli.example"),
    ));
    assert_eq!(from_cli.http.value, "http://cli.example");
    assert_eq!(from_cli.http.source, Source::Cli);
    assert_eq!(from_cli.ws.value, "ws://cli.example");
    assert_eq!(from_cli.ws.source, Source::Cli);
}

#[test]
fn unresolved_node_rpc_names_every_layer_consulted() {
    let chain = 1_u64;
    let empty = MapEnv::default();

    let http = must_err(resolve_node_http_uri(&empty, chain, None));
    let text = http.problems.join("\n");
    assert!(text.contains("no HTTP RPC endpoint resolved"), "{text}");
    assert!(text.contains("--node-http"), "names the CLI layer: {text}");
    assert!(
        text.contains(&node_http_env_name(chain)),
        "names the env layer: {text}"
    );
    assert!(text.contains("unset"), "marks both layers unset: {text}");
    assert!(text.contains("no localhost default"), "{text}");

    let ws = must_err(resolve_node_ws_uri(&empty, chain, None));
    let text = ws.problems.join("\n");
    assert!(text.contains("no WS RPC endpoint resolved"), "{text}");
    assert!(text.contains("--node-ws"), "{text}");
    assert!(text.contains(&node_ws_env_name(chain)), "{text}");

    // The pair resolver aggregates BOTH missing endpoints in one error.
    let both = must_err(resolve_node_uris(&empty, chain, None, None));
    assert_eq!(both.problems.len(), 2, "{:?}", both.problems);
    assert!(both.problems.iter().any(|p| p.contains("no HTTP")));
    assert!(both.problems.iter().any(|p| p.contains("no WS")));
}

#[test]
fn empty_node_uri_layer_is_absent_not_a_value() {
    let chain = 1_u64;
    let http_env = node_http_env_name(chain);
    let ws_env = node_ws_env_name(chain);

    // Empty env + empty CLI: unresolved, never an empty-string URI.
    let empty = map_env(&[(http_env.as_str(), "")]);
    let err = must_err(resolve_node_http_uri(&empty, chain, Some("")));
    assert!(
        err.problems[0].contains("no HTTP RPC endpoint resolved"),
        "{}",
        err.problems[0]
    );

    // Whitespace-only is a value (parity with the Python cascade's truthiness).
    let ws = must_ok(resolve_node_ws_uri(
        &map_env(&[(ws_env.as_str(), " ")]),
        chain,
        None,
    ));
    assert_eq!(ws.value, " ");
    assert_eq!(ws.source, Source::Env);
}
