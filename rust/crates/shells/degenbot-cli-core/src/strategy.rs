//! The `strategy` command arms: the console's typed view AND write path over
//! the strategy facets.
//!
//! A strategy **facet** is one typed per-strategy config section
//! (`strategy.settlement`, `strategy.mevblocker_backrun`, `strategy.txpool_backrun`); each facet's `active` key
//! selects it (the retired single-arm selector could not express a
//! two-strategy host). Activation settles an endpoint posture: EITHER an
//! explicit `--endpoints` set OR the explicit `--endpoints-default` choice,
//! and the typed readiness validation is the single authority for what
//! counts as settled.
//!
//! All config mutation flows through the `degenbot-config` writer (nothing
//! else in the workspace edits a config.toml programmatically), and every
//! write is mirrored through the loader so a shadowing env var is reported
//! rather than silently masking the file.

use std::path::Path;

use degenbot_config::readiness::Arm;
use degenbot_config::writer::{write_key_with_env, WriteOutcome};
use degenbot_config::{strategy_readiness, SCHEMA};

use crate::context::CliContext;
use crate::error::CliError;
use crate::prompt::PromptPlan;
use crate::report::StrategyReport;

/// One strategy arm — the per-ecosystem selection surface. Settlement plus the
/// two per-ecosystem pending-transaction backruns (ADR-055 D1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrategyFacet {
    /// The settled-block strategy (settlement arbitrage).
    Settlement,
    /// The MEVBlocker-ecosystem pending-transaction strategy.
    MevblockerBackrun,
    /// The public-mempool pending-transaction strategy.
    TxpoolBackrun,
}

impl StrategyFacet {
    /// Every facet, in declaration order.
    pub const ALL: [Self; 3] = [
        Self::Settlement,
        Self::MevblockerBackrun,
        Self::TxpoolBackrun,
    ];

    /// The canonical lowercase spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Settlement => "settlement",
            Self::MevblockerBackrun => "mevblocker_backrun",
            Self::TxpoolBackrun => "txpool_backrun",
        }
    }

    /// The reaction kind this facet implements (ADR-055 D1).
    #[must_use]
    pub const fn trigger_kind(self) -> &'static str {
        match self {
            Self::Settlement => "settled-block",
            Self::MevblockerBackrun | Self::TxpoolBackrun => "pending-transaction",
        }
    }

    /// The facet's dotted TOML section path.
    #[must_use]
    pub const fn config_section(self) -> &'static str {
        match self {
            Self::Settlement => "strategy.settlement",
            Self::MevblockerBackrun => "strategy.mevblocker_backrun",
            Self::TxpoolBackrun => "strategy.txpool_backrun",
        }
    }

    /// The facet's activation key.
    #[must_use]
    pub fn activation_key(self) -> &'static degenbot_config::KeyDecl {
        facet_key(self.config_section(), "active")
    }

    /// The facet's explicit-endpoints key.
    #[must_use]
    pub fn endpoints_key(self) -> &'static degenbot_config::KeyDecl {
        facet_key(self.config_section(), "endpoints")
    }

    /// The pinned default endpoint set `--endpoints-default` stamps into
    /// the facet's `endpoints` list at activation time.
    #[must_use]
    pub fn default_endpoint_set(self) -> Vec<String> {
        let urls: &[&str] = match self {
            Self::Settlement => degenbot_config::SETTLEMENT_DEFAULT_ENDPOINTS,
            Self::MevblockerBackrun => {
                std::slice::from_ref(&degenbot_config::DEFAULT_BACKRUN_STREAM_URL)
            }
            Self::TxpoolBackrun => degenbot_config::DEFAULT_TXPOOL_BACKRUN_RELAYS,
        };
        urls.iter().map(|url| (*url).to_string()).collect()
    }

    /// Parse a facet selector case-insensitively.
    ///
    /// # Errors
    ///
    /// [`CliError::InvalidArgument`] naming the accepted spellings.
    pub fn parse(raw: &str) -> Result<Self, CliError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "settlement" => Ok(Self::Settlement),
            "mevblocker_backrun" => Ok(Self::MevblockerBackrun),
            "txpool_backrun" => Ok(Self::TxpoolBackrun),
            other => Err(CliError::InvalidArgument(format!(
                "unknown strategy facet {other:?} (expected one of: settlement \
                 mevblocker_backrun txpool_backrun)"
            ))),
        }
    }
}

/// Lookup a schema key by dotted section + field.
///
/// # Panics
///
/// Panics when the facet's declared section carries no such field; callers
/// pair a facet's own section with one of its declared fields.
#[expect(clippy::panic)] // both the section and the field are schema-registry facts
fn facet_key(section: &str, field: &str) -> &'static degenbot_config::KeyDecl {
    SCHEMA
        .iter()
        .find(|k| k.section == section && k.field == field)
        .unwrap_or_else(|| panic!("{section}.{field} must be declared"))
}

/// A console descriptor entry for one strategy facet: the typed data the
/// `list`/`show` arms render. The declared keys come straight from `SCHEMA`
/// (the one declaration site), so the descriptor cannot drift from the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrategyFacetDescriptor {
    /// The facet this entry describes.
    pub facet: StrategyFacet,
    /// The facet's dotted TOML section path.
    pub config_section: &'static str,
    /// The facet's reaction kind.
    pub trigger_kind: &'static str,
    /// The declared config fields under this facet, sourced from the schema
    /// registry.
    pub fields: Vec<&'static str>,
    /// The declared `DEGENBOT_*` env names under this facet, sourced from the
    /// schema registry.
    pub envs: Vec<&'static str>,
}

/// Build the descriptor for one facet from the schema registry.
#[must_use]
pub fn descriptor(facet: StrategyFacet) -> StrategyFacetDescriptor {
    let section = facet.config_section();
    let mut fields = Vec::new();
    let mut envs = Vec::new();
    for key in SCHEMA {
        if key.section == section {
            fields.push(key.field);
            envs.push(key.env);
        }
    }
    StrategyFacetDescriptor {
        facet,
        config_section: section,
        trigger_kind: facet.trigger_kind(),
        fields,
        envs,
    }
}

/// Build the descriptor for every facet, in declaration order.
#[must_use]
pub fn descriptors() -> Vec<StrategyFacetDescriptor> {
    StrategyFacet::ALL.into_iter().map(descriptor).collect()
}

/// The endpoint posture one facet renders for `show`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointSummary {
    /// The facet is dormant.
    Inactive,
    /// The facet is active but no endpoint choice is recorded.
    Unset,
    /// The explicit default choice, resolved.
    Default(Vec<String>),
    /// The operator's endpoint set, resolved.
    Explicit(Vec<String>),
}

/// The `strategy` command group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrategyCommand {
    /// `strategy list`: every declared facet with its section and trigger kind.
    List,
    /// `strategy show <facet>`: one facet's descriptor, activation, and
    /// resolved endpoint posture.
    Show {
        /// The facet to show.
        facet: StrategyFacet,
    },
    /// `strategy activate <facet>`: activate the facet and settle its endpoint
    /// posture (`--endpoints <csv>` or `--endpoints-default`, exactly one;
    /// neither is allowed only when the facet already carries a settled
    /// choice).
    Activate {
        /// The facet to activate.
        facet: StrategyFacet,
        /// The explicit endpoint set (comma-separated).
        endpoints: Option<String>,
        /// Adopt the pinned default endpoint set as the facet's endpoints.
        endpoints_default: bool,
    },
    /// `strategy deactivate <facet>`: deactivate the facet; its recorded
    /// endpoint choice stays put for a later re-activation.
    Deactivate {
        /// The facet to deactivate.
        facet: StrategyFacet,
    },
    /// `strategy set <facet> <key> <value>`: write one declared facet key.
    Set {
        /// The facet to mutate.
        facet: StrategyFacet,
        /// The config key name.
        key: String,
        /// The raw value.
        value: String,
    },
    /// `strategy default <facet> <key>`: drop one key's override so the
    /// declared schema default applies again.
    Default {
        /// The facet to mutate.
        facet: StrategyFacet,
        /// The config key name.
        key: String,
    },
    /// `strategy remove <facet> <key>`: the `default` verb's traditional
    /// spelling (`strategy default` reads clearer; both behave identically).
    Remove {
        /// The facet to mutate.
        facet: StrategyFacet,
        /// The config key name.
        key: String,
    },
}

impl StrategyCommand {
    /// No strategy arm prompts: writes are config-only and reversible.
    #[must_use]
    pub const fn prompt_plan(&self, _ctx: &CliContext<'_>) -> PromptPlan {
        PromptPlan::None
    }
}

/// The write outcome a mutation renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationOutcome {
    /// The write took effect.
    Applied,
    /// The write landed in the file but the env layer will shadow it at load
    /// time (the rendered line says so loudly).
    Shadowed {
        /// The env var name holding the shadowing value.
        env: &'static str,
    },
}

impl From<WriteOutcome> for MutationOutcome {
    fn from(outcome: WriteOutcome) -> Self {
        match outcome {
            WriteOutcome::Written => Self::Applied,
            WriteOutcome::Shadowed { env } => Self::Shadowed { env },
        }
    }
}

/// Execute a `strategy` command.
///
/// # Errors
///
/// [`CliError`] for an unsettled activation, an off-allowlist endpoint, an
/// unknown or facet-foreign key, or a config read/write failure.
pub(crate) fn execute(
    command: &StrategyCommand,
    ctx: &CliContext<'_>,
    _prompter: &dyn crate::prompt::Prompter,
) -> Result<StrategyReport, CliError> {
    match command {
        StrategyCommand::List => Ok(StrategyReport::Listed {
            rows: descriptors(),
        }),
        StrategyCommand::Show { facet } => {
            let file = ctx.resolve_config_file()?;
            let loaded = load(&file, ctx)?;
            let readiness = strategy_readiness(&loaded.config);
            let summary = endpoint_summary(*facet, &loaded.config, &readiness);
            Ok(StrategyReport::Shown {
                descriptor: descriptor(*facet),
                activation: Some(summary),
            })
        }
        StrategyCommand::Activate {
            facet,
            endpoints,
            endpoints_default,
        } => {
            let file = ctx.resolve_config_file()?;
            activate(*facet, endpoints.as_ref(), *endpoints_default, &file, ctx)
        }
        StrategyCommand::Deactivate { facet } => {
            let file = ctx.resolve_config_file()?;
            let outcome = write_key_with_env(&file, facet.activation_key(), "false", ctx.env())
                .map_err(|error| strategy_write_error(&error))?
                .into();
            Ok(StrategyReport::Deactivated {
                facet: *facet,
                outcome,
            })
        }
        StrategyCommand::Set { facet, key, value } => {
            let file = ctx.resolve_config_file()?;
            let key = facet_lookup(*facet, key)?;
            let outcome = write_key_with_env(&file, key, value, ctx.env())
                .map_err(|error| strategy_write_error(&error))?
                .into();
            Ok(StrategyReport::Set {
                facet: *facet,
                key: key.field,
                outcome,
            })
        }
        StrategyCommand::Default { facet, key } | StrategyCommand::Remove { facet, key } => {
            let file = ctx.resolve_config_file()?;
            let key = facet_lookup(*facet, key)?;
            let loaded = load(&file, ctx)?;
            if loaded.source_of(key.env) != Some(degenbot_config::loader::Source::File) {
                return Err(CliError::InvalidArgument(format!(
                    "{} has no file override to remove (its present value is default or env)",
                    key.toml_path
                )));
            }
            degenbot_config::writer::remove_key(&file, key)
                .map_err(|error| strategy_write_error(&error))?;
            Ok(StrategyReport::Defaulted {
                facet: *facet,
                key: key.field,
            })
        }
    }
}

/// Resolve one facet's endpoint summary for `show`.
fn endpoint_summary(
    facet: StrategyFacet,
    cfg: &degenbot_config::schema::BotConfig,
    readiness: &Result<degenbot_config::StrategyReadiness, degenbot_config::StrategyReadinessError>,
) -> EndpointSummary {
    let active = match facet {
        StrategyFacet::Settlement => cfg.strategy.settlement.active,
        StrategyFacet::MevblockerBackrun => cfg.strategy.mevblocker_backrun.active,
        StrategyFacet::TxpoolBackrun => cfg.strategy.txpool_backrun.active,
    };
    if !active {
        return EndpointSummary::Inactive;
    }
    match readiness {
        Ok(readiness) => {
            let arm = match facet {
                StrategyFacet::Settlement => &readiness.settlement,
                StrategyFacet::MevblockerBackrun => &readiness.mevblocker_backrun,
                StrategyFacet::TxpoolBackrun => &readiness.txpool_backrun,
            };
            match arm {
                Arm::Active(urls) => {
                    // Report provenance: an endpoints list that equals the
                    // pinned default set reads as "pinned default",
                    // anything else as the operator's own set.
                    if *urls == facet.default_endpoint_set() {
                        EndpointSummary::Default(urls.clone())
                    } else {
                        EndpointSummary::Explicit(urls.clone())
                    }
                }
                Arm::Inactive => EndpointSummary::Unset,
            }
        }
        Err(_) => EndpointSummary::Unset,
    }
}

/// Load the file-backed config through the standard loader (env included).
fn load(file: &Path, ctx: &CliContext<'_>) -> Result<degenbot_config::LoadedConfig, CliError> {
    ctx.load_bot_config_at(file)
}

/// The activate arm: settle the posture THEN set `active` (an activation can
/// never leave the config in a state the readiness gate would refuse).
fn activate(
    facet: StrategyFacet,
    endpoints: Option<&String>,
    endpoints_default: bool,
    file: &Path,
    ctx: &CliContext<'_>,
) -> Result<StrategyReport, CliError> {
    if endpoints.is_some() && endpoints_default {
        return Err(CliError::InvalidArgument(format!(
            "strategy {}: choose exactly one of --endpoints and --endpoints-default",
            facet.as_str()
        )));
    }
    // `--endpoints-default` resolves to the pinned set and takes the same
    // single write path: the persisted `endpoints` list IS the posture (no
    // denormalized marker key to keep in step with it).
    let stamped = endpoints_default.then(|| facet.default_endpoint_set().join(","));
    let chosen = endpoints.cloned().or(stamped);
    let loaded = load(file, ctx)?;
    let already_settled = facet_has_settled_choice(facet, &loaded.config);
    if chosen.is_none() && !already_settled {
        return Err(CliError::InvalidArgument(format!(
            "activating strategy {} requires an endpoint choice: pass exactly one of \
             --endpoints <urls> or --endpoints-default",
            facet.as_str()
        )));
    }

    // The write plan applied to an in-memory candidate first: readiness over
    // the candidate refuses an activation the boot would later reject (an
    // off-allowlist endpoint), so a refused command leaves the file untouched.
    let mut candidate = loaded.config.clone();
    if let Some(urls) = &chosen {
        candidate
            .assign(
                facet.endpoints_key().section,
                facet.endpoints_key().field,
                urls,
            )
            .map_err(CliError::InvalidArgument)?;
    }
    let activation = facet.activation_key();
    candidate
        .assign(activation.section, activation.field, "true")
        .map_err(CliError::InvalidArgument)?;
    let readiness = strategy_readiness(&candidate)
        .map_err(|error| CliError::InvalidArgument(error.to_string()))?;
    let arm = match facet {
        StrategyFacet::Settlement => &readiness.settlement,
        StrategyFacet::MevblockerBackrun => &readiness.mevblocker_backrun,
        StrategyFacet::TxpoolBackrun => &readiness.txpool_backrun,
    };
    let Arm::Active(urls) = arm else {
        return Err(CliError::InvalidArgument(format!(
            "strategy {} did not settle to an active arm: this is a CLI bug",
            facet.as_str()
        )));
    };

    // Only now touch the file, mirroring the validated plan.
    let mut outcome = MutationOutcome::Applied;
    if let Some(urls) = &chosen {
        outcome = write_key_with_env(file, facet.endpoints_key(), urls, ctx.env())
            .map_err(|error| strategy_write_error(&error))?
            .into();
    }
    let activation_outcome = write_key_with_env(file, facet.activation_key(), "true", ctx.env())
        .map_err(|error| strategy_write_error(&error))?
        .into();
    if matches!(outcome, MutationOutcome::Applied) {
        outcome = activation_outcome;
    }

    let summary = if endpoints_default {
        EndpointSummary::Default(urls.clone())
    } else {
        EndpointSummary::Explicit(urls.clone())
    };
    Ok(StrategyReport::Activated {
        facet,
        summary,
        outcome,
    })
}

/// Whether the loaded config already settles this facet's endpoint choice.
fn facet_has_settled_choice(
    facet: StrategyFacet,
    cfg: &degenbot_config::schema::BotConfig,
) -> bool {
    let endpoints = match facet {
        StrategyFacet::Settlement => cfg.strategy.settlement.endpoints.as_deref(),
        StrategyFacet::MevblockerBackrun => cfg.strategy.mevblocker_backrun.endpoints.as_deref(),
        StrategyFacet::TxpoolBackrun => cfg.strategy.txpool_backrun.endpoints.as_deref(),
    };
    endpoints.is_some_and(|raw| raw.split(',').any(|s| !s.trim().is_empty()))
}

/// Resolve a `set`/`default` key against the facet's declared section.
fn facet_lookup(
    facet: StrategyFacet,
    key: &str,
) -> Result<&'static degenbot_config::KeyDecl, CliError> {
    let matched = SCHEMA
        .iter()
        .find(|k| k.section == facet.config_section() && k.field == key);
    let Some(key) = matched else {
        let declared = descriptor(facet).fields.clone().join(", ");
        return Err(CliError::InvalidArgument(format!(
            "unknown key {key:?} on facet {} (declared: {declared})",
            facet.config_section()
        )));
    };
    // The activation, endpoint, and default keys carry CLI verbs of their own;
    // routing them through `set`/`default` bypasses the readiness gates.
    if matches!(key.field, "active" | "endpoints") {
        return Err(CliError::InvalidArgument(format!(
            "key {key:?} is owned by `strategy activate`; use the activate verb \
             (or --endpoints / --endpoints-default) instead"
        )));
    }
    Ok(key)
}

/// Surface a write refusal with its facet context.
fn strategy_write_error(error: &degenbot_config::ConfigError) -> CliError {
    CliError::InvalidArgument(error.to_string())
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "unit tests: a malformed fixture must fail the test loudly"
)]
mod tests {
    use super::*;
    use crate::report::StrategyReport;
    use degenbot_config::{BotConfigLoader, MapEnv, SECTION_PATHS};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    struct NoPrompt;
    impl crate::prompt::Prompter for NoPrompt {
        fn confirm(&self, _message: &str, default: bool) -> bool {
            default
        }
    }

    fn test_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("strategy-cli-{name}-{}", std::process::id()));
        let file = dir.join("config.toml");
        let _ = std::fs::remove_file(&file);
        file
    }

    fn empty_env() -> MapEnv {
        MapEnv::new(BTreeMap::new())
    }

    fn ctx_at<'e>(env: &'e MapEnv, file: &Path) -> CliContext<'e> {
        let path = file.to_string_lossy().into_owned();
        CliContext::new(env).with_config(path)
    }

    #[test]
    fn facets_name_their_typed_sections_and_trigger_kinds() {
        for descriptor in descriptors() {
            assert!(
                SECTION_PATHS.contains(&descriptor.config_section),
                "{} must be a declared schema section",
                descriptor.config_section
            );
        }
    }

    #[test]
    fn facet_descriptors_mirror_their_schema_keys() {
        for descriptor in descriptors() {
            let expected_fields: Vec<&str> = SCHEMA
                .iter()
                .filter(|key| key.section == descriptor.config_section)
                .map(|key| key.field)
                .collect();
            assert_eq!(descriptor.fields, expected_fields);
            let expected_envs: Vec<&str> = SCHEMA
                .iter()
                .filter(|key| key.section == descriptor.config_section)
                .map(|key| key.env)
                .collect();
            assert_eq!(descriptor.envs, expected_envs);
        }
        assert!(
            !descriptor(StrategyFacet::Settlement).fields.is_empty(),
            "strategy.settlement declares its keys"
        );
    }

    #[test]
    fn facet_selector_parses_case_insensitively_and_refuses_unknown() {
        assert_eq!(
            StrategyFacet::parse("Settlement").unwrap(),
            StrategyFacet::Settlement
        );
        assert_eq!(
            StrategyFacet::parse("  mevblocker_backrun ").unwrap(),
            StrategyFacet::MevblockerBackrun
        );
        assert_eq!(
            StrategyFacet::parse("txpool_backrun").unwrap(),
            StrategyFacet::TxpoolBackrun
        );
        assert!(
            StrategyFacet::parse("backrun").is_err(),
            "the retired single backrun spelling must not resolve"
        );
        assert!(StrategyFacet::parse("sandwich").is_err());
    }

    #[test]
    fn both_backrun_facets_activate_independently() {
        let file = test_file("activate-both-backruns");
        let env = empty_env();
        let ctx = ctx_at(&env, &file);
        execute(
            &StrategyCommand::Activate {
                facet: StrategyFacet::MevblockerBackrun,
                endpoints: None,
                endpoints_default: true,
            },
            &ctx,
            &NoPrompt,
        )
        .expect("activate mevblocker");
        execute(
            &StrategyCommand::Activate {
                facet: StrategyFacet::TxpoolBackrun,
                endpoints: None,
                endpoints_default: true,
            },
            &ctx,
            &NoPrompt,
        )
        .expect("activate peer");
        let loaded = BotConfigLoader::new()
            .with_config_path(&file)
            .without_env()
            .load()
            .expect("config loads");
        assert!(loaded.config.strategy.mevblocker_backrun.active);
        assert!(loaded.config.strategy.txpool_backrun.active);
        assert_eq!(
            loaded.config.strategy.txpool_backrun.endpoints.as_deref(),
            Some(
                degenbot_config::DEFAULT_TXPOOL_BACKRUN_RELAYS
                    .join(",")
                    .as_str()
            )
        );
    }

    // ── list / show ──

    #[test]
    fn list_reports_every_facet() {
        let env = empty_env();
        let ctx = CliContext::new(&env);
        let report = execute(&StrategyCommand::List, &ctx, &NoPrompt).expect("list executes");
        let StrategyReport::Listed { rows } = report else {
            panic!("list must produce Listed");
        };
        assert_eq!(rows.len(), StrategyFacet::ALL.len());
    }

    #[test]
    fn show_reports_activation_and_posture() {
        let file = test_file("show");
        let env = empty_env();
        let ctx = ctx_at(&env, &file);
        // Activate with the default posture first: show then renders it.
        execute(
            &StrategyCommand::Activate {
                facet: StrategyFacet::MevblockerBackrun,
                endpoints: None,
                endpoints_default: true,
            },
            &ctx,
            &NoPrompt,
        )
        .expect("activate executes");
        let report = execute(
            &StrategyCommand::Show {
                facet: StrategyFacet::MevblockerBackrun,
            },
            &ctx,
            &NoPrompt,
        )
        .expect("show executes");
        let StrategyReport::Shown {
            activation: Some(summary),
            ..
        } = report
        else {
            panic!("show must produce Shown with activation");
        };
        assert_eq!(
            summary,
            EndpointSummary::Default(vec![degenbot_config::DEFAULT_BACKRUN_STREAM_URL.to_string()])
        );
    }

    // ── activate ──

    #[test]
    fn activate_with_endpoints_default_stamps_the_pinned_set_and_active() {
        let file = test_file("activate-default");
        let env = empty_env();
        let ctx = ctx_at(&env, &file);
        execute(
            &StrategyCommand::Activate {
                facet: StrategyFacet::MevblockerBackrun,
                endpoints: None,
                endpoints_default: true,
            },
            &ctx,
            &NoPrompt,
        )
        .expect("activate executes");
        let loaded = BotConfigLoader::new()
            .with_config_path(&file)
            .without_env()
            .load()
            .expect("written config loads");
        assert!(loaded.config.strategy.mevblocker_backrun.active);
        assert_eq!(
            loaded
                .config
                .strategy
                .mevblocker_backrun
                .endpoints
                .as_deref(),
            Some(degenbot_config::DEFAULT_BACKRUN_STREAM_URL),
            "the pinned default must be stamped into the persisted list"
        );
    }

    #[test]
    fn activate_with_explicit_endpoints_writes_them() {
        let file = test_file("activate-explicit");
        let env = empty_env();
        let ctx = ctx_at(&env, &file);
        execute(
            &StrategyCommand::Activate {
                facet: StrategyFacet::Settlement,
                endpoints: Some("https://rpc.mevblocker.io/noreverts".to_string()),
                endpoints_default: false,
            },
            &ctx,
            &NoPrompt,
        )
        .expect("activate executes");
        let loaded = BotConfigLoader::new()
            .with_config_path(&file)
            .without_env()
            .load()
            .expect("written config loads");
        assert!(loaded.config.strategy.settlement.active);
        assert_eq!(
            loaded.config.strategy.settlement.endpoints.as_deref(),
            Some("https://rpc.mevblocker.io/noreverts")
        );
    }

    #[test]
    fn a_refused_activation_leaves_no_file_residue() {
        let file = test_file("activate-atomic");
        let env = empty_env();
        let ctx = ctx_at(&env, &file);
        let error = execute(
            &StrategyCommand::Activate {
                facet: StrategyFacet::Settlement,
                endpoints: Some("https://rpc.mevblocker.io/fast".to_string()),
                endpoints_default: false,
            },
            &ctx,
            &NoPrompt,
        )
        .expect_err("off-allowlist activation must refuse");
        assert!(
            error.message().contains("RELAYS_AND_GUARDRAILS"),
            "{}",
            error.message()
        );
        assert!(
            !file.exists(),
            "a refused activation must not create or touch the config file"
        );
    }

    #[test]
    fn activate_without_a_choice_refuses_naming_both_flags() {
        let file = test_file("activate-nochoice");
        let env = empty_env();
        let ctx = ctx_at(&env, &file);
        let error = execute(
            &StrategyCommand::Activate {
                facet: StrategyFacet::Settlement,
                endpoints: None,
                endpoints_default: false,
            },
            &ctx,
            &NoPrompt,
        )
        .expect_err("unsettled activation must refuse");
        let message = error.message();
        assert!(message.contains("--endpoints"), "{message}");
        assert!(message.contains("--endpoints-default"), "{message}");
        // Nothing was written.
        assert!(!file.exists() || std::fs::read_to_string(&file).expect("read").is_empty());
    }

    #[test]
    fn activate_with_both_choices_refuses() {
        let file = test_file("activate-both");
        let env = empty_env();
        let ctx = ctx_at(&env, &file);
        let error = execute(
            &StrategyCommand::Activate {
                facet: StrategyFacet::MevblockerBackrun,
                endpoints: Some("wss://searchers.example".to_string()),
                endpoints_default: true,
            },
            &ctx,
            &NoPrompt,
        )
        .expect_err("both choices must refuse");
        assert!(
            error.message().contains("exactly one"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn activate_refuses_off_allowlist_settlement_endpoints_at_write_time() {
        let file = test_file("activate-offallowlist");
        let env = empty_env();
        let ctx = ctx_at(&env, &file);
        let error = execute(
            &StrategyCommand::Activate {
                facet: StrategyFacet::Settlement,
                endpoints: Some("https://rpc.mevblocker.io/fast".to_string()),
                endpoints_default: false,
            },
            &ctx,
            &NoPrompt,
        )
        .expect_err("off-allowlist endpoints must refuse at write time");
        assert!(
            error.message().contains("RELAYS_AND_GUARDRAILS"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn reactivating_with_a_new_choice_overwrites_the_old() {
        let file = test_file("activate-rewrite");
        let env = empty_env();
        let ctx = ctx_at(&env, &file);
        execute(
            &StrategyCommand::Activate {
                facet: StrategyFacet::MevblockerBackrun,
                endpoints: Some("wss://searchers.example".to_string()),
                endpoints_default: false,
            },
            &ctx,
            &NoPrompt,
        )
        .expect("initial activate");
        // Flip to the default posture: the explicit key must drop away.
        execute(
            &StrategyCommand::Activate {
                facet: StrategyFacet::MevblockerBackrun,
                endpoints: None,
                endpoints_default: true,
            },
            &ctx,
            &NoPrompt,
        )
        .expect("re-activation executes");
        let loaded = BotConfigLoader::new()
            .with_config_path(&file)
            .without_env()
            .load()
            .expect("config loads");
        assert!(loaded.config.strategy.mevblocker_backrun.active);
        assert_eq!(
            loaded
                .config
                .strategy
                .mevblocker_backrun
                .endpoints
                .as_deref(),
            Some(degenbot_config::DEFAULT_BACKRUN_STREAM_URL),
            "the default re-activation must restamp the pinned list"
        );
    }

    #[test]
    fn activate_reports_an_env_shadow_loudly() {
        let file = test_file("activate-shadowed");
        let env = MapEnv::new(BTreeMap::from([(
            "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_ACTIVE".to_string(),
            "false".to_string(),
        )]));
        let ctx = ctx_at(&env, &file);
        let report = execute(
            &StrategyCommand::Activate {
                facet: StrategyFacet::MevblockerBackrun,
                endpoints: None,
                endpoints_default: true,
            },
            &ctx,
            &NoPrompt,
        )
        .expect("activate executes");
        let StrategyReport::Activated {
            outcome: MutationOutcome::Shadowed { env },
            ..
        } = report
        else {
            panic!("the shadow must reach the report");
        };
        assert_eq!(env, "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_ACTIVE");
    }

    #[test]
    fn deactivate_leaves_the_recorded_choice_alone() {
        let file = test_file("deactivate");
        let env = empty_env();
        let ctx = ctx_at(&env, &file);
        execute(
            &StrategyCommand::Activate {
                facet: StrategyFacet::Settlement,
                endpoints: None,
                endpoints_default: true,
            },
            &ctx,
            &NoPrompt,
        )
        .expect("activate");
        execute(
            &StrategyCommand::Deactivate {
                facet: StrategyFacet::Settlement,
            },
            &ctx,
            &NoPrompt,
        )
        .expect("deactivate");
        let loaded = BotConfigLoader::new()
            .with_config_path(&file)
            .without_env()
            .load()
            .expect("config loads");
        assert!(!loaded.config.strategy.settlement.active);
        assert_eq!(
            loaded.config.strategy.settlement.endpoints.as_deref(),
            Some(
                degenbot_config::SETTLEMENT_DEFAULT_ENDPOINTS
                    .join(",")
                    .as_str()
            ),
            "deactivation keeps the recorded endpoint set for re-activation"
        );
    }

    // ── set / default / remove ──

    #[test]
    fn set_writes_a_typed_override_and_default_restores_it() {
        let file = test_file("set-default");
        let env = empty_env();
        let ctx = ctx_at(&env, &file);
        execute(
            &StrategyCommand::Set {
                facet: StrategyFacet::MevblockerBackrun,
                key: "priority_fee_gwei".to_string(),
                value: "7".to_string(),
            },
            &ctx,
            &NoPrompt,
        )
        .expect("set executes");
        let loaded = BotConfigLoader::new()
            .with_config_path(&file)
            .without_env()
            .load()
            .expect("config loads");
        assert_eq!(
            loaded.config.strategy.mevblocker_backrun.priority_fee_gwei,
            7
        );

        execute(
            &StrategyCommand::Default {
                facet: StrategyFacet::MevblockerBackrun,
                key: "priority_fee_gwei".to_string(),
            },
            &ctx,
            &NoPrompt,
        )
        .expect("default executes");
        let restored = BotConfigLoader::new()
            .with_config_path(&file)
            .without_env()
            .load()
            .expect("config loads");
        assert_eq!(
            restored
                .config
                .strategy
                .mevblocker_backrun
                .priority_fee_gwei,
            2
        );
    }

    #[test]
    fn remove_is_the_default_verb_alias() {
        let file = test_file("remove-alias");
        let env = empty_env();
        let ctx = ctx_at(&env, &file);
        execute(
            &StrategyCommand::Set {
                facet: StrategyFacet::MevblockerBackrun,
                key: "bundle_gas_est".to_string(),
                value: "333000".to_string(),
            },
            &ctx,
            &NoPrompt,
        )
        .expect("set executes");
        execute(
            &StrategyCommand::Remove {
                facet: StrategyFacet::MevblockerBackrun,
                key: "bundle_gas_est".to_string(),
            },
            &ctx,
            &NoPrompt,
        )
        .expect("remove executes");
        let restored = BotConfigLoader::new()
            .with_config_path(&file)
            .without_env()
            .load()
            .expect("config loads");
        assert_eq!(
            restored.config.strategy.mevblocker_backrun.bundle_gas_est,
            300_000
        );
    }

    #[test]
    fn set_refuses_unknown_and_activation_owned_keys() {
        let file = test_file("set-refuse");
        let env = empty_env();
        let ctx = ctx_at(&env, &file);
        for (key, expectation) in [
            ("no_such_key", "declared"),
            ("log_stderr", "declared"),
            ("active", "activate verb"),
            ("endpoints", "activate verb"),
        ] {
            let error = execute(
                &StrategyCommand::Set {
                    facet: StrategyFacet::MevblockerBackrun,
                    key: key.to_string(),
                    value: "1".to_string(),
                },
                &ctx,
                &NoPrompt,
            )
            .expect_err("key must refuse");
            assert!(
                error.message().contains(expectation),
                "{key}: {}",
                error.message()
            );
        }
    }

    #[test]
    fn default_refuses_when_no_override_is_present() {
        let file = test_file("default-noop");
        let env = empty_env();
        let ctx = ctx_at(&env, &file);
        let error = execute(
            &StrategyCommand::Default {
                facet: StrategyFacet::MevblockerBackrun,
                key: "priority_fee_gwei".to_string(),
            },
            &ctx,
            &NoPrompt,
        )
        .expect_err("no-op default must refuse");
        assert!(
            error.message().contains("no file override"),
            "{}",
            error.message()
        );
    }
}
