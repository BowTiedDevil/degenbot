//! The `strategy` command arms: the console view of the typed strategy
//! facets (ADR-055 D1/D5).
//!
//! A strategy **facet** is one typed per-strategy config section
//! (`strategy.settlement`, `strategy.backrun`); `strategy.name` is the single
//! arm selector until the Phase-C host can enable more than one. This module
//! owns the facet descriptor table and the pure transforms over it; it never
//! reads a config file or the environment (the argv facade and the loader do
//! that).
//!
//! The facets declare their keys from the schema registry — the migrated
//! backrun knobs live in the typed `strategy.backrun` facet (settlement is
//! still empty) — but the console exposes no write path yet, so `add`/`set`/
//! `remove` refuse loudly rather than silently no-op. `list`/`show` are the
//! working verbs.

use degenbot_config::SCHEMA;

use crate::context::CliContext;
use crate::error::CliError;
use crate::prompt::PromptPlan;
use crate::report::StrategyReport;

/// One strategy arm — the two reaction kinds of ADR-055 (D1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrategyFacet {
    /// The settled-block strategy (settlement arbitrage).
    Settlement,
    /// The pending-transaction strategy (backrun).
    Backrun,
}

impl StrategyFacet {
    /// Both facets, in declaration order.
    pub const ALL: [Self; 2] = [Self::Settlement, Self::Backrun];

    /// The canonical lowercase spelling (`settlement` / `backrun`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Settlement => "settlement",
            Self::Backrun => "backrun",
        }
    }

    /// The reaction kind this facet implements (ADR-055 D1).
    #[must_use]
    pub const fn trigger_kind(self) -> &'static str {
        match self {
            Self::Settlement => "settled-block",
            Self::Backrun => "pending-transaction",
        }
    }

    /// The facet's dotted TOML section path.
    #[must_use]
    pub const fn config_section(self) -> &'static str {
        match self {
            Self::Settlement => "strategy.settlement",
            Self::Backrun => "strategy.backrun",
        }
    }

    /// Parse a facet selector case-insensitively.
    ///
    /// # Errors
    ///
    /// [`CliError::InvalidArgument`] naming the accepted spellings.
    pub fn parse(raw: &str) -> Result<Self, CliError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "settlement" => Ok(Self::Settlement),
            "backrun" => Ok(Self::Backrun),
            other => Err(CliError::InvalidArgument(format!(
                "unknown strategy facet {other:?} (expected one of: settlement backrun)"
            ))),
        }
    }
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

/// The `strategy` command group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrategyCommand {
    /// `strategy list`: every declared facet with its section and trigger kind.
    List,
    /// `strategy show <facet>`: one facet's descriptor and declared keys.
    Show {
        /// The facet to show.
        facet: StrategyFacet,
    },
    /// `strategy add <facet> <key> <value>`: refused — no console write path.
    Add {
        /// The facet the key would belong to.
        facet: StrategyFacet,
        /// The key name.
        key: String,
        /// The raw value.
        value: String,
    },
    /// `strategy set <facet> <key> <value>`: refused — no console write path.
    Set {
        /// The facet the key would belong to.
        facet: StrategyFacet,
        /// The key name.
        key: String,
        /// The raw value.
        value: String,
    },
    /// `strategy remove <facet> <key>`: refused — no console write path.
    Remove {
        /// The facet the key would belong to.
        facet: StrategyFacet,
        /// The key name.
        key: String,
    },
}

impl StrategyCommand {
    /// No strategy arm prompts.
    #[must_use]
    pub const fn prompt_plan(&self, _ctx: &CliContext<'_>) -> PromptPlan {
        PromptPlan::None
    }

    /// The `list`/`show` rendering inputs, or the loud refusal for a mutation
    /// verb (the console has no write path — ADR-055 Phase C).
    ///
    /// # Errors
    ///
    /// [`CliError::InvalidArgument`] for `add`/`set`/`remove`.
    pub fn report(&self) -> Result<StrategyReport, CliError> {
        match self {
            Self::List => Ok(StrategyReport::Listed {
                rows: descriptors(),
            }),
            Self::Show { facet } => Ok(StrategyReport::Shown {
                descriptor: descriptor(*facet),
            }),
            Self::Add { facet, .. } | Self::Set { facet, .. } => {
                Err(CliError::InvalidArgument(format!(
                    "strategy {} is not writable: the {} facet has no console write path \
                     (ADR-055 Phase C); edit the typed config directly for now",
                    if matches!(self, Self::Add { .. }) {
                        "add"
                    } else {
                        "set"
                    },
                    facet.config_section()
                )))
            }
            Self::Remove { facet, .. } => Err(CliError::InvalidArgument(format!(
                "strategy remove is not writable: the {} facet has no console write path \
                 (ADR-055 Phase C); edit the typed config directly for now",
                facet.config_section()
            ))),
        }
    }
}

/// Execute a `strategy` command.
///
/// # Errors
///
/// [`CliError::InvalidArgument`] for a mutating verb (see
/// [`StrategyCommand::report`]).
pub(crate) fn execute(
    command: &StrategyCommand,
    _ctx: &CliContext<'_>,
    _prompter: &dyn crate::prompt::Prompter,
) -> Result<StrategyReport, CliError> {
    command.report()
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "unit tests: a malformed fixture must fail the test loudly"
)]
mod tests {
    use super::*;
    use crate::context::CliContext;
    use degenbot_config::{MapEnv, SECTION_PATHS};
    use std::collections::BTreeMap;

    #[test]
    fn facets_name_their_typed_sections_and_trigger_kinds() {
        for descriptor in descriptors() {
            assert!(
                SECTION_PATHS.contains(&descriptor.config_section),
                "{} must be a declared schema section",
                descriptor.config_section
            );
            match descriptor.facet {
                StrategyFacet::Settlement => {
                    assert_eq!(descriptor.trigger_kind, "settled-block");
                }
                StrategyFacet::Backrun => {
                    assert_eq!(descriptor.trigger_kind, "pending-transaction");
                }
            }
        }
    }

    #[test]
    fn facet_descriptors_mirror_their_schema_keys() {
        // The typed facets carry exactly the schema keys declared under their
        // dotted section path: the migrated backrun knobs are present, and
        // settlement remains empty until its own migration.
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
            !descriptor(StrategyFacet::Backrun).fields.is_empty(),
            "strategy.backrun declares its migrated keys"
        );
        assert!(descriptor(StrategyFacet::Settlement).fields.is_empty());
    }

    #[test]
    fn facet_selector_parses_case_insensitively_and_refuses_unknown() {
        assert_eq!(
            StrategyFacet::parse("Settlement").unwrap(),
            StrategyFacet::Settlement
        );
        assert_eq!(
            StrategyFacet::parse("  backrun ").unwrap(),
            StrategyFacet::Backrun
        );
        assert!(StrategyFacet::parse("sandwich").is_err());
    }

    #[test]
    fn list_reports_every_facet_and_show_reports_one() {
        let listed = StrategyCommand::List.report().unwrap();
        let StrategyReport::Listed { rows } = listed else {
            panic!("list must produce Listed");
        };
        assert_eq!(rows.len(), StrategyFacet::ALL.len());

        let shown = StrategyCommand::Show {
            facet: StrategyFacet::Backrun,
        }
        .report()
        .unwrap();
        let StrategyReport::Shown { descriptor } = shown else {
            panic!("show must produce Shown");
        };
        assert_eq!(descriptor.facet, StrategyFacet::Backrun);
        assert_eq!(descriptor.config_section, "strategy.backrun");
    }

    #[test]
    fn mutation_verbs_refuse_loudly_without_a_write_path() {
        struct NoPrompt;
        impl crate::prompt::Prompter for NoPrompt {
            fn confirm(&self, _message: &str, default: bool) -> bool {
                default
            }
        }
        let env = MapEnv::new(BTreeMap::new());
        let ctx = CliContext::new(&env);
        for command in [
            StrategyCommand::Add {
                facet: StrategyFacet::Backrun,
                key: "enabled".to_string(),
                value: "1".to_string(),
            },
            StrategyCommand::Set {
                facet: StrategyFacet::Backrun,
                key: "enabled".to_string(),
                value: "1".to_string(),
            },
            StrategyCommand::Remove {
                facet: StrategyFacet::Backrun,
                key: "enabled".to_string(),
            },
        ] {
            let err = execute(&command, &ctx, &NoPrompt).unwrap_err();
            let message = err.message();
            assert!(
                message.contains("strategy.backrun") && message.contains("Phase C"),
                "unexpected refusal: {message}"
            );
        }
    }
}
