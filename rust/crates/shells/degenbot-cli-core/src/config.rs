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

use degenbot_config::{LoadedConfig, NodeTransport, Source};

use crate::context::CliContext;
use crate::error::CliError;
use crate::prompt::PromptPlan;
use crate::report::{ConfigReport, ConfigValue};

/// The `config` command group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}

impl ConfigCommand {
    /// No config arm prompts: both are read-only reports.
    #[must_use]
    pub const fn prompt_plan(&self, _ctx: &CliContext<'_>) -> PromptPlan {
        PromptPlan::None
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
) -> Result<ConfigReport, CliError> {
    match command {
        ConfigCommand::Path => Ok(ConfigReport::Path(ctx.resolve_config_file()?)),
        ConfigCommand::Show { resolved } => show(ctx, resolved),
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
