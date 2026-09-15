//! The 12-factor loader: precedence CLI > env > file > defaults.
//!
//! Layer order is applied to the typed default value; every assignment is
//! recorded in [`LoadedConfig::provenance`] so operators/tests can see WHICH
//! layer supplied each key.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::ConfigError;
use crate::schema::{BotConfig, SCHEMA};

/// Which layer supplied a key's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    /// Built-in default from the schema declaration.
    Default,
    /// `--config` TOML file.
    File,
    /// `DEGENBOT_*` environment variable.
    Env,
    /// CLI / explicit argument override.
    Cli,
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Default => "default",
            Self::File => "file",
            Self::Env => "env",
            Self::Cli => "cli",
        })
    }
}

/// Environment variable provider (test seam so no test ever mutates the
/// process environment).
pub trait EnvVars {
    /// Env lookup; `None` when unset.
    fn get(&self, name: &str) -> Option<String>;
}

/// The real process environment.
pub struct ProcessEnv;

impl EnvVars for ProcessEnv {
    fn get(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

/// Overlay map environment (tests, embedding hosts).
#[derive(Debug, Clone, Default)]
pub struct MapEnv(BTreeMap<String, String>);

impl MapEnv {
    /// Build from an ordered map.
    #[must_use]
    pub fn new(map: BTreeMap<String, String>) -> Self {
        Self(map)
    }
}

impl EnvVars for MapEnv {
    fn get(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }
}

/// Migration doc named by every retired-layout refusal .
const MIGRATION_DOC: &str = "docs/config-migration.md";

/// Retired operator-file layout items (ergo JLFE2F, Option B hard cutover):
/// top-level keys/section names from the pre-0.6 config.toml vocabulary the
/// typed schema never carried. A surviving item fails the load with a
/// POINTED problem naming the replacement surface and the migration doc —
/// silent fail-open here would trade on settings the typed file layer never
/// received.
pub(crate) const RETIRED_LAYOUT_ITEMS: &[(&str, &str)] = &[
    (
        "rpc",
        "move per-chain RPC endpoints to the DEGENBOT_RPC_HTTP_CHAINID_<chain> env names (or the Python config.py cascade)",
    ),
    (
        "ws",
        "move per-chain WebSocket endpoints to the DEGENBOT_RPC_WS_CHAINID_<chain> env names (or the Python config.py cascade)",
    ),
    (
        "database",
        "the database path is Python-driver domain: set it in the Python config cascade",
    ),
    (
        "otel",
        "the [otel] table is retired: use the modern telemetry section (telemetry.otel, telemetry.jaeger_endpoint)",
    ),
    (
        "default_chain_id",
        "default_chain_id is Python-driver domain: set it in the Python config cascade",
    ),
];

/// Sanctioned free-form file sections the loader SKIPS: not typed, not
/// retired. Read as raw tables by `file_path()` consumers sharing the
/// same file.
///  - `[failure_policy]` (ADR-040 D3): per-bucket override table owned by
///    degenbot-python's failure-policy reader; freedom-of-policy outlives
///    the typed schema.
pub(crate) const FREE_FORM_FILE_SECTIONS: &[&str] = &["failure_policy"];

/// Loaded result: the typed config plus per-key provenance.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    /// The typed configuration.
    pub config: BotConfig,
    /// Winning source per env key name (one entry per schema key).
    pub provenance: BTreeMap<&'static str, Source>,
}

impl LoadedConfig {
    /// Which layer supplied `env_key` (e.g. `DEGENBOT_OTEL`).
    #[must_use]
    pub fn source_of(&self, env: &str) -> Option<Source> {
        self.provenance.get(env).copied()
    }
}

/// Builder for the layered load. The default env source is the process
/// environment (12-factor: env > file > defaults; tests replace it via
/// [`Self::with_env`] with a [`MapEnv`] or disable it via
/// [`Self::without_env`]).
pub struct BotConfigLoader {
    file: Option<PathBuf>,
    cli: Vec<(String, String)>,
    env: Option<Box<dyn EnvVars>>,
}

impl Default for BotConfigLoader {
    /// Defaults + process environment (no file, no CLI) — the documented
    /// production surface. A `#[derive(Default)]` here silently produced a
    /// no-env loader (K7-config gap: production boots ignored every
    /// DEGENBOT_* override).
    fn default() -> Self {
        Self {
            file: None,
            cli: Vec::new(),
            env: Some(Box::new(ProcessEnv)),
        }
    }
}

impl std::fmt::Debug for BotConfigLoader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotConfigLoader")
            .field("file", &self.file)
            .field("cli", &self.cli)
            .field(
                "env",
                &if self.env.is_some() {
                    "<custom>"
                } else {
                    "<process>"
                },
            )
            .finish()
    }
}

/// The canonical STANDARD config file path : the
/// `DEGENBOT_CONFIG` env override when set — even when missing, the
/// operator asked for it — else `$HOME/.config/degenbot/config.toml` when
/// it exists, else `None` (an absent user file is contractually defaults).
/// `BotConfigLoader::with_standard_file_paths` selects exactly this value,
/// and raw-table readers resolve the SAME file through this function so
/// file discovery stays a single contract. The std env reads live in THIS
/// crate so they stay confined to degenbot-config.
#[must_use]
pub fn standard_file_path() -> Option<PathBuf> {
    if let Some(p) = ::std::env::var("DEGENBOT_CONFIG")
        .ok()
        .filter(|s| !s.is_empty())
    {
        return Some(p.into());
    }
    let home = ::std::env::var_os("HOME")?;
    let path = ::std::path::Path::new(&home).join(".config/degenbot/config.toml");
    path.is_file().then_some(path)
}

impl BotConfigLoader {
    /// Empty loader: defaults only (no env, no file, no CLI) until a layer
    /// is attached with the `with_*` builders.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Select the config file (the `--config <path>` surface). Replaces any
    /// previously selected path.
    #[must_use]
    pub fn with_config_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.file = Some(path.into());
        self
    }

    /// Select the STANDARD file layer: the `DEGENBOT_CONFIG` env override when
    /// set (missing file then fails the load — the operator asked for it),
    /// else `$HOME/.config/degenbot/config.toml` when it exists, else no file
    /// layer (12-factor: an absent user file is contractually defaults). The
    /// env read lives HERE so std env stays confined to this crate.
    #[must_use]
    pub fn with_standard_file_paths(mut self) -> Self {
        self.file = standard_file_path();
        self
    }

    /// The file layer chosen by [`Self::with_config_path`] /
    /// [`Self::with_standard_file_paths`], if any — so a caller that can only
    /// read files through this crate (e.g. the `[failure_policy]` table) sees
    /// the SAME file the loader did.
    #[must_use]
    pub fn file_path(&self) -> Option<&PathBuf> {
        self.file.as_ref()
    }

    /// Add one CLI / explicit override. The key may be the `DEGENBOT_*` env
    /// name OR the dotted TOML path (e.g. `solve.executor`).
    #[must_use]
    pub fn with_cli(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.cli.push((key.into(), value.into()));
        self
    }

    /// Add CLI overrides in bulk (key = env name or TOML path).
    #[must_use]
    pub fn with_cli_overrides(
        mut self,
        overrides: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        self.cli.extend(overrides);
        self
    }

    /// Replace the environment source (tests pass a [`MapEnv`]; production
    /// keeps the default [`ProcessEnv`]).
    #[must_use]
    pub fn with_env(mut self, env: Box<dyn EnvVars>) -> Self {
        self.env = Some(env);
        self
    }

    /// Drop the environment layer entirely (file-vs-default tests).
    #[must_use]
    pub fn without_env(mut self) -> Self {
        self.env = None;
        self
    }

    /// Run the layered load. Fails closed with ALL problems aggregated.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] when the file is unreadable/unparsable, a TOML key is
    /// unknown, a value does not parse into the declared kind, or a CLI
    /// override names an undeclared key.
    pub fn load(&self) -> Result<LoadedConfig, ConfigError> {
        let mut problems: Vec<String> = Vec::new();
        let mut config = BotConfig::default();

        // Defaults first: every schema key starts at its declared default.
        let mut provenance: BTreeMap<&'static str, Source> =
            SCHEMA.iter().map(|k| (k.env, Source::Default)).collect();

        // Layer 2 (lowest override): --config TOML file.
        if let Some(path) = &self.file {
            Self::apply_file(path, &mut config, &mut provenance, &mut problems);
        }

        // Layer 3: environment. Iterate the SCHEMA (not the process env) so
        // foreign DEGENBOT_*-prefixed vars never leak into the typed tree.
        if let Some(env) = &self.env {
            for key in SCHEMA {
                if let Some(raw) = env.get(key.env) {
                    match config.assign(key.section, key.field, &raw) {
                        Ok(()) => {
                            provenance.insert(key.env, Source::Env);
                        }
                        Err(problem) => problems.push(problem),
                    }
                }
            }
        }

        // Layer 4 (highest): CLI / explicit argument overrides.
        for (name, value) in &self.cli {
            match resolve_key(name) {
                Some(key) => match config.assign(key.section, key.field, value) {
                    Ok(()) => {
                        provenance.insert(key.env, Source::Cli);
                    }
                    Err(problem) => problems.push(problem),
                },
                None => problems.push(format!(
                    "cli override {name:?} does not name a schema key (env name or TOML path required)"
                )),
            }
        }

        if problems.is_empty() {
            Ok(LoadedConfig { config, provenance })
        } else {
            Err(ConfigError::of(problems))
        }
    }

    fn apply_file(
        path: &Path,
        config: &mut BotConfig,
        provenance: &mut BTreeMap<&'static str, Source>,
        problems: &mut Vec<String>,
    ) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => {
                problems.push(format!("--config {}: unreadable: {e}", path.display()));
                return;
            }
        };
        let value: toml::Table = match text.parse() {
            Ok(value) => value,
            Err(e) => {
                problems.push(format!("--config {}: parse error: {e}", path.display()));
                return;
            }
        };
        // A parsed `toml::Table` IS the top-level table.
        let table = &value;
        for (section, section_value) in table {
            // Sanctioned free-form tables (e.g. [failure_policy], ADR-040
            // D3): not typed into the schema and NOT retired — consumers
            // read them from the same file this loader selected (see
            // file_path()). Skip silently so the typed file layer and the
            // raw-table reader can share one file.
            if FREE_FORM_FILE_SECTIONS.contains(&section.as_str()) {
                continue;
            }
            // Retired pre-0.6 layout vocabulary (ergo JLFE2F, Option B hard
            // cutover): fail pointed, naming the replacement surface and the
            // migration doc.
            if let Some((_, replacement)) = RETIRED_LAYOUT_ITEMS
                .iter()
                .find(|(name, _)| section.as_str() == *name)
            {
                problems.push(format!(
                    "--config {}: retired config-layout item [{section}] is no \
                     longer supported — {replacement}; see {MIGRATION_DOC}",
                    path.display()
                ));
                continue;
            }
            let members: Vec<_> = SCHEMA
                .iter()
                .filter(|k| k.section == section.as_str())
                .collect();
            if members.is_empty() {
                problems.push(format!(
                    "--config {}: unknown section [{section}]",
                    path.display()
                ));
                continue;
            }
            let Some(section_table) = section_value.as_table() else {
                problems.push(format!(
                    "--config {}: section [{section}] must be a table",
                    path.display()
                ));
                continue;
            };
            for (field, field_value) in section_table {
                let Some(key) = members.iter().copied().find(|k| k.field == field.as_str()) else {
                    problems.push(format!(
                        "--config {}: unknown key {field} in section [{section}]",
                        path.display()
                    ));
                    continue;
                };
                // Map-kind keys accept the nested table form (the primary
                // `[telemetry.diag]` surface) and the flat string form.
                let raw = if matches!(key.kind.base, crate::schema::BaseKind::Map(_)) {
                    map_value_to_raw(field_value, key, path, problems)
                } else {
                    toml_value_to_raw(field_value, key.toml_path, path, problems)
                };
                let Some(raw) = raw else {
                    continue;
                };
                match config.assign(key.section, key.field, &raw) {
                    Ok(()) => {
                        provenance.insert(key.env, Source::File);
                    }
                    Err(problem) => problems.push(problem),
                }
            }
        }
    }
}

/// Resolve a CLI override key: exact env name first, then TOML path.
fn resolve_key(name: &str) -> Option<&'static crate::schema::KeyDecl> {
    SCHEMA.iter().find(|k| k.env == name || k.toml_path == name)
}

/// Render a TOML `[section.map]` table (or a flat string) into the
/// `key=value,...` raw form the map parser consumes.
fn map_value_to_raw(
    value: &toml::Value,
    key: &'static crate::schema::KeyDecl,
    path: &Path,
    problems: &mut Vec<String>,
) -> Option<String> {
    let toml::Value::Table(t) = value else {
        return toml_value_to_raw(value, key.toml_path, path, problems);
    };
    let mut parts: Vec<String> = Vec::new();
    let mut bad = false;
    for (k, v) in t {
        if let Some(level) = v.as_str() {
            parts.push(format!("{k}={level}"));
        } else {
            problems.push(format!(
                "--config {}: {}.{k} must be a string level",
                path.display(),
                key.toml_path
            ));
            bad = true;
        }
    }
    if bad {
        return None;
    }
    Some(parts.join(","))
}

/// Convert a TOML scalar into the normalized raw text the typed parser
/// consumes (`bool`/integer/float render to their textual form; the `u128`
/// wei kind is declared as a quoted string in TOML).
fn toml_value_to_raw(
    value: &toml::Value,
    label: &str,
    path: &Path,
    problems: &mut Vec<String>,
) -> Option<String> {
    let rendered = match value {
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(float) => float.to_string(),
        toml::Value::String(s) => s.clone(),
        other => {
            problems.push(format!(
                "--config {}: {label}: unsupported TOML value {other:?} (expected bool/integer/float/string)",
                path.display()
            ));
            return None;
        }
    };
    Some(rendered)
}
