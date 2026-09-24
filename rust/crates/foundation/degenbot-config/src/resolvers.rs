//! Driver-domain resolvers (ADR-051 D8): database path, session chain id,
//! and node (HTTP/WS) RPC URI.
//!
//! These three values are needed before any console command runs, but they are
//! NOT typed-schema keys: they never lived in the `BotConfig` file layer, and
//! the retired `[rpc]`/`[ws]`/`[database]`/`default_chain_id` TOML vocabulary
//! stays refused (the config-migration Option-B cutover stands). The resolvers
//! therefore read the SAME layers the Python cascade reads — a CLI argument and
//! a `DEGENBOT_*` environment variable — and deliberately do not re-add file
//! vocabulary.
//!
//! They are plain functions over the loader's [`EnvVars`] seam (never
//! `std::env`) plus explicit `Option<&str>` CLI-override arguments, and every
//! result carries the winning [`Source`] exactly as [`LoadedConfig`]
//! provenance does. The empty string and an absent variable are
//! indistinguishable (both mean "this layer supplied nothing"), so an
//! exported-but-blank variable cannot silently become a value.
//!
//! Precedence, highest first:
//!
//! | value       | layers                                                    |
//! |-------------|-----------------------------------------------------------|
//! | database    | `--database` > `DEGENBOT_DB_PATH` > state-home default    |
//! | chain id    | `--chain-id` > `DEGENBOT_DEFAULT_CHAIN_ID`                |
//! | HTTP RPC    | `--node-http` > `DEGENBOT_RPC_HTTP_CHAINID_<id>`          |
//! | WS RPC      | `--node-ws` > `DEGENBOT_RPC_WS_CHAINID_<id>`              |
//!
//! The database default lives under the XDG state home: `$XDG_STATE_HOME`
//! (absolute only) else `$HOME/.local/state`, then `degenbot/db/degenbot.db`.
//! The state-rooted schema keys (`logging.runs_dir`, `persistence.state_dir`)
//! share the same base, expanded by [`expand_state_path`].
//!
//! [`LoadedConfig`]: crate::LoadedConfig

use std::path::{Path, PathBuf};

use crate::error::ConfigError;
use crate::loader::{EnvVars, Source};

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
/// chain id (dynamic, intentionally not a schema key).
pub const RPC_WS_ENV_PREFIX: &str = "DEGENBOT_RPC_WS_CHAINID_";

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

/// The resolved node-URI pair (each URI tagged with its own source).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedNodeUris {
    /// The HTTP/IPC endpoint.
    pub http: Resolved<String>,
    /// The WebSocket endpoint.
    pub ws: Resolved<String>,
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

/// Treat an empty string exactly like an absent layer.
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|v| !v.is_empty())
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
/// `<state_home>/degenbot/db/degenbot.db`.
///
/// The winning value has a leading `~` expanded against `HOME` (read
/// through the env seam), so a hand-written `~/.local/state/...` never
/// resolves against the process cwd. Only the built-in default consults
/// `$XDG_STATE_HOME`; an explicit CLI/env path is expanded as written.
#[must_use]
pub fn resolve_database_path(env: &dyn EnvVars, cli_database: Option<&str>) -> Resolved<PathBuf> {
    let env_value = env.get(DB_PATH_ENV);
    let (raw, source) = match (non_empty(cli_database), non_empty(env_value.as_deref())) {
        (Some(cli), _) => (cli, Source::Cli),
        (None, Some(envv)) => (envv, Source::Env),
        (None, None) => (DB_PATH_DEFAULT, Source::Default),
    };
    let value = match source {
        Source::Default => expand_state_path_with(env, raw),
        Source::File | Source::Cli | Source::Env => expand_tilde(env, raw),
    };
    Resolved::new(value, source)
}

/// Resolve the session chain id: `--chain-id` > `DEGENBOT_DEFAULT_CHAIN_ID`.
///
/// # Errors
///
/// [`ConfigError`] when neither layer supplied a value, or when the winning
/// layer is set to a non-integer (the message names that layer).
pub fn resolve_chain_id(
    env: &dyn EnvVars,
    cli_chain_id: Option<&str>,
) -> Result<Resolved<u64>, ConfigError> {
    let env_value = env.get(DEFAULT_CHAIN_ID_ENV);
    if let Some(cli) = non_empty(cli_chain_id) {
        return parse_chain_id(cli, "cli", "--chain-id");
    }
    if let Some(envv) = non_empty(env_value.as_deref()) {
        return parse_chain_id(envv, "env", DEFAULT_CHAIN_ID_ENV);
    }
    Err(ConfigError::of(vec![format!(
        "no chain id resolved: layers consulted (highest precedence first) were \
         --chain-id (CLI, unset) and {DEFAULT_CHAIN_ID_ENV} (env, unset); the \
         retired default_chain_id file key is deliberately not consulted \
         (ADR-051 D8) — set {DEFAULT_CHAIN_ID_ENV} or pass --chain-id"
    )]))
}

/// Parse a chain-id layer value, tagging the layer in the error text.
fn parse_chain_id(raw: &str, layer: &str, label: &str) -> Result<Resolved<u64>, ConfigError> {
    match raw.trim().parse::<u64>() {
        Ok(chain_id) => Ok(Resolved::new(chain_id, source_of_layer(layer))),
        Err(_) => Err(ConfigError::of(vec![format!(
            "{label} ({layer} layer) = {raw:?} is not a valid chain id \
             (expected a positive integer, e.g. 1)"
        )])),
    }
}

/// Map the internal layer tag to its [`Source`].
fn source_of_layer(layer: &str) -> Source {
    if layer == "cli" {
        Source::Cli
    } else {
        Source::Env
    }
}

/// Resolve the HTTP/IPC RPC URI: `--node-http` >
/// `DEGENBOT_RPC_HTTP_CHAINID_<chain_id>`.
///
/// # Errors
///
/// [`ConfigError`] when no layer supplied a non-empty value; the message
/// names every layer consulted. There is deliberately no localhost default
/// and no file-table layer.
pub fn resolve_node_http_uri(
    env: &dyn EnvVars,
    chain_id: u64,
    cli_node_http: Option<&str>,
) -> Result<Resolved<String>, ConfigError> {
    let env_name = node_http_env_name(chain_id);
    resolve_node_uri(
        env,
        chain_id,
        "HTTP",
        "rpc",
        "--node-http",
        &env_name,
        cli_node_http,
    )
}

/// Resolve the WebSocket RPC URI: `--node-ws` >
/// `DEGENBOT_RPC_WS_CHAINID_<chain_id>`.
///
/// # Errors
///
/// [`ConfigError`] when no layer supplied a non-empty value; the message
/// names every layer consulted. There is deliberately no localhost default
/// and no file-table layer.
pub fn resolve_node_ws_uri(
    env: &dyn EnvVars,
    chain_id: u64,
    cli_node_ws: Option<&str>,
) -> Result<Resolved<String>, ConfigError> {
    let env_name = node_ws_env_name(chain_id);
    resolve_node_uri(
        env,
        chain_id,
        "WS",
        "ws",
        "--node-ws",
        &env_name,
        cli_node_ws,
    )
}

/// Resolve both node URIs, each through its own cascade, aggregating every
/// unresolved endpoint into one [`ConfigError`] (the loader's fail-closed
/// posture).
///
/// # Errors
///
/// [`ConfigError`] when either URI is unresolved; the message carries one
/// problem line per missing endpoint.
pub fn resolve_node_uris(
    env: &dyn EnvVars,
    chain_id: u64,
    cli_node_http: Option<&str>,
    cli_node_ws: Option<&str>,
) -> Result<ResolvedNodeUris, ConfigError> {
    let http = resolve_node_http_uri(env, chain_id, cli_node_http);
    let ws = resolve_node_ws_uri(env, chain_id, cli_node_ws);
    match (http, ws) {
        (Ok(http), Ok(ws)) => Ok(ResolvedNodeUris { http, ws }),
        (http, ws) => {
            let mut problems = Vec::new();
            if let Err(e) = http {
                problems.extend(e.problems);
            }
            if let Err(e) = ws {
                problems.extend(e.problems);
            }
            Err(ConfigError::of(problems))
        }
    }
}

/// Shared single-URI cascade: CLI > per-chain env var > hard error naming
/// every layer consulted.
fn resolve_node_uri(
    env: &dyn EnvVars,
    chain_id: u64,
    kind: &str,
    file_section: &str,
    cli_flag: &str,
    env_name: &str,
    cli_value: Option<&str>,
) -> Result<Resolved<String>, ConfigError> {
    if let Some(cli) = non_empty(cli_value) {
        return Ok(Resolved::new(cli.to_string(), Source::Cli));
    }
    if let Some(envv) = non_empty(env.get(env_name).as_deref()) {
        return Ok(Resolved::new(envv.to_string(), Source::Env));
    }
    Err(ConfigError::of(vec![format!(
        "no {kind} RPC endpoint resolved for chain {chain_id}: layers consulted \
         (highest precedence first) were {cli_flag} (CLI, unset) and {env_name} \
         (env, unset); no localhost default is applied and the retired \
         [{file_section}] file table is deliberately not consulted (ADR-051 D8) \
         — set {env_name} or pass {cli_flag}"
    )]))
}
