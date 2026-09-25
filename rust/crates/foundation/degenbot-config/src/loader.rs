//! The 12-factor loader: precedence CLI > env > file > defaults.
//!
//! Layer order is applied to the typed default value; every assignment is
//! recorded in [`LoadedConfig::provenance`] so operators/tests can see WHICH
//! layer supplied each key.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::ConfigError;
use crate::schema::{BotConfig, KeyDecl, SCHEMA, SECTION_PATHS};

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

    /// Every name this source holds that starts with `prefix`, sorted.
    ///
    /// A family-shaped key is one env name per operator-chosen entry, so
    /// reading it needs enumeration rather than a lookup. The default
    /// reports no names: a source that cannot list its own environment stays
    /// usable for every single-name key instead of being forced to fake an
    /// inventory.
    fn names_with_prefix(&self, _prefix: &str) -> Vec<String> {
        Vec::new()
    }
}

/// The real process environment.
pub struct ProcessEnv;

impl EnvVars for ProcessEnv {
    fn get(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn names_with_prefix(&self, prefix: &str) -> Vec<String> {
        // The process environment has no ordering, and a name that is not
        // valid UTF-8 cannot be spelled in a config file, so it is not a
        // member of any name family an operator can write.
        let mut names: Vec<String> = std::env::vars_os()
            .filter_map(|(name, _)| {
                let name = name.to_str()?;
                name.starts_with(prefix).then_some(name.to_string())
            })
            .collect();
        names.sort_unstable();
        names
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

    fn names_with_prefix(&self, prefix: &str) -> Vec<String> {
        // `BTreeMap` iterates in key order, so the filtered scan already is
        // the sorted answer the trait promises.
        self.0
            .keys()
            .filter(|name| name.starts_with(prefix))
            .cloned()
            .collect()
    }
}

/// Migration doc named by every retired-layout refusal .
const MIGRATION_DOC: &str = "docs/config-migration.md";

/// Retired operator-file layout items: top-level keys/section names from the
/// pre-0.6 config.toml vocabulary that no key replaced. A surviving item fails
/// the load with a POINTED problem naming the key that replaced it and the
/// migration doc — silent fail-open here would trade on settings the typed
/// file layer never received.
///
/// The endpoint tables and the database path are NOT here: `[nodes.http]`,
/// `[nodes.ws]`, `[nodes.ipc]`, and `[database].path` are declared keys
/// (ADR-062 D2/D4), so the old `[rpc]`/`[ws]` spellings are simply undeclared
/// sections and keep the generic unknown-section shape. ADR-062 D13 declined a
/// shim for them, so the refusal is not reshaped into a translation.
pub(crate) const RETIRED_LAYOUT_ITEMS: &[(&str, &str)] = &[
    (
        "otel",
        "the [otel] table is retired: use the modern telemetry section (telemetry.otel, telemetry.jaeger_endpoint)",
    ),
    (
        "default_chain_id",
        "default_chain_id is retired: declare the session chain id as session.chain_id in the [session] table, or export DEGENBOT_DEFAULT_CHAIN_ID",
    ),
];

/// Sanctioned free-form file sections the loader SKIPS: not typed, not
/// retired. Read as raw tables by `file_path()` consumers sharing the
/// same file.
///  - `[failure_policy]` (ADR-040 D3): per-bucket override table owned by
///    degenbot-python's failure-policy reader; freedom-of-policy outlives
///    the typed schema.
///  - `[deployments]` (ADR-062 D7): the Python deployment-registry overlay
///    table owned by `src/degenbot/registry/deployment_loader.py`; the
///    overlay lives outside the typed schema and outlives it.
pub(crate) const FREE_FORM_FILE_SECTIONS: &[&str] = &["failure_policy", "deployments"];

/// Per-ENTRY provenance for the map-kind keys: for each such key (addressed by
/// its env name — for a family-shaped key the PREFIX), the layer that supplied
/// each of the operator-chosen entries. A family-shaped env layer overrides
/// one entry at a time, so the key-level [`LoadedConfig::provenance`] entry
/// cannot describe the table on its own.
pub type EntryProvenance = BTreeMap<&'static str, BTreeMap<String, Source>>;

/// Loaded result: the typed config plus per-key provenance.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    /// The typed configuration.
    pub config: BotConfig,
    /// Winning source per env key name (one entry per schema key). For a
    /// map-kind key this is the HIGHEST-RANKED layer that contributed an
    /// entry, not the layer of every entry.
    pub provenance: BTreeMap<&'static str, Source>,
    /// Winning source per entry of each map-kind key.
    pub entry_provenance: EntryProvenance,
}

impl LoadedConfig {
    /// Which layer supplied `env_key` (e.g. `DEGENBOT_OTEL`).
    #[must_use]
    pub fn source_of(&self, env: &str) -> Option<Source> {
        self.provenance.get(env).copied()
    }

    /// Which layer supplied one entry of a map-kind key: `entry` is the
    /// operator-chosen table key (the chain id for a `[nodes.*]` table) and
    /// `env` is the key's env name — the PREFIX for a family-shaped key.
    #[must_use]
    pub fn entry_source_of(&self, env: &str, entry: &str) -> Option<Source> {
        self.entry_provenance.get(env)?.get(entry).copied()
    }
}

/// Builder for the layered load. The default env source is the process
/// environment (12-factor: env > file > defaults; tests replace it via
/// [`Self::with_env`] with a [`MapEnv`] or disable it via
/// [`Self::without_env`]).
pub struct BotConfigLoader<'a> {
    file: Option<PathBuf>,
    cli: Vec<(String, String)>,
    env: Option<Box<dyn EnvVars + 'a>>,
}

/// The borrowing env adapter behind [`BotConfigLoader::with_env_ref`].
struct EnvRef<'a>(&'a dyn EnvVars);

impl EnvVars for EnvRef<'_> {
    fn get(&self, name: &str) -> Option<String> {
        self.0.get(name)
    }

    fn names_with_prefix(&self, prefix: &str) -> Vec<String> {
        // Forwarded, not defaulted: the borrowed source is the real one, and
        // falling back to the empty default here would report "no family
        // names" for a host whose environment does have them.
        self.0.names_with_prefix(prefix)
    }
}

impl Default for BotConfigLoader<'_> {
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

impl std::fmt::Debug for BotConfigLoader<'_> {
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

/// The standard path resolved against an explicit [`EnvVars`] seam: the
/// `DEGENBOT_CONFIG` override when set, else
/// `$XDG_CONFIG_HOME/degenbot/config.toml` (absolute `XDG_CONFIG_HOME` only —
/// a relative or empty value is ignored per the XDG spec), else
/// `$HOME/.config/degenbot/config.toml` when that file exists, else `None`.
/// Tests drive HOME/XDG through a [`MapEnv`] so no test mutates the process
/// environment.
#[must_use]
pub fn standard_file_path_with(env: &dyn EnvVars) -> Option<PathBuf> {
    if let Some(p) = env.get("DEGENBOT_CONFIG").filter(|s| !s.is_empty()) {
        return Some(p.into());
    }
    let base = crate::resolvers::config_home(env)?;
    let path = base.join("degenbot").join("config.toml");
    path.is_file().then_some(path)
}

/// The canonical STANDARD config file path over the process environment
/// (the production surface): the `DEGENBOT_CONFIG` env override when set —
/// even when missing, the operator asked for it — else the XDG config home
/// (`$XDG_CONFIG_HOME/degenbot/config.toml`, absolute only) else
/// `$HOME/.config/degenbot/config.toml` when it exists, else `None` (an
/// absent user file is contractually defaults).
/// `BotConfigLoader::with_standard_file_paths` selects exactly this value,
/// and raw-table readers resolve the SAME file through this function so
/// file discovery stays a single contract. The std env reads live in THIS
/// crate so they stay confined to degenbot-config.
#[must_use]
pub fn standard_file_path() -> Option<PathBuf> {
    standard_file_path_with(&crate::ProcessEnv)
}

/// This process's own layers, loaded once: the [`standard_file_path`] file
/// layer (`DEGENBOT_CONFIG` else the XDG/HOME config file when it exists) over
/// the process environment. A consumer with no CLI to thread (the strategy
/// driver's node join, a Python-hosted boot) loads through this so its
/// resolvers see the same file and environment a console command would.
///
/// # Errors
///
/// The loader's fail-closed [`ConfigError`]: a config file the operator named
/// that is unreadable or unparsable, an unknown key, or an invalid value.
pub fn load_process_config() -> Result<LoadedConfig, ConfigError> {
    BotConfigLoader::new().with_standard_file_paths().load()
}

impl<'a> BotConfigLoader<'a> {
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
    /// else the XDG config home (absolute `$XDG_CONFIG_HOME` else
    /// `$HOME/.config`) `/degenbot/config.toml` when it exists, else no file
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
    pub fn with_env(mut self, env: Box<dyn EnvVars + 'a>) -> Self {
        self.env = Some(env);
        self
    }

    /// Replace the environment source with a borrowed seam (an embedding
    /// host that already holds an `&dyn EnvVars` — the CLI context).
    #[must_use]
    pub fn with_env_ref(self, env: &'a dyn EnvVars) -> Self {
        self.with_env(Box::new(EnvRef(env)))
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
    /// unknown, a value does not parse into the declared kind, a family-shaped
    /// env name does not end in a chain id, or a CLI override names an
    /// undeclared key.
    pub fn load(&self) -> Result<LoadedConfig, ConfigError> {
        let mut state = LoadState::new();
        if let Some(path) = &self.file {
            state.apply_file(path);
        }
        if let Some(env) = &self.env {
            state.apply_env(env.as_ref());
        }
        state.apply_cli(&self.cli);
        state.finish()
    }
}

/// The mutable bookkeeping one load accumulates: the typed value, the
/// provenance every layer records, and the problems every layer reports.
/// Bundled into one receiver so the layer functions thread one value instead
/// of five borrowed fields, and so a new layer's bookkeeping is one field here
/// rather than a new parameter on every layer.
struct LoadState {
    /// The typed value under construction.
    config: BotConfig,
    /// Winning layer per declared key.
    provenance: BTreeMap<&'static str, Source>,
    /// Winning layer per entry of each map-kind key.
    entry_provenance: EntryProvenance,
    /// The file layer's rendered raw text per map-kind key. A family-shaped
    /// env layer MERGES its entries onto the file's table, so the merge needs
    /// the entries the file supplied, not only the ones env adds.
    file_raw: BTreeMap<&'static str, String>,
    /// Every problem found, in the order the layers found them.
    problems: Vec<String>,
}

impl LoadState {
    /// The declared defaults, an empty per-entry map for every map-kind key,
    /// and no problems. Every declared key starts at `Source::Default` so a
    /// key no layer touched still reports where its value came from.
    fn new() -> Self {
        Self {
            config: BotConfig::default(),
            provenance: SCHEMA.iter().map(|k| (k.env, Source::Default)).collect(),
            entry_provenance: SCHEMA
                .iter()
                .filter(|k| is_map_kind(k))
                .map(|k| (k.env, BTreeMap::new()))
                .collect(),
            file_raw: BTreeMap::new(),
            problems: Vec::new(),
        }
    }

    /// Assign one raw value for `key` from `source` and record the layer. A
    /// map-kind key takes the WHOLE table from one layer, so its entry
    /// provenance is exactly the entries the raw form names.
    fn assign(&mut self, key: &'static KeyDecl, raw: &str, source: Source) {
        match self.config.assign(key.section, key.field, raw) {
            Ok(()) => {
                self.provenance.insert(key.env, source);
                if is_map_kind(key) {
                    self.record_entries(key, entry_names(raw), source);
                }
            }
            Err(problem) => self.problems.push(problem),
        }
    }

    /// Record the per-entry layer of a map-kind key whose entries are the
    /// `names` one layer supplied.
    fn record_entries(&mut self, key: &'static KeyDecl, names: Vec<String>, source: Source) {
        self.entry_provenance.insert(
            key.env,
            names.into_iter().map(|name| (name, source)).collect(),
        );
    }

    /// Layer 2 (lowest override): the --config TOML file.
    fn apply_file(&mut self, path: &Path) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => {
                self.problems
                    .push(format!("--config {}: unreadable: {e}", path.display()));
                return;
            }
        };
        let value: toml::Table = match text.parse() {
            Ok(value) => value,
            Err(e) => {
                self.problems
                    .push(format!("--config {}: parse error: {e}", path.display()));
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
            // Layout vocabulary no key replaced: fail pointed, naming the
            // key that replaced it and the migration doc.
            if let Some((_, replacement)) = RETIRED_LAYOUT_ITEMS
                .iter()
                .find(|(name, _)| section.as_str() == *name)
            {
                self.problems.push(format!(
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
            let Some(section_table) = section_value.as_table() else {
                self.problems.push(format!(
                    "--config {}: section [{section}] must be a table",
                    path.display()
                ));
                continue;
            };
            // A section whose declared keys all live under nested facets
            // (e.g. `[strategy]` with no direct keys left) is a grouping
            // table: every entry must name a declared facet, else it stays
            // unknown. The per-field loop routes each facet table.
            if members.is_empty() {
                let only_facets = !section_table.is_empty()
                    && section_table.keys().all(|field| {
                        SECTION_PATHS.contains(&format!("{section}.{field}").as_str())
                    });
                if !only_facets {
                    self.problems.push(format!(
                        "--config {}: unknown section [{section}]",
                        path.display()
                    ));
                    continue;
                }
            }
            for (field, field_value) in section_table {
                let Some(key) = members.iter().copied().find(|k| k.field == field.as_str()) else {
                    // A nested facet section (e.g. `[strategy.settlement]`):
                    // its own keys are declared under the dotted section path,
                    // and an empty facet is valid.
                    let nested = format!("{section}.{field}");
                    if crate::schema::SECTION_PATHS.contains(&nested.as_str()) {
                        self.apply_nested_facet(&nested, field_value, path);
                    } else {
                        self.problems.push(format!(
                            "--config {}: unknown key {field} in section [{section}]",
                            path.display()
                        ));
                    }
                    continue;
                };
                // Map-kind keys accept the nested table form (the primary
                // `[telemetry.diag]` and `[nodes.http]` surfaces) and the flat
                // string form.
                let raw = if is_map_kind(key) {
                    map_value_to_raw(field_value, key, path, &mut self.problems)
                } else {
                    toml_value_to_raw(field_value, key.toml_path, path, &mut self.problems)
                };
                let Some(raw) = raw else {
                    continue;
                };
                if let Err(problem) = self.config.assign(key.section, key.field, &raw) {
                    self.problems.push(problem);
                    continue;
                }
                self.provenance.insert(key.env, Source::File);
                if is_map_kind(key) {
                    // Kept so a later layer can merge onto the file's entries.
                    let names = entry_names(&raw);
                    self.file_raw.insert(key.env, raw);
                    self.record_entries(key, names, Source::File);
                }
            }
        }
    }

    /// Resolve a known nested facet section (`[strategy.mevblocker_backrun]`). Its
    /// members are either deeper facet paths or declared leaf keys under the
    /// dotted section path; an empty table is valid (the settlement facet
    /// declares no keys yet).
    fn apply_nested_facet(&mut self, section: &str, value: &toml::Value, path: &Path) {
        let Some(table) = value.as_table() else {
            self.problems.push(format!(
                "--config {}: section [{section}] must be a table",
                path.display()
            ));
            return;
        };
        for (field, field_value) in table {
            let nested = format!("{section}.{field}");
            if crate::schema::SECTION_PATHS.contains(&nested.as_str()) {
                self.apply_nested_facet(&nested, field_value, path);
                continue;
            }
            let Some(key) = SCHEMA.iter().find(|k| k.toml_path == nested) else {
                self.problems.push(format!(
                    "--config {}: unknown key {field} in section [{section}]",
                    path.display()
                ));
                continue;
            };
            let raw = if is_map_kind(key) {
                map_value_to_raw(field_value, key, path, &mut self.problems)
            } else {
                toml_value_to_raw(field_value, key.toml_path, path, &mut self.problems)
            };
            let Some(raw) = raw else {
                continue;
            };
            if let Err(problem) = self.config.assign(key.section, key.field, &raw) {
                self.problems.push(problem);
                continue;
            }
            self.provenance.insert(key.env, Source::File);
            if is_map_kind(key) {
                let names = entry_names(&raw);
                self.file_raw.insert(key.env, raw);
                self.record_entries(key, names, Source::File);
            }
        }
    }

    /// Layer 3: the environment. Iterates the SCHEMA (not the process env) so
    /// foreign DEGENBOT_*-prefixed vars never leak into the typed tree.
    fn apply_env(&mut self, env: &dyn EnvVars) {
        for key in SCHEMA {
            // A family-shaped key is one env name PER entry, so it is read by
            // enumeration rather than by a lookup.
            if let Some(prefix) = key.env_prefix {
                self.apply_env_family(env, key, prefix);
                continue;
            }
            if let Some(raw) = env.get(key.env) {
                self.assign(key, &raw, Source::Env);
            }
        }
    }

    /// The env layer of a family-shaped key: one name per operator-chosen
    /// entry (`PREFIX_<chain_id>=<value>`). Each name MERGES onto the entry
    /// the file supplied for that chain, so a family export overrides exactly
    /// the chain it names and leaves the rest of the table alone.
    fn apply_env_family(&mut self, env: &dyn EnvVars, key: &'static KeyDecl, prefix: &str) {
        let mut merged: BTreeMap<String, String> = self
            .file_raw
            .get(key.env)
            .and_then(|raw| crate::parse_string_map(raw).ok())
            .unwrap_or_default();
        let mut supplied: Vec<String> = Vec::new();
        for name in env.names_with_prefix(prefix) {
            let Some(chain) = name.strip_prefix(prefix) else {
                continue;
            };
            // The suffix is the chain id the entry is FOR. A name that does
            // not end in one is a typo, and a typo that became a table key
            // would only surface as a parse failure in a distant resolver.
            if chain.parse::<u64>().is_err() {
                self.problems.push(format!(
                    "{name} is not a {prefix}<chain_id> name: the suffix after the \
                     prefix is the decimal chain id the entry is for \u{2014} expected \
                     {prefix}<chain_id> (e.g. {prefix}1)"
                ));
                continue;
            }
            // A blank value means "this layer supplied nothing": it must not
            // win the cascade slot and then surface as a malformed endpoint.
            let raw_value = env.get(&name);
            let Some(value) = crate::resolvers::non_empty(raw_value.as_deref()) else {
                continue;
            };
            merged.insert(chain.to_string(), value.to_string());
            supplied.push(chain.to_string());
        }
        if supplied.is_empty() {
            return;
        }
        let raw = render_string_map(&merged);
        if let Err(problem) = self.config.assign(key.section, key.field, &raw) {
            self.problems.push(problem);
            return;
        }
        self.provenance.insert(key.env, Source::Env);
        let entries = self.entry_provenance.entry(key.env).or_default();
        for chain in supplied {
            entries.insert(chain, Source::Env);
        }
    }

    /// Layer 4 (highest): CLI / explicit argument overrides, keyed by env
    /// name or dotted TOML path.
    fn apply_cli(&mut self, cli: &[(String, String)]) {
        for (name, value) in cli {
            match resolve_key(name) {
                Some(key) => self.assign(key, value, Source::Cli),
                None => self.problems.push(format!(
                    "cli override {name:?} does not name a schema key (env name or TOML path required)"
                )),
            }
        }
    }

    /// Close the load: fold the per-entry layers into the aggregate key
    /// provenance, run semantic validation, and hand back either the config
    /// or every problem found.
    fn finish(mut self) -> Result<LoadedConfig, ConfigError> {
        // `Source` orders as the layers rank (default < file < env < cli), so
        // the highest entry is the highest-ranked layer that contributed
        // anything. A family export over one chain of a file table therefore
        // reports the key as env-sourced rather than file-sourced.
        let aggregates: Vec<(&'static str, Source)> = self
            .entry_provenance
            .iter()
            .filter_map(|(env, entries)| entries.values().copied().max().map(|top| (*env, top)))
            .collect();
        for (env, source) in aggregates {
            self.provenance.insert(env, source);
        }

        // Semantic validation: a value that parses into its declared kind
        // but cannot serve its domain fails the load here, with the remedy in
        // the message, rather than at a distant call site.
        if let Err(error) = self.config.validate() {
            self.problems.extend(error.problems);
        }

        if self.problems.is_empty() {
            Ok(LoadedConfig {
                config: self.config,
                provenance: self.provenance,
                entry_provenance: self.entry_provenance,
            })
        } else {
            Err(ConfigError::of(self.problems))
        }
    }
}

/// Resolve a CLI override key: exact env name first, then TOML path.
fn resolve_key(name: &str) -> Option<&'static crate::schema::KeyDecl> {
    SCHEMA.iter().find(|k| k.env == name || k.toml_path == name)
}

/// Whether a declared key's typed value is a table of operator-chosen
/// entries (a per-chain endpoint table, a per-domain level map). Such a key
/// keeps provenance PER ENTRY, because a family-shaped env layer overrides one
/// entry at a time.
fn is_map_kind(key: &crate::schema::KeyDecl) -> bool {
    matches!(
        key.kind.base,
        crate::schema::BaseKind::Map(_) | crate::schema::BaseKind::StrMap
    )
}

/// Render a table as the comma-separated `key=value` raw form every map-kind
/// parse consumes. The single encoding of these tables: the file layer, the
/// env-family merge, and the entry bookkeeping all speak it.
fn render_string_map(table: &BTreeMap<String, String>) -> String {
    table
        .iter()
        .map(|(entry, value)| format!("{entry}={value}"))
        .collect::<Vec<String>>()
        .join(",")
}

/// The entry names a rendered map raw form carries, for per-entry provenance.
/// Recovered at the ENCODING level (both map kinds share the comma-separated
/// `key=value` form), so a malformed raw is not reported twice: the layer's own
/// assign reports it.
fn entry_names(raw: &str) -> Vec<String> {
    crate::parse_string_map(raw).map_or_else(|_| Vec::new(), |table| table.into_keys().collect())
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
    let mut table: BTreeMap<String, String> = BTreeMap::new();
    let mut bad = false;
    for (k, v) in t {
        let Some(entry) = v.as_str() else {
            problems.push(format!(
                "--config {}: {}.{k} must be {}",
                path.display(),
                key.toml_path,
                map_entry_expectation(key.kind.base)
            ));
            bad = true;
            continue;
        };
        table.insert(k.clone(), entry.to_string());
    }
    if bad {
        return None;
    }
    Some(render_string_map(&table))
}

/// What a map-kind entry's TOML value must be, spelled for the refusal. The
/// level map carries a typed enum, so its entries are levels; a string map's
/// entries are plain text.
fn map_entry_expectation(base: crate::schema::BaseKind) -> &'static str {
    match base {
        crate::schema::BaseKind::Map(_) => "a string level",
        _ => "a string",
    }
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
