//! The `config` command arms: the read-only view of the operator file.
//!
//! The console resolves its driver-domain values through the four-layer
//! cascade, so "which value applies, and who supplied it" is the question an
//! operator actually has. These arms answer it without ever writing:
//!
//! - `config show` lists what the operator FILE declares — the view an editor
//!   and the mutating arms share, with no layer column.
//! - `config show --resolved` lists the same keys as the process will resolve
//!   them, each annotated with its winning [`Source`]; a key no layer supplied
//!   is reported as unresolved rather than omitted, so the listing is a
//!   complete inventory of the driver-domain surface.
//! - `config path` prints the one file the mutating arms write (the
//!   [`crate::context::CliContext::resolve_config_file`] contract).
//!
//! The endpoint tables are per-chain, so a table entry renders as
//! `nodes.ws[8453] = …`; an explicit `--node` fills a whole transport's slot
//! for the session chain and renders unindexed as `nodes.ws = … (cli)`.

use std::collections::BTreeMap;

use degenbot_config::writer::{remove_entry, remove_key, write_entry_with_env, write_key_with_env};
use degenbot_config::{BaseKind, KeyDecl, LoadedConfig, NodeTransport, Source, SCHEMA};

use crate::context::CliContext;
use crate::error::CliError;
use crate::prompt::{PromptPlan, Prompter};
use crate::report::{ConfigReport, ConfigValue};
use crate::strategy::MutationOutcome;

/// The `config` command group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigCommand {
    /// `config show`: the driver-domain values. `--resolved` renders every
    /// key with the layer that supplied it; without it the report is the file
    /// layer alone — what the operator wrote.
    Show {
        /// Whether to render the winning layer per key.
        resolved: bool,
    },
    /// `config path`: the config file the mutating arms read and write.
    Path,
    /// `config get <key>`: the resolved value of one declared key (or one
    /// `str_map` entry), with the layer that supplied it.
    Get {
        /// The dotted key or `table.entry` path.
        key: String,
    },
    /// `config set <key> <value>`: write one declared key (or one `str_map`
    /// entry) through the degenbot-config writer.
    Set {
        /// The dotted key or `table.entry` path.
        key: String,
        /// The raw value.
        value: String,
        /// Skip the confirmation prompt.
        force: bool,
    },
    /// `config unset <key>`: remove one declared key's override (or one
    /// `str_map` entry) so the declared default applies again.
    Unset {
        /// The dotted key or `table.entry` path.
        key: String,
        /// Skip the confirmation prompt.
        force: bool,
    },
}

impl ConfigCommand {
    /// Read arms never prompt; a mutating arm confirms unless `--force`
    /// (ADR-051 D4).
    #[must_use]
    pub const fn prompt_plan(&self, _ctx: &CliContext<'_>) -> PromptPlan {
        match self {
            Self::Set { .. } | Self::Unset { .. } => PromptPlan::UnlessForce,
            Self::Show { .. } | Self::Path | Self::Get { .. } => PromptPlan::None,
        }
    }
}

/// Execute a `config` command.
///
/// # Errors
///
/// [`CliError::Config`] when the context's config layers do not load (a file
/// the operator named that is unreadable, unparsable, or holds an invalid
/// value), or [`CliError::InvalidArgument`] when no config file location
/// resolves for `config path`.
pub(crate) fn execute(
    command: ConfigCommand,
    ctx: &CliContext<'_>,
    prompter: &dyn Prompter,
) -> Result<ConfigReport, CliError> {
    match command {
        ConfigCommand::Path => Ok(ConfigReport::Path(ctx.resolve_config_file()?)),
        ConfigCommand::Show { resolved } => show(ctx, resolved),
        ConfigCommand::Get { key } => get(ctx, &key),
        ConfigCommand::Set { key, value, force } => set(ctx, prompter, &key, &value, force),
        ConfigCommand::Unset { key, force } => unset(ctx, prompter, &key, force),
    }
}

/// A key the config verb can address: one declared scalar key, or one entry
/// of an operator-keyed `StrMap` table (spelled `table.entry`).
enum ConfigTarget {
    /// A whole declared key (`session.chain_id`, `nodes.http`).
    Scalar(&'static KeyDecl),
    /// One entry of an operator-keyed table (`nodes.http.1`).
    Entry(&'static KeyDecl, String),
}

/// `config get`: read the value out of the very inventory `config show`
/// renders, so the two arms cannot disagree about a value or its layer.
fn get(ctx: &CliContext<'_>, key: &str) -> Result<ConfigReport, CliError> {
    let wanted = inventory_key(key);
    let ConfigReport::Shown { values, .. } = show(ctx, true)? else {
        return Err(CliError::InvalidArgument(format!(
            "{key:?} is not a driver-domain config value"
        )));
    };
    let value = values
        .into_iter()
        .find(|value| value.key == wanted)
        .ok_or_else(|| {
            CliError::InvalidArgument(format!(
                "{key:?} does not name a driver-domain config key: try database.path, \
                 session.chain_id, or a nodes.<transport>[.<chain>] entry"
            ))
        })?;
    Ok(ConfigReport::Got {
        key: key.to_string(),
        value: value.value,
        source: value.source,
    })
}

/// Translate an operator-spelled entry path (`nodes.http.1`) into the
/// inventory's bracketed spelling (`nodes.http[1]`); every other key passes
/// through unchanged.
fn inventory_key(key: &str) -> String {
    if let Some((table, entry)) = key.rsplit_once('.') {
        if SCHEMA
            .iter()
            .any(|decl| decl.toml_path == table && matches!(decl.kind.base, BaseKind::StrMap))
        {
            return format!("{table}[{entry}]");
        }
    }
    key.to_string()
}

/// `config set`: confirm (unless forced) then write through the single
/// validate-before-write path.
fn set(
    ctx: &CliContext<'_>,
    prompter: &dyn Prompter,
    key: &str,
    value: &str,
    force: bool,
) -> Result<ConfigReport, CliError> {
    let file = ctx.resolve_config_file()?;
    confirm_mutation(
        prompter,
        force,
        &format!(
            "Write {key} = {} to {}?",
            degenbot_config::redact_uri(value),
            file.display()
        ),
    )?;
    let target = resolve_target(key)?;
    let outcome = match target {
        ConfigTarget::Scalar(decl) => write_key_with_env(&file, decl, value, ctx.env()),
        ConfigTarget::Entry(decl, entry) => {
            write_entry_with_env(&file, decl, &entry, value, ctx.env())
        }
    }
    .map_err(CliError::Config)?;
    Ok(ConfigReport::Set {
        key: key.to_string(),
        value: value.to_string(),
        outcome: outcome.into(),
    })
}

/// `config unset`: confirm (unless forced) then drop the override so the
/// declared default applies again.
fn unset(
    ctx: &CliContext<'_>,
    prompter: &dyn Prompter,
    key: &str,
    force: bool,
) -> Result<ConfigReport, CliError> {
    let file = ctx.resolve_config_file()?;
    confirm_mutation(
        prompter,
        force,
        &format!("Remove {key} from {}?", file.display()),
    )?;
    let target = resolve_target(key)?;
    let outcome = match target {
        ConfigTarget::Scalar(decl) => {
            remove_key(&file, decl).map_err(CliError::Config)?;
            shadow_outcome(ctx, decl.env)
        }
        ConfigTarget::Entry(decl, entry) => {
            remove_entry(&file, decl, &entry).map_err(CliError::Config)?;
            shadow_outcome(ctx, &format!("{}{entry}", decl.env))
        }
    };
    Ok(ConfigReport::Unset {
        key: key.to_string(),
        outcome,
    })
}

/// Resolve one operator-spelled key into a declared key or a `StrMap` entry.
fn resolve_target(path: &str) -> Result<ConfigTarget, CliError> {
    if let Some(decl) = SCHEMA.iter().find(|decl| decl.toml_path == path) {
        return Ok(ConfigTarget::Scalar(decl));
    }
    if let Some((table, entry)) = path.rsplit_once('.') {
        if let Some(decl) = SCHEMA
            .iter()
            .find(|decl| decl.toml_path == table && matches!(decl.kind.base, BaseKind::StrMap))
        {
            if entry.is_empty() {
                return Err(CliError::InvalidArgument(format!(
                    "{path:?} names an empty table entry"
                )));
            }
            return Ok(ConfigTarget::Entry(decl, entry.to_string()));
        }
    }
    Err(CliError::InvalidArgument(format!(
        "{path:?} does not name a declared config key: use a dotted key \
         (session.chain_id) or a str-map entry path (nodes.http.1)"
    )))
}

/// Ask before mutating unless `--force` was given.
fn confirm_mutation(prompter: &dyn Prompter, force: bool, message: &str) -> Result<(), CliError> {
    if force || prompter.confirm(message, false) {
        Ok(())
    } else {
        Err(CliError::Aborted)
    }
}

/// Whether the env layer still supplies `env_name` after a removal: the
/// environment wins at load time even though the file override is gone, which
/// the report must say rather than let the operator believe the default took
/// over.
fn shadow_outcome(ctx: &CliContext<'_>, env_name: &str) -> MutationOutcome {
    if ctx
        .env()
        .get(env_name)
        .is_some_and(|value| !value.is_empty())
    {
        MutationOutcome::Shadowed {
            env: env_name.to_string(),
        }
    } else {
        MutationOutcome::Applied
    }
}

/// The `show` arm over the context's ONE loaded config, so every line reports
/// the same load the resolvers read.
fn show(ctx: &CliContext<'_>, resolved: bool) -> Result<ConfigReport, CliError> {
    let loaded = ctx.loaded_config()?;
    let mut values = Vec::new();
    database_path(ctx, resolved, &mut values);
    session_chain_id(ctx, resolved, &mut values);
    for transport in NodeTransport::ALL {
        nodes(ctx, loaded, transport, resolved, &mut values);
    }
    Ok(ConfigReport::Shown {
        file: ctx.resolve_config_file().ok(),
        values,
        resolved,
    })
}

/// `database.path`: `--database` > `DEGENBOT_DB_PATH` > the declared key >
/// the state-home default, so the line is never absent in the resolved view.
fn database_path(ctx: &CliContext<'_>, resolved: bool, out: &mut Vec<ConfigValue>) {
    let path = ctx.database_path();
    let Ok(database) = path else {
        // The default is declared, so this arm cannot lose it; an unresolvable
        // layers set is reported the same way as any other absent value.
        if resolved {
            out.push(ConfigValue::unresolved("database.path"));
        }
        return;
    };
    if resolved || database.source == Source::File {
        out.push(ConfigValue::new(
            "database.path",
            database.value.display().to_string(),
            database.source,
        ));
    }
}

/// `session.chain_id`: the one driver-domain key with no default, so its
/// absence is the common misconfiguration and the listing says so.
fn session_chain_id(ctx: &CliContext<'_>, resolved: bool, out: &mut Vec<ConfigValue>) {
    match ctx.chain_id() {
        Ok(chain) => {
            if resolved || chain.source == Source::File {
                out.push(ConfigValue::new(
                    "session.chain_id",
                    chain.value.to_string(),
                    chain.source,
                ));
            }
        }
        Err(_) if resolved => out.push(ConfigValue::unresolved("session.chain_id")),
        Err(_) => {}
    }
}

/// One transport's endpoint surface: the explicit slot, then the loaded
/// per-chain entries, each keeping the layer that supplied it.
fn nodes(
    ctx: &CliContext<'_>,
    loaded: &LoadedConfig,
    transport: NodeTransport,
    resolved: bool,
    out: &mut Vec<ConfigValue>,
) {
    let mut present = false;
    if let Some(uri) = ctx.node_overrides().get(transport) {
        present = true;
        out.push(ConfigValue::new(
            transport.key_path(),
            uri.to_string(),
            Source::Cli,
        ));
    }
    for (chain, uri) in table(loaded, transport) {
        let source = loaded
            .entry_source_of(transport.env_prefix(), chain)
            .unwrap_or(Source::File);
        if !resolved && source != Source::File {
            continue;
        }
        present = true;
        out.push(ConfigValue::new(
            &format!("{}[{chain}]", transport.key_path()),
            uri.clone(),
            source,
        ));
    }
    if resolved && !present {
        out.push(ConfigValue::unresolved(transport.key_path()));
    }
}

/// The loaded entries of one transport's endpoint table.
fn table(loaded: &LoadedConfig, transport: NodeTransport) -> &BTreeMap<String, String> {
    static EMPTY: std::sync::OnceLock<BTreeMap<String, String>> = std::sync::OnceLock::new();
    match transport {
        NodeTransport::Http => loaded.config.nodes.http.as_ref(),
        NodeTransport::Ws => loaded.config.nodes.ws.as_ref(),
        NodeTransport::Ipc => loaded.config.nodes.ipc.as_ref(),
    }
    .unwrap_or_else(|| EMPTY.get_or_init(BTreeMap::new))
}
