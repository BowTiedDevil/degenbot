#![recursion_limit = "2048"]

//! Typed `BotConfig` schema + 12-factor loader for degenbot (file + env parity).
//!
//! # One declaration site per key
//!
//! Every configuration key is declared EXACTLY once, in [`schema::SCHEMA`]
//! (expanded by `config_schema!`). That single declaration produces:
//!
//! 1. the typed Rust field on the sectioned [`BotConfig`] struct,
//! 2. the `DEGENBOT_*` environment variable mapping,
//! 3. the dotted TOML path in the config-file tree,
//! 4. the generated key-reference doc ([`doc::render_key_reference`]),
//!    written to `docs/rust-config-keys.md` by the doc-generation test.
//!
//! There are NO parallel hand-maintained lists of env names or TOML paths;
//! `SCHEMA` is the machine-checked registry.
//!
//! # Precedence (12-factor)
//!
//! Effective value for any key, highest wins:
//!
//! 1. **CLI / explicit argument** — passed to the loader as overrides
//!    (keyed by env name or TOML path),
//! 2. **environment** — the `DEGENBOT_*` variable,
//! 3. **config file** — the TOML tree selected by `--config <path>`,
//! 4. **built-in defaults** — declared with each key.
//!
//! This is the authoritative precedence ORDER for the whole bot; this
//! crate-level doc and the generated key-reference doc are rendered from
//! one source (the doc-generation test fails on drift).
//!
//! # Parity
//!
//! Every declared key is settable from the file OR the environment over the
//! same name tree (TOML `[section].field` <-> `DEGENBOT_<...>`). Env keys
//! keep the historical `DEGENBOT_*` spelling for operator continuity.
//!
//! # Strictness
//!
//! The loader is fail-closed: unparsable values and unknown TOML keys are
//! reported in one aggregate error rather than silently falling back. This
//! is a deliberate break from several per-site fail-open parses scattered
//! across the bot today; call sites migrate onto [`BotConfig`] in the
//! "Migrate env reads onto `BotConfig`" task and re-validate their fallback
//! semantics there.
//!
//! # Family-shaped keys and driver-domain resolvers
//!
//! A key whose entries the operator picks at runtime (the per-chain
//! `[nodes.*]` endpoint tables) cannot be spelled as one env variable: its env
//! layer is a NAME FAMILY. `KeyDecl::env_prefix` holds the `PREFIX_` that
//! `DEGENBOT_RPC_HTTP_CHAINID_<chain_id>`-style names share, and the loader
//! reads such a key by ENUMERATION ([`EnvVars::names_with_prefix`]) rather
//! than by a lookup. The declaration is still one static key, so the
//! inventory gate and the generated doc cover the family exactly once.
//!
//! Each entry then keeps the layer that supplied it in
//! [`LoadedConfig::entry_provenance`], because a family export overrides one
//! chain of the table and leaves the rest of it on the file layer.
//!
//! The console's driver-domain values are declared keys too
//! (`database.path`, `session.chain_id`, the `nodes.*` tables), and
//! [`resolvers`] reads all four layers out of a [`LoadedConfig`]. A resolver
//! never reads the environment itself: the loader owns that, so a resolver
//! sees one loaded value per key and reports which layer supplied it.

pub mod doc;
pub mod error;
/// the process-wide typed-config holder. The loader (the ONLY env
/// reader) produces a [`BotConfig`]; the boot path installs it here and every
/// formerly env-reading call site in the workspace reads a typed field off
/// [`holder::config`] instead. A plain VALUE holder — zero env access.
pub mod holder;
pub mod loader;
pub mod readiness;
pub mod redact;
pub mod resolvers;
pub mod schema;
#[doc(hidden)]
pub mod schema_macro;
pub mod writer;

pub use error::ConfigError;
pub use loader::{
    load_process_config, standard_file_path, standard_file_path_with, BotConfigLoader,
    EntryProvenance, EnvVars, LoadedConfig, MapEnv, ProcessEnv, Source,
};
pub use readiness::{
    strategy_readiness, validate_hosted_strategy_readiness, StrategyArm, StrategyReadiness,
    StrategyReadinessError, DEFAULT_BACKRUN_STREAM_URL, DEFAULT_TXPOOL_BACKRUN_RELAYS,
    SETTLEMENT_DEFAULT_ENDPOINTS,
};
pub use redact::redact_uri;
pub use resolvers::{
    config_home, expand_state_path, expand_state_path_with, expand_tilde_path, node_http_env_name,
    node_ipc_env_name, node_ws_env_name, resolve_chain_id, resolve_database_path,
    resolve_database_path_with, resolve_node_request_uri, resolve_node_subscription_uri,
    resolve_node_uri, state_home, NodeOverrides, NodeScope, Resolved, DB_PATH_DEFAULT, DB_PATH_ENV,
    DEFAULT_CHAIN_ID_ENV, RPC_HTTP_ENV_PREFIX, RPC_IPC_ENV_PREFIX, RPC_WS_ENV_PREFIX,
    XDG_CONFIG_HOME_ENV, XDG_STATE_HOME_ENV,
};
pub use schema::{
    AnchorSweep, FleetConfig, FleetProfile, LogLevel, QuiesceMode, StrategyMevblockerBackrunConfig,
    StrategySettlementConfig, StrategyTxpoolBackrunConfig, VerifyTicks,
};
pub use schema::{
    BaseKind, BotConfig, ConfigValue, KeyDecl, NodeTransport, ValueKind, READABLE_KEYS, SCHEMA,
    SECTION_PATHS, UNPREFIXED_ENV_NAMES, VALUES_PROJECTION,
};

/// The closed set of observability domains (ADR-043 section 3). A
/// `TelemetryConfig::diag` entry naming anything else is a boot error, so a
/// typo cannot silently no-op an escalation.
pub const OBSERVABILITY_DOMAINS: &[&str] = &[
    "state", "path", "solver", "sim", "pump", "exec", "verify", "ingest", "rpc", "aave",
];

/// Parse and validate the diag map: a comma-separated `domain=level` list
/// (the env encoding of the `[telemetry.diag]` table). Every domain must be
/// in [`OBSERVABILITY_DOMAINS`] and every level must parse into the generated
/// level enum; the first problem fails the load.
///
/// # Errors
///
/// Returns a description for a malformed entry, an unknown domain, or an
/// unparsable level.
pub fn parse_level_map<T>(raw: &str) -> Result<std::collections::BTreeMap<String, T>, String>
where
    T: std::str::FromStr<Err = String>,
{
    let mut out = std::collections::BTreeMap::new();
    for part in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let Some((domain, level)) = part.split_once('=') else {
            return Err(format!(
                "invalid diag entry {part:?} (expected domain=level, comma-separated)"
            ));
        };
        let domain = domain.trim();
        if !OBSERVABILITY_DOMAINS.contains(&domain) {
            return Err(format!(
                "unknown telemetry domain {domain:?} (expected one of: {})",
                OBSERVABILITY_DOMAINS.join(" ")
            ));
        }
        let level = T::from_str(level.trim())?;
        out.insert(domain.to_string(), level);
    }
    Ok(out)
}

/// Parse an operator-keyed `key=value` table: a comma-separated list, the
/// env encoding of a [`BaseKind::StrMap`] key and the flat-string twin of its
/// TOML table form. Surrounding whitespace around an entry and around its key
/// and value is trimmed; empty input is the empty table (an unset env value
/// means "no entries", not a malformed list). A repeated key keeps the last
/// value. An entry value MAY be empty — whether an entry is meaningful
/// (a transport URL, a chain id) is the declaring key's own validation, not
/// this parse's.
///
/// # Errors
///
/// Returns a description for an entry with no `=`, or an entry whose key
/// side is empty. An empty entry inside a non-empty list is refused for the
/// same reason: a dropped `1=x,,` would silently lose a value the operator
/// typed.
pub fn parse_string_map(raw: &str) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut out = std::collections::BTreeMap::new();
    if raw.trim().is_empty() {
        return Ok(out);
    }
    for part in raw.split(',') {
        let part = part.trim();
        let Some((key, value)) = part.split_once('=') else {
            return Err(format!(
                "invalid table entry {part:?} (expected key=value, comma-separated)"
            ));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(format!(
                "invalid table entry {part:?} (the key side is empty; expected key=value)"
            ));
        }
        out.insert(key.to_string(), value.trim().to_string());
    }
    Ok(out)
}

/// Parse a boolean flag value using the bot-wide truthy/falsey word lists.
///
/// Truthy: `1`, `true`, `yes`, `on`, `y`. Falsey: `0`, `false`, `off`, `no`,
/// `n` (case-insensitive, trimmed). Anything else (including an empty
/// value) is an error - the loader is fail-closed; per-site fail-open
/// variants migrate explicitly.
///
/// # Errors
///
/// Returns a description when the value is neither a known truthy nor a
/// known falsey word (including the empty string).
pub fn parse_bool_flag(raw: &str) -> Result<bool, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" | "y" => Ok(true),
        "0" | "false" | "off" | "no" | "n" => Ok(false),
        other => Err(format!(
            "invalid flag value {other:?} (expected a bool word: 1/true/yes/on/y or 0/false/off/no/n)"
        )),
    }
}
