//! ADR-062 D3: capability-scoped node resolution over all four layers.
//!
//! One operator file, one loader, one resolver: a transport's endpoint is
//! whatever the explicit layer, the `DEGENBOT_RPC_*_CHAINID_<id>` family, and
//! the `[nodes.*]` file tables say it is. The scope a CONSUMER declares picks
//! which transports may win, and the layer a value came from outranks the
//! transport preference — the two rules this file pins.
//!
//! No case here mutates the process environment: every load drives the
//! loader's injectable [`MapEnv`] seam and a temp file.

use std::collections::BTreeMap;
use std::path::PathBuf;

use degenbot_config::{
    resolve_node_request_uri, resolve_node_subscription_uri, resolve_node_uri, BotConfigLoader,
    LoadedConfig, MapEnv, NodeOverrides, NodeScope, Source,
};

const IPC_FILE: &str = "/run/file.ipc";
const WS_FILE: &str = "wss://file.example/rpc";
const HTTP_FILE: &str = "https://file.example/rpc";
const IPC_ENV: &str = "/run/env.ipc";
const WS_ENV: &str = "wss://env.example/rpc";
const HTTP_ENV: &str = "https://env.example/rpc";
const IPC_CLI: &str = "/run/cli.ipc";
const WS_CLI: &str = "wss://cli.example/rpc";
const HTTP_CLI: &str = "https://cli.example/rpc";

fn temp_toml(name: &str, body: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "degenbot-config-resolver-layers-{}-{name}.toml",
        std::process::id()
    ));
    if let Err(e) = std::fs::write(&path, body) {
        unreachable!("temp toml write failed: {e}");
    }
    path
}

/// Load `body` as the file layer with `env` as the environment layer.
fn loaded(name: &str, body: &str, env: &[(&str, &str)]) -> LoadedConfig {
    let path = temp_toml(name, body);
    let overlay = MapEnv::new(
        env.iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect::<BTreeMap<_, _>>(),
    );
    let result = BotConfigLoader::new()
        .with_config_path(&path)
        .with_env(Box::new(overlay))
        .load();
    // Best-effort cleanup in a sandboxed test tree.
    let _ = std::fs::remove_file(&path);
    match result {
        Ok(loaded) => loaded,
        Err(e) => unreachable!("load constructed to succeed, refused with: {e}"),
    }
}

/// A file carrying one entry per transport for chain 1.
fn all_transports_file() -> String {
    format!(
        "[nodes]\nipc = {{ 1 = \"{IPC_FILE}\" }}\nws = {{ 1 = \"{WS_FILE}\" }}\n\
         http = {{ 1 = \"{HTTP_FILE}\" }}\n"
    )
}

fn must_ok(cfg: &LoadedConfig, scope: NodeScope, overrides: &NodeOverrides) -> (String, Source) {
    resolve_ok(cfg, 1, scope, overrides)
}

fn resolve_ok(
    cfg: &LoadedConfig,
    chain: u64,
    scope: NodeScope,
    overrides: &NodeOverrides,
) -> (String, Source) {
    let resolved = match resolve_node_uri(cfg, chain, scope, overrides) {
        Ok(resolved) => resolved,
        Err(e) => unreachable!("resolution constructed to succeed, refused with: {e}"),
    };
    (resolved.value, resolved.source)
}

fn must_fail(cfg: &LoadedConfig, scope: NodeScope, overrides: &NodeOverrides) -> String {
    resolve_fail(cfg, 1, scope, overrides)
}

fn resolve_fail(
    cfg: &LoadedConfig,
    chain: u64,
    scope: NodeScope,
    overrides: &NodeOverrides,
) -> String {
    match resolve_node_uri(cfg, chain, scope, overrides) {
        Ok(resolved) => unreachable!(
            "resolution constructed to fail, got {:?} from the {} layer",
            resolved.value, resolved.source
        ),
        Err(e) => e.problems.join("\n"),
    }
}

/// One row of the per-transport cascade: which declared key carries the file
/// entry, which env name overrides it, and what the explicit value is.
struct TransportCase {
    /// The `[nodes.*]` table and its per-chain file entry.
    file_key: &'static str,
    /// The env family name (without the chain suffix) that overrides the entry.
    env_name: &'static str,
    /// The file / env / explicit endpoint values, in cascade order.
    values: (&'static str, &'static str, &'static str),
    /// The explicit override slot this transport occupies.
    override_for: fn(NodeOverrides) -> NodeOverrides,
    /// The scope a consumer uses to reach this transport: a subscription may
    /// take ipc and ws, so those cases resolve the subscription scope and the
    /// request-only case resolves the request scope.
    scope: NodeScope,
}

const CASES: [TransportCase; 3] = [
    TransportCase {
        file_key: "ipc",
        env_name: "DEGENBOT_RPC_IPC_CHAINID_",
        values: (IPC_FILE, IPC_ENV, IPC_CLI),
        override_for: |o| o.with_ipc(IPC_CLI),
        scope: NodeScope::Subscription,
    },
    TransportCase {
        file_key: "ws",
        env_name: "DEGENBOT_RPC_WS_CHAINID_",
        values: (WS_FILE, WS_ENV, WS_CLI),
        override_for: |o| o.with_ws(WS_CLI),
        scope: NodeScope::Subscription,
    },
    TransportCase {
        file_key: "http",
        env_name: "DEGENBOT_RPC_HTTP_CHAINID_",
        values: (HTTP_FILE, HTTP_ENV, HTTP_CLI),
        override_for: |o| o.with_http(HTTP_CLI),
        scope: NodeScope::Request,
    },
];

fn env_name(case: &TransportCase) -> String {
    format!("{}1", case.env_name)
}

/// Every transport carries its own four-layer cascade: the file entry is the
/// floor, an export overrides it, and an explicit value overrides the export.
#[test]
fn every_transport_resolves_explicit_over_env_over_file() {
    for (index, case) in CASES.iter().enumerate() {
        let body = format!(
            "[nodes]\n{} = {{ 1 = \"{}\" }}\n",
            case.file_key, case.values.0
        );
        let name = format!("cascade-{index}");
        let (file_value, env_value, cli_value) = case.values;
        let scope = case.scope;

        let from_file = loaded(&name, &body, &[]);
        let (value, source) = must_ok(&from_file, scope, &NodeOverrides::new());
        assert_eq!(
            value, file_value,
            "{}: the file layer resolves",
            case.file_key
        );
        assert_eq!(source, Source::File);

        let from_env = loaded(&name, &body, &[(env_name(case).as_str(), env_value)]);
        let (value, source) = must_ok(&from_env, scope, &NodeOverrides::new());
        assert_eq!(value, env_value, "{}: env beats file", case.file_key);
        assert_eq!(source, Source::Env);

        let explicit = (case.override_for)(NodeOverrides::new());
        let (value, source) = must_ok(&from_env, scope, &explicit);
        assert_eq!(value, cli_value, "{}: explicit beats env", case.file_key);
        assert_eq!(source, Source::Cli);
    }
}

/// A request consumer takes every transport, most-preferred first.
#[test]
fn request_scope_prefers_ipc_then_ws_then_http() {
    let all = loaded("request-all", &all_transports_file(), &[]);
    let (value, source) = must_ok(&all, NodeScope::Request, &NodeOverrides::new());
    assert_eq!((value.as_str(), source), (IPC_FILE, Source::File));

    let without_ipc = loaded(
        "request-no-ipc",
        &format!("[nodes]\nws = {{ 1 = \"{WS_FILE}\" }}\nhttp = {{ 1 = \"{HTTP_FILE}\" }}\n"),
        &[],
    );
    let (value, source) = must_ok(&without_ipc, NodeScope::Request, &NodeOverrides::new());
    assert_eq!((value.as_str(), source), (WS_FILE, Source::File));

    let http_only = loaded(
        "request-http",
        &format!("[nodes]\nhttp = {{ 1 = \"{HTTP_FILE}\" }}\n"),
        &[],
    );
    let (value, source) = must_ok(&http_only, NodeScope::Request, &NodeOverrides::new());
    assert_eq!((value.as_str(), source), (HTTP_FILE, Source::File));
}

/// A subscription consumer takes ipc and ws, and NEVER `nodes.http`: an `http`
/// entry cannot carry a feed, so the scope refuses rather than silently
/// degrading the consumer to polling.
#[test]
fn subscription_scope_takes_ipc_or_ws_and_never_http() {
    let ipc_only = loaded(
        "sub-ipc",
        &format!("[nodes]\nipc = {{ 1 = \"{IPC_FILE}\" }}\n"),
        &[],
    );
    let (value, source) = must_ok(&ipc_only, NodeScope::Subscription, &NodeOverrides::new());
    assert_eq!((value.as_str(), source), (IPC_FILE, Source::File));

    let ws_only = loaded(
        "sub-ws",
        &format!("[nodes]\nws = {{ 1 = \"{WS_FILE}\" }}\n"),
        &[],
    );
    let (value, source) = must_ok(&ws_only, NodeScope::Subscription, &NodeOverrides::new());
    assert_eq!((value.as_str(), source), (WS_FILE, Source::File));

    let both = loaded(
        "sub-both",
        &format!("[nodes]\nipc = {{ 1 = \"{IPC_FILE}\" }}\nws = {{ 1 = \"{WS_FILE}\" }}\n"),
        &[],
    );
    let (value, source) = must_ok(&both, NodeScope::Subscription, &NodeOverrides::new());
    assert_eq!(
        (value.as_str(), source),
        (IPC_FILE, Source::File),
        "ipc outranks ws for a subscription"
    );

    // The refused case: an http entry exists, and a subscription still fails.
    let http_only = loaded(
        "sub-http",
        &format!("[nodes]\nhttp = {{ 1 = \"{HTTP_FILE}\" }}\n"),
        &[],
    );
    let text = must_fail(&http_only, NodeScope::Subscription, &NodeOverrides::new());
    assert!(text.contains("subscription"), "names the scope: {text}");
    assert!(
        text.contains("nodes.ipc") && text.contains("nodes.ws"),
        "names the two transports it accepts: {text}"
    );
    assert!(
        !text.contains(HTTP_FILE),
        "an http entry must not be served to a subscription: {text}"
    );
}

/// An exported endpoint beats a MORE PREFERRED file entry: layers are resolved
/// before transport preference is applied. This is the ordering that keeps a
/// container's env overrides working against a bind-mounted host file — the
/// file's `nodes.ipc` socket would otherwise win the scope and defeat the
/// export that names the endpoint the container can actually reach.
#[test]
fn an_exported_endpoint_beats_a_more_preferred_file_entry() {
    let cfg = loaded(
        "layers-outrank-transport",
        &all_transports_file(),
        &[("DEGENBOT_RPC_HTTP_CHAINID_1", HTTP_ENV)],
    );
    let (value, source) = must_ok(&cfg, NodeScope::Request, &NodeOverrides::new());
    assert_eq!(
        (value.as_str(), source),
        (HTTP_ENV, Source::Env),
        "the file's ipc entry is more preferred, and still loses to the export"
    );
    // The scope filter runs FIRST: `nodes.http` is not in a subscription's
    // transport set, so the export cannot be taken and the file's ipc entry
    // stands rather than the consumer silently downgrading to polling.
    let (value, source) = must_ok(&cfg, NodeScope::Subscription, &NodeOverrides::new());
    assert_eq!(
        (value.as_str(), source),
        (IPC_FILE, Source::File),
        "a subscription never takes nodes.http, so the layer rule has nothing to rank"
    );
}

/// With the layers equal, transport preference decides: among exported
/// entries, ipc > ws > http.
#[test]
fn among_exported_endpoints_transport_preference_decides() {
    let body = all_transports_file();
    let all_three = loaded(
        "pref-all",
        &body,
        &[
            ("DEGENBOT_RPC_HTTP_CHAINID_1", HTTP_ENV),
            ("DEGENBOT_RPC_WS_CHAINID_1", WS_ENV),
            ("DEGENBOT_RPC_IPC_CHAINID_1", IPC_ENV),
        ],
    );
    let (value, source) = must_ok(&all_three, NodeScope::Request, &NodeOverrides::new());
    assert_eq!((value.as_str(), source), (IPC_ENV, Source::Env));

    let ws_and_http = loaded(
        "pref-ws-http",
        &body,
        &[
            ("DEGENBOT_RPC_HTTP_CHAINID_1", HTTP_ENV),
            ("DEGENBOT_RPC_WS_CHAINID_1", WS_ENV),
        ],
    );
    let (value, source) = must_ok(&ws_and_http, NodeScope::Request, &NodeOverrides::new());
    assert_eq!((value.as_str(), source), (WS_ENV, Source::Env));
}

/// Transports and chains resolve independently: the chain a request consumer
/// reaches says nothing about what a subscription consumer reaches.
#[test]
fn each_transport_and_chain_resolves_independently() {
    let cfg = loaded(
        "independent",
        concat!(
            "[nodes]\n",
            "http = { 1 = \"https://one.example/rpc\", 8453 = \"https://base.example/rpc\" }\n",
            "ws = { 8453 = \"wss://base.example/rpc\" }\n",
            "ipc = { 8453 = \"/run/base.ipc\" }\n",
        ),
        &[],
    );
    let none = NodeOverrides::new();

    let (value, source) = must_ok(&cfg, NodeScope::Request, &none);
    assert_eq!(
        (value.as_str(), source),
        ("https://one.example/rpc", Source::File),
        "chain 1's only entry is http, which a request consumer takes"
    );
    let text = must_fail(&cfg, NodeScope::Subscription, &none);
    assert!(
        text.contains("subscription") && text.contains("nodes.ws"),
        "chain 1 has only an http entry, which no subscription may take: {text}"
    );

    let base = |scope: NodeScope| {
        let resolved = resolve_node_uri(&cfg, 8453, scope, &none);
        match resolved {
            Ok(resolved) => (resolved.value, resolved.source),
            Err(e) => unreachable!("chain 8453 resolves for {scope:?}: {e}"),
        }
    };
    assert_eq!(
        base(NodeScope::Request),
        ("/run/base.ipc".into(), Source::File)
    );
    assert_eq!(
        base(NodeScope::Subscription),
        ("/run/base.ipc".into(), Source::File),
        "the ipc entry serves both scopes"
    );
}

/// A chain no layer names fails loud, and the refusal names every layer and
/// every transport the scope consulted.
#[test]
fn an_absent_chain_fails_naming_every_layer_and_transport() {
    let cfg = loaded("absent", &all_transports_file(), &[]);
    let text = resolve_fail(&cfg, 8453, NodeScope::Subscription, &NodeOverrides::new());
    assert!(text.contains("no subscription endpoint resolved"), "{text}");
    assert!(text.contains("chain 8453"), "names the chain: {text}");
    assert!(
        text.contains("explicit"),
        "names the explicit layer: {text}"
    );
    assert!(
        text.contains("DEGENBOT_RPC_IPC_CHAINID_8453")
            && text.contains("DEGENBOT_RPC_WS_CHAINID_8453"),
        "names the env families the scope consults: {text}"
    );
    assert!(
        text.contains("nodes.ipc") && text.contains("nodes.ws"),
        "names the file tables the scope consults: {text}"
    );
    assert!(
        text.contains("no localhost default"),
        "states that the fourth layer is this refusal, not a default: {text}"
    );
    assert!(
        text.contains("ipc, ws"),
        "names the transports in preference order: {text}"
    );
    assert!(
        !text.contains("nodes.http"),
        "a subscription never consults the request-only table: {text}"
    );
}

/// The same refusal, one chain deeper: a chain absent from every layer of an
/// EMPTY config names the scope's transports and the remedy.
#[test]
fn a_chain_absent_from_every_layer_names_the_remedy() {
    let cfg = loaded("empty", "[session]\nchain_id = 1\n", &[]);
    let text = resolve_fail(&cfg, 1, NodeScope::Request, &NodeOverrides::new());
    assert!(text.contains("no request endpoint resolved"), "{text}");
    assert!(
        text.contains("ipc, ws, http"),
        "a request consults all three, in preference order: {text}"
    );
    assert!(
        text.contains("explicit node argument"),
        "names the explicit layer: {text}"
    );
}

/// The wrappers are the scope spelled out: a request consumer and a
/// subscription consumer asking the same config get their own transport.
#[test]
fn the_scope_wrappers_select_the_transport() {
    let cfg = loaded("wrappers", &all_transports_file(), &[]);
    let none = NodeOverrides::new();
    let request = resolve_node_request_uri(&cfg, 1, &none);
    let subscription = resolve_node_subscription_uri(&cfg, 1, &none);
    match (request, subscription) {
        (Ok(request), Ok(subscription)) => {
            assert_eq!(request.value, IPC_FILE);
            assert_eq!(subscription.value, IPC_FILE);
        }
        (other, _) => unreachable!("both scopes resolve over an ipc entry: {other:?}"),
    }

    let http_only = loaded(
        "wrappers-http",
        &format!("[nodes]\nhttp = {{ 1 = \"{HTTP_FILE}\" }}\n"),
        &[],
    );
    let request = resolve_node_request_uri(&http_only, 1, &none);
    let subscription = resolve_node_subscription_uri(&http_only, 1, &none);
    assert_eq!(request.map(|r| r.value), Ok(HTTP_FILE.to_string()));
    assert!(
        subscription.is_err(),
        "the subscription wrapper refuses the request-only transport"
    );
}
