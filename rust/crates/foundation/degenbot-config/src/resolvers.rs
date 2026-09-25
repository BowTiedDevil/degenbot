//! Driver-domain resolvers (ADR-051 D8, ADR-062 D1/D3/D4): database path,
//! session chain id, and the node RPC URI.
//!
//! Each value is a DECLARED typed key — `database.path`, `session.chain_id`,
//! and the `nodes.{ipc,ws,http}` endpoint tables — so all four layers are one
//! code path and one operator writes them in one place. A resolver here is a
//! thin reader of a [`LoadedConfig`]: the loader is the only crate that reads
//! the environment, and a resolver never takes an [`EnvVars`] seam for a
//! config layer. (`HOME` and `$XDG_STATE_HOME` are not config keys — they only
//! expand the database default's `~` and rebase the state home, which is why
//! the path helper takes a seam and the resolvers do not.)
//!
//! Every result carries the winning [`Source`] exactly as [`LoadedConfig`]
//! provenance does. The empty string and an absent value are
//! indistinguishable (both mean "this layer supplied nothing"), so an
//! exported-but-blank variable cannot silently become a value.
//!
//! Layers, highest first:
//!
//! | value    | layers                                                                    |
//! |----------|---------------------------------------------------------------------------|
//! | database | `--database` > `DEGENBOT_DB_PATH` > `database.path` > state-home default   |
//! | chain id | `--chain-id` > `DEGENBOT_DEFAULT_CHAIN_ID` > `session.chain_id` > fail loud |
//! | node URI | explicit > `DEGENBOT_RPC_IPC_CHAINID_<id>` / `..._WS_...` / `..._HTTP_...` > the `nodes.*` entry, per scope |
//!
//! A node URI is additionally SCOPED: the consumer declares the capability it
//! needs ([`NodeScope`]) and the scope decides which transports may win, ipc >
//! ws > http for a request and ipc > ws for a subscription (ADR-062 D3). A
//! subscription never polls, so `nodes.http` is not in its transport set.
//!
//! The database default lives under the XDG state home: `$XDG_STATE_HOME`
//! (absolute only) else `$HOME/.local/state`, then `degenbot/db/degenbot.db`.
//! The state-rooted schema keys (`logging.runs_dir`, `persistence.state_dir`)
//! share the same base, expanded by [`expand_state_path`].
//!
//! [`LoadedConfig`]: crate::LoadedConfig

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::error::ConfigError;
use crate::loader::{EnvVars, LoadedConfig, Source};
use crate::schema::NodeTransport;

/// Environment variable supplying the database path (`--database` overrides
/// it; the built-in default applies when both are unset).
pub const DB_PATH_ENV: &str = "DEGENBOT_DB_PATH";

/// The XDG Base Directory variable selecting the config home. Honored by
/// [`standard_file_path`](crate::standard_file_path) and [`config_home`].
pub const XDG_CONFIG_HOME_ENV: &str = "XDG_CONFIG_HOME";

/// The XDG Base Directory variable selecting the state home, the base of the
/// database / run-artifact / durable-state defaults.
pub const XDG_STATE_HOME_ENV: &str = "XDG_STATE_HOME";

/// Built-in default database path under the state home. The leading `~` is
/// expanded against the `HOME` env layer at resolution time, and a usable
/// `$XDG_STATE_HOME` rebases the whole state-home prefix (see
/// [`expand_state_path`]).
pub const DB_PATH_DEFAULT: &str = "~/.local/state/degenbot/db/degenbot.db";

/// The state-home prefix carried by the state-rooted schema defaults
/// (`logging.runs_dir`, `persistence.state_dir`, [`DB_PATH_DEFAULT`]).
const STATE_HOME_PREFIX: &str = "~/.local/state";

/// Environment variable supplying the session chain id (`--chain-id`
/// overrides it). This is the name the Python CLI reads today (`config.py`);
/// no second spelling is invented.
pub const DEFAULT_CHAIN_ID_ENV: &str = "DEGENBOT_DEFAULT_CHAIN_ID";

/// Prefix of the per-chain HTTP RPC env name; the suffix is the numeric
/// chain id (dynamic, intentionally not a schema key).
pub const RPC_HTTP_ENV_PREFIX: &str = "DEGENBOT_RPC_HTTP_CHAINID_";

/// Prefix of the per-chain WS RPC env name; the suffix is the numeric
/// chain id.
pub const RPC_WS_ENV_PREFIX: &str = "DEGENBOT_RPC_WS_CHAINID_";

/// Prefix of the per-chain IPC RPC env name; the suffix is the numeric
/// chain id. The local-socket twin of [`RPC_HTTP_ENV_PREFIX`].
pub const RPC_IPC_ENV_PREFIX: &str = "DEGENBOT_RPC_IPC_CHAINID_";

/// The env variable read for `~` expansion in the database-path default.
const HOME_ENV: &str = "HOME";

/// A resolved value plus the layer that supplied it — the per-value analogue
/// of [`LoadedConfig`](crate::LoadedConfig)'s `provenance` map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved<T> {
    /// The effective value.
    pub value: T,
    /// Which layer supplied it.
    pub source: Source,
}

impl<T> Resolved<T> {
    /// Bundle a value with its winning layer.
    #[must_use]
    pub const fn new(value: T, source: Source) -> Self {
        Self { value, source }
    }
}

/// The `DEGENBOT_RPC_HTTP_CHAINID_<id>` name for `chain_id`.
#[must_use]
pub fn node_http_env_name(chain_id: u64) -> String {
    format!("{RPC_HTTP_ENV_PREFIX}{chain_id}")
}

/// The `DEGENBOT_RPC_WS_CHAINID_<id>` name for `chain_id`.
#[must_use]
pub fn node_ws_env_name(chain_id: u64) -> String {
    format!("{RPC_WS_ENV_PREFIX}{chain_id}")
}

/// The `DEGENBOT_RPC_IPC_CHAINID_<id>` name for `chain_id`.
#[must_use]
pub fn node_ipc_env_name(chain_id: u64) -> String {
    format!("{RPC_IPC_ENV_PREFIX}{chain_id}")
}

/// The capability a consumer needs from a node (ADR-062 D3).
///
/// The scope is the CONSUMER's requirement, never the operator's spelling: a
/// subscription consumer takes what can carry a feed (ipc, ws) and fails
/// loudly rather than silently degrading to polling over `nodes.http`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeScope {
    /// One-shot JSON-RPC: pool reads, `eth_callMany`, transaction submission.
    Request,
    /// A long-lived event feed: the pump, the head/logs streams.
    Subscription,
}

/// The transports a request consumer may resolve, most preferred first.
const REQUEST_TRANSPORTS: &[NodeTransport] =
    &[NodeTransport::Ipc, NodeTransport::Ws, NodeTransport::Http];

/// The transports a subscription consumer may resolve: ipc and ws, and never
/// `nodes.http` — an HTTP endpoint cannot carry a feed.
const SUBSCRIPTION_TRANSPORTS: &[NodeTransport] = &[NodeTransport::Ipc, NodeTransport::Ws];

impl NodeScope {
    /// The transports this scope may resolve, in preference order.
    #[must_use]
    pub const fn transports(self) -> &'static [NodeTransport] {
        match self {
            Self::Request => REQUEST_TRANSPORTS,
            Self::Subscription => SUBSCRIPTION_TRANSPORTS,
        }
    }

    /// The scope's name, as the refusal spells it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Subscription => "subscription",
        }
    }
}

impl std::fmt::Display for NodeScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The explicit layer a caller threads in (ADR-062 D1 rank 1): the endpoints an
/// argument named, one slot per transport. An empty value is "this layer
/// supplied nothing" — the same convention the loader's env-family merge uses,
/// so a bare `--node ""` cannot become an endpoint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeOverrides {
    /// The `nodes.ipc` slot: an `ipc://` URL or a socket path.
    pub ipc: Option<String>,
    /// The `nodes.ws` slot: a `ws://` or `wss://` URL.
    pub ws: Option<String>,
    /// The `nodes.http` slot: an `http://` or `https://` URL.
    pub http: Option<String>,
}

impl NodeOverrides {
    /// No explicit value in any slot.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ipc: None,
            ws: None,
            http: None,
        }
    }

    /// Set the `nodes.ipc` slot.
    #[must_use]
    pub fn with_ipc(mut self, uri: impl Into<String>) -> Self {
        self.ipc = Some(uri.into());
        self
    }

    /// Set the `nodes.ws` slot.
    #[must_use]
    pub fn with_ws(mut self, uri: impl Into<String>) -> Self {
        self.ws = Some(uri.into());
        self
    }

    /// Set the `nodes.http` slot.
    #[must_use]
    pub fn with_http(mut self, uri: impl Into<String>) -> Self {
        self.http = Some(uri.into());
        self
    }

    /// The explicit value for `transport`, if this layer supplied one.
    #[must_use]
    pub fn get(&self, transport: NodeTransport) -> Option<&str> {
        match transport {
            NodeTransport::Ipc => self.ipc.as_deref(),
            NodeTransport::Ws => self.ws.as_deref(),
            NodeTransport::Http => self.http.as_deref(),
        }
    }
}

/// Treat an empty string exactly like an absent layer. Shared with the
/// loader's env-family merge so "supplied nothing" has one spelling.
pub(crate) fn non_empty<T: AsRef<str> + ?Sized>(value: Option<&T>) -> Option<&str> {
    value.map(AsRef::as_ref).filter(|v| !v.is_empty())
}

/// Expand a leading `~` (or `~/`) against `HOME` from the env seam.
///
/// Only the bare-home forms are handled: `~user` would need the passwd
/// database, and with no `HOME` the text is returned unchanged (Python's
/// `Path.expanduser()` falls back to `pwd`, which has no pure-Rust
/// equivalent on the config path).
fn expand_tilde(env: &dyn EnvVars, raw: &str) -> PathBuf {
    let home = env.get(HOME_ENV).filter(|h| !h.is_empty());
    if raw == "~" {
        if let Some(home) = home {
            return PathBuf::from(home);
        }
    } else if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = home {
            return Path::new(&home).join(rest);
        }
    }
    PathBuf::from(raw)
}

/// Expand a leading `~` against `HOME` for a `~`-carrying schema default
/// (the `logging.runs_dir` key uses the same convention as
/// [`DB_PATH_DEFAULT`]). Reads `HOME` through the [`EnvVars`] seam so
/// degenbot-config stays the only env-reading crate; with no `HOME` the
/// text is returned unchanged.
#[must_use]
pub fn expand_tilde_path(raw: &str) -> PathBuf {
    expand_tilde(&crate::ProcessEnv, raw)
}

/// An XDG base-directory variable that is USABLE per the spec: set,
/// non-empty, and absolute. An empty or relative value is deliberately
/// ignored (spec: relative paths are invalid and must be skipped).
fn xdg_base(env: &dyn EnvVars, name: &str) -> Option<PathBuf> {
    let raw = env.get(name).filter(|v| !v.is_empty())?;
    let path = PathBuf::from(raw);
    path.is_absolute().then_some(path)
}

/// The XDG config home: `$XDG_CONFIG_HOME` (absolute only) else
/// `$HOME/.config`. `None` when neither a usable variable nor `HOME` exists.
#[must_use]
pub fn config_home(env: &dyn EnvVars) -> Option<PathBuf> {
    if let Some(xdg) = xdg_base(env, XDG_CONFIG_HOME_ENV) {
        return Some(xdg);
    }
    env.get(HOME_ENV)
        .filter(|home| !home.is_empty())
        .map(|home| Path::new(&home).join(".config"))
}

/// The XDG state home: `$XDG_STATE_HOME` (absolute only) else
/// `$HOME/.local/state`. `None` when neither a usable variable nor `HOME`
/// exists.
#[must_use]
pub fn state_home(env: &dyn EnvVars) -> Option<PathBuf> {
    if let Some(xdg) = xdg_base(env, XDG_STATE_HOME_ENV) {
        return Some(xdg);
    }
    env.get(HOME_ENV)
        .filter(|home| !home.is_empty())
        .map(|home| Path::new(&home).join(".local/state"))
}

/// Expand a state-rooted schema path through the env seam: a usable
/// `$XDG_STATE_HOME` rebases a path under the `~/.local/state` prefix, and
/// every other form falls back to plain leading-`~` expansion against HOME.
/// Centralized here so the database, run-artifact, and durable-state
/// defaults share one XDG rule instead of re-deriving it per key.
#[must_use]
pub fn expand_state_path_with(env: &dyn EnvVars, raw: &str) -> PathBuf {
    if let (Some(xdg), Some(rest)) = (
        xdg_base(env, XDG_STATE_HOME_ENV),
        raw.strip_prefix(STATE_HOME_PREFIX),
    ) {
        // Component-aware match: "~/.local/state" alone or followed by a
        // `/` rebases; a sibling like "~/.local/stateful" is somebody
        // else's directory and expands as written.
        if rest.is_empty() || rest.starts_with('/') {
            return xdg.join(rest.trim_start_matches('/'));
        }
    }
    expand_tilde(env, raw)
}

/// [`expand_state_path_with`] over the process environment (the production
/// convenience for `degenbot-runs`).
#[must_use]
pub fn expand_state_path(raw: &str) -> PathBuf {
    expand_state_path_with(&crate::ProcessEnv, raw)
}

/// Resolve the database path: `--database` > `DEGENBOT_DB_PATH` >
/// `database.path` > `<state_home>/degenbot/db/degenbot.db`.
///
/// A value an operator left blank is treated as absent (the same
/// "this layer supplied nothing" rule the env-family merge uses).
///
/// The winning value has a leading `~` expanded against `HOME`, so a
/// hand-written `~/.local/state/...` never resolves against the process cwd.
/// Only the schema default consults `$XDG_STATE_HOME`; a value an operator
/// wrote (file, environment, or argument) expands as written.
///
/// The env seam supplies `HOME` / `$XDG_STATE_HOME` for that expansion — it is
/// not a config layer, and every `DEGENBOT_*` layer value is read from `cfg`.
#[must_use]
pub fn resolve_database_path(cfg: &LoadedConfig, cli_database: Option<&str>) -> Resolved<PathBuf> {
    resolve_database_path_with(cfg, cli_database, &crate::ProcessEnv)
}

/// [`resolve_database_path`] over an explicit env seam (tests and embedding
/// hosts that do not touch the process environment).
#[must_use]
pub fn resolve_database_path_with(
    cfg: &LoadedConfig,
    cli_database: Option<&str>,
    env: &dyn EnvVars,
) -> Resolved<PathBuf> {
    let (raw, source) = match non_empty(cli_database) {
        Some(cli) => (PathBuf::from(cli), Source::Cli),
        None if cfg.config.database.path.as_os_str().is_empty() => {
            // A blank declared value is "this layer supplied nothing" (the
            // crate's one spelling), so it falls through to the schema
            // default instead of resolving to the empty path.
            (PathBuf::from(DB_PATH_DEFAULT), Source::Default)
        }
        None => {
            // The declared key carries the schema default, so the file layer
            // and the default are one field distinguished by provenance.
            let source = cfg.source_of(DB_PATH_ENV).unwrap_or(Source::File);
            (cfg.config.database.path.clone(), source)
        }
    };
    let value = match source {
        Source::Default => expand_state_path_with(env, &raw.to_string_lossy()),
        Source::File | Source::Cli | Source::Env => expand_tilde(env, &raw.to_string_lossy()),
    };
    Resolved::new(value, source)
}

/// Resolve the session chain id: `--chain-id` > `DEGENBOT_DEFAULT_CHAIN_ID` >
/// `session.chain_id`.
///
/// # Errors
///
/// [`ConfigError`] when no layer supplied a value (the message names every
/// layer consulted), or when the explicit argument is not an integer (the
/// message names that layer). A declared value is already validated as a
/// positive integer by the load.
pub fn resolve_chain_id(
    cfg: &LoadedConfig,
    cli_chain_id: Option<&str>,
) -> Result<Resolved<u64>, ConfigError> {
    if let Some(cli) = non_empty(cli_chain_id) {
        return cli
            .trim()
            .parse::<u64>()
            .map(|chain_id| Resolved::new(chain_id, Source::Cli))
            .map_err(|_| {
                ConfigError::of(vec![format!(
                    "--chain-id (explicit layer) = {cli:?} is not a valid chain id \
                     (expected a positive integer, e.g. 1)"
                )])
            });
    }
    if let Some(chain_id) = cfg.config.session.chain_id {
        return Ok(Resolved::new(
            chain_id,
            cfg.source_of(DEFAULT_CHAIN_ID_ENV).unwrap_or(Source::File),
        ));
    }
    Err(ConfigError::of(vec![format!(
        "no session chain id resolved: layers consulted (highest precedence first) were \
         --chain-id (explicit, unset), {DEFAULT_CHAIN_ID_ENV} (env, unset), and \
         session.chain_id (file, unset); no localhost default exists and the \
         endpoint tables are keyed by chain id, so the chain must be named — set \
         {DEFAULT_CHAIN_ID_ENV} or pass --chain-id"
    )]))
}

/// Resolve the node RPC URI for a consumer with the `scope` capability
/// (ADR-062 D3).
///
/// Every transport the scope accepts resolves through the four layers
/// INDEPENDENTLY, and then the winner is chosen by two rules that are easy to
/// invert:
///
/// 1. the scope filters the transports — a subscription consults `nodes.ipc`
///    and `nodes.ws` and never `nodes.http`, so it fails loudly rather than
///    falling back to polling;
/// 2. LAYERS OUTRANK TRANSPORT PREFERENCE — if any transport in scope has an
///    explicit or exported endpoint, the most preferred of THOSE wins, and the
///    file entries compete by preference only when no explicit value exists
///    anywhere. A file `ipc` entry therefore loses to an exported `http` one,
///    which is what keeps a container's env overrides working against a
///    bind-mounted host file.
///
/// # Errors
///
/// [`ConfigError`] when no layer supplied an endpoint for `chain_id`; the
/// message names the scope, the transports it consulted, the layers each was
/// read through, and states that no localhost default is applied.
pub fn resolve_node_uri(
    cfg: &LoadedConfig,
    chain_id: u64,
    scope: NodeScope,
    overrides: &NodeOverrides,
) -> Result<Resolved<String>, ConfigError> {
    let chain = chain_id.to_string();
    let mut from_file: Option<Resolved<String>> = None;
    for transport in scope.transports() {
        let Some(candidate) = transport_candidate(cfg, *transport, &chain, overrides) else {
            continue;
        };
        match candidate.source {
            Source::Cli | Source::Env => return Ok(candidate),
            Source::Default | Source::File => {
                from_file.get_or_insert(candidate);
            }
        }
    }
    if let Some(resolved) = from_file {
        return Ok(resolved);
    }
    Err(ConfigError::of(vec![unresolved_endpoint_message(
        cfg, chain_id, scope,
    )]))
}

/// The endpoint one transport resolves to for `chain`, with the layer that
/// supplied it. The explicit layer wins outright for its transport; otherwise
/// the loaded table's entry carries its own per-entry provenance (ADR-062 D2).
fn transport_candidate(
    cfg: &LoadedConfig,
    transport: NodeTransport,
    chain: &str,
    overrides: &NodeOverrides,
) -> Option<Resolved<String>> {
    if let Some(value) = non_empty(overrides.get(transport)) {
        return Some(Resolved::new(value.to_string(), Source::Cli));
    }
    let value = non_empty(node_table(cfg, transport)?.get(chain))?;
    // Every entry a layer contributed is recorded in `entry_provenance`; an
    // entry with no record could only have come from the file layer, which is
    // the floor of the cascade.
    let source = cfg
        .entry_source_of(transport.env_prefix(), chain)
        .unwrap_or(Source::File);
    Some(Resolved::new(value.to_string(), source))
}

/// The loaded entries of one transport's table.
fn node_table(cfg: &LoadedConfig, transport: NodeTransport) -> Option<&BTreeMap<String, String>> {
    match transport {
        NodeTransport::Ipc => cfg.config.nodes.ipc.as_ref(),
        NodeTransport::Ws => cfg.config.nodes.ws.as_ref(),
        NodeTransport::Http => cfg.config.nodes.http.as_ref(),
    }
}

/// Resolve the node URI a REQUEST consumer uses (pool reads, `eth_callMany`,
/// transaction submission): ipc, then ws, then http.
///
/// # Errors
///
/// [`ConfigError`] when no layer supplied an endpoint for `chain_id`; the
/// message names every layer and transport consulted.
pub fn resolve_node_request_uri(
    cfg: &LoadedConfig,
    chain_id: u64,
    overrides: &NodeOverrides,
) -> Result<Resolved<String>, ConfigError> {
    resolve_node_uri(cfg, chain_id, NodeScope::Request, overrides)
}

/// Resolve the node URI a SUBSCRIPTION consumer uses (the pump, the head/logs
/// streams): ipc, then ws — never http, because an HTTP endpoint cannot carry
/// a feed.
///
/// # Errors
///
/// [`ConfigError`] when no accepted transport supplied an endpoint for
/// `chain_id`; the message names the two transports the scope accepts even
/// when a `nodes.http` entry exists, so the operator sees why the feed is
/// unresolved rather than finding a silent downgrade to polling.
pub fn resolve_node_subscription_uri(
    cfg: &LoadedConfig,
    chain_id: u64,
    overrides: &NodeOverrides,
) -> Result<Resolved<String>, ConfigError> {
    resolve_node_uri(cfg, chain_id, NodeScope::Subscription, overrides)
}

/// The refusal for a chain no layer names: the scope, the transports it
/// consults in preference order, the layers each is read through, and the
/// fact that the fourth layer is THIS error rather than a localhost default.
fn unresolved_endpoint_message(cfg: &LoadedConfig, chain_id: u64, scope: NodeScope) -> String {
    let transports = scope.transports();
    let names: Vec<&str> = transports.iter().map(|t| t.as_str()).collect();
    let env_names: Vec<String> = transports
        .iter()
        .map(|t| format!("{}{chain_id}", t.env_prefix()))
        .collect();
    let keys: Vec<&str> = transports.iter().map(|t| t.key_path()).collect();
    let remedy = env_names[0].clone();
    let mut message = format!(
        "no {scope} endpoint resolved for chain {chain_id}: transports consulted (preference \
         order) were {}; for each, the layers consulted (highest precedence first) were an \
         explicit node argument, the {} env families, and the {} file tables — none of \
         them supplied chain {chain_id}; there is no localhost default, and the refusal is \
         the fourth layer — declare the endpoint in the operator file, export {remedy}, or \
         pass the endpoint explicitly",
        names.join(", "),
        env_names.join(", "),
        keys.join(", "),
    );
    // A request-only transport holding the chain's endpoint is the surprise
    // worth naming: the operator wrote one, and the scope may not take it.
    let chain = chain_id.to_string();
    for transport in NodeScope::Request.transports() {
        if scope.transports().contains(transport) {
            continue;
        }
        let entry = node_table(cfg, *transport).and_then(|table| table.get(&chain));
        if non_empty(entry).is_some() {
            // A write! cannot fail into a String.
            let _ = write!(
                message,
                " (chain {chain_id} does have a {} entry in {}, which serves requests but not a \
                 {} feed — give the feed its own nodes.ipc or nodes.ws entry)",
                transport.as_str(),
                transport.key_path(),
                scope.as_str(),
            );
            break;
        }
    }
    message
}
