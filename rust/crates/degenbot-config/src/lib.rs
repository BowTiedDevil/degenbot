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
//! # Dynamic (non-schema) env names
//!
//! `DEGENBOT_RPC_WS_CHAINID_<chain_id>` carries a numeric suffix at runtime
//! and is intentionally NOT a static key (documented next to `SCHEMA`).

pub mod doc;
pub mod error;
/// KAHU5W: the process-wide typed-config holder. The loader (the ONLY env
/// reader) produces a [`BotConfig`]; the boot path installs it here and every
/// formerly env-reading call site in the workspace reads a typed field off
/// [`holder::config`] instead. A plain VALUE holder — zero env access.
pub mod holder;
pub mod loader;
pub mod schema;
#[doc(hidden)]
pub mod schema_macro;

pub use error::ConfigError;
pub use loader::{
    standard_file_path, BotConfigLoader, EnvVars, LoadedConfig, MapEnv, ProcessEnv, Source,
};
pub use schema::{AnchorSweep, FleetConfig, FleetProfile, LogLevel, QuiesceMode};
pub use schema::{BaseKind, BotConfig, KeyDecl, ValueKind, SCHEMA};

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
