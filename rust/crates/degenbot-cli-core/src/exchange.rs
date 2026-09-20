//! The `exchange` command arms (ADR-051 D5) — data, not 34 verbs.
//!
//! The Python-era click tree declared one verb per `(chain, DEX)` pair
//! (`base_aerodrome_v2`, `ethereum_uniswap_v3`, … — 17 pairs, each with an
//! activate + deactivate handler). Those verbs are now ONE constructor-parsed
//! command, `exchange activate|deactivate --chain <slug|id> --name <dex>`,
//! resolving `(chain, name)` through the [`RetiredExchange`] table and the
//! CREATE2 identity (factory/deployer) through
//! [`degenbot_uniswap::deployments`] — the byte-compared single source.
//!
//! The write path is `degenbot-db`'s discovery primitives:
//! [`upsert_exchange`](degenbot_db::DegenbotDb::upsert_exchange) (get-or-create,
//! `active = false`), [`set_exchange_active`](degenbot_db::DegenbotDb::set_exchange_active),
//! and — for the Uniswap V4 singleton — [`upsert_pool_manager`](degenbot_db::DegenbotDb::upsert_pool_manager).
//!
//! The V4 `pool_manager`/`state_view` are the SAME literals the Python module
//! carries (`deployments.py` documents them as "singleton addresses with no
//! factory path in the registry yet"); they are the only table entries that do
//! not resolve to a registry record.

use alloy::primitives::{address, Address};
use degenbot_db::DegenbotDb;
use degenbot_uniswap::deployments;

use crate::context::CliContext;
use crate::error::CliError;
use crate::prompt::{PromptPlan, Prompter};
use crate::report::{
    ActivateOutcome, DeactivateOutcome, ExchangeActiveState, ExchangeListRow, ExchangeReport,
};

/// The `exchange` command group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExchangeCommand {
    /// Activate the exchange (get-or-create the row, flip `active = true`).
    Activate {
        /// The chain selector (`base`, `ethereum`, or a numeric id).
        chain: String,
        /// The DEX name slug (`aerodrome_v2`, `uniswap_v3`, …).
        name: String,
    },
    /// Deactivate the exchange (resolve the row by name, flip `active = false`).
    Deactivate {
        /// The chain selector.
        chain: String,
        /// The DEX name slug.
        name: String,
    },
    /// List every supported `(chain, DEX)` pair and its DB activation state.
    List {
        /// The optional chain filter (a chain slug or numeric id).
        chain: Option<String>,
    },
}

impl ExchangeCommand {
    /// The arm's confirmation policy: the Python exchange handlers never prompt.
    #[must_use]
    pub const fn prompt_plan(&self, _ctx: &CliContext<'_>) -> PromptPlan {
        PromptPlan::None
    }

    /// The `(chain, name)` pair the command addresses (`None` for the filterless
    /// `List` arm).
    #[must_use]
    pub fn selector(&self) -> Option<(&str, &str)> {
        match self {
            Self::Activate { chain, name } | Self::Deactivate { chain, name } => Some((chain, name)),
            Self::List { .. } => None,
        }
    }
}

/// A deployed pool manager singleton (Uniswap V4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolManagerDeployment {
    /// The `PoolManager` address (the DB `exchanges.factory`).
    pub address: Address,
    /// The companion `StateView` contract.
    pub state_view: Address,
}

/// A resolved exchange deployment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExchangeDeployment {
    /// The resolved chain id.
    pub chain_id: u64,
    /// The human chain label (`Base`, `Ethereum`).
    pub chain_label: &'static str,
    /// The DEX name slug stored in the DB `exchanges.name`.
    pub dex_slug: &'static str,
    /// The human DEX label used in the report lines.
    pub display_name: &'static str,
    /// The DB `exchanges.factory` value.
    pub factory: Address,
    /// The DB `exchanges.deployer` value (the JSON `deployer`; `None` = factory).
    pub deployer: Option<Address>,
    /// The V4 `pool_managers` row to upsert on activate, when applicable.
    pub pool_manager: Option<PoolManagerDeployment>,
}

/// Where a retired exchange's identity comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentSource {
    /// A `(chain, factory)` row in the shipped `deployments.json` registry.
    Registry {
        /// The factory address (the registry lookup key).
        factory: Address,
    },
    /// A Uniswap V4 singleton (no registry factory path yet).
    V4Singleton {
        /// The `PoolManager` address.
        pool_manager: Address,
        /// The `StateView` address.
        state_view: Address,
    },
}

/// One retired Python verb pair's `(chain, DEX)` identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetiredExchange {
    /// The Python verb's chain prefix (`base`, `ethereum`).
    pub chain_slug: &'static str,
    /// The resolved chain id.
    pub chain_id: u64,
    /// The human chain label.
    pub chain_label: &'static str,
    /// The DEX name slug.
    pub dex_slug: &'static str,
    /// The human DEX label.
    pub display_name: &'static str,
    /// The identity source.
    pub source: DeploymentSource,
}

/// Every retired `exchange activate|deactivate` verb, collapsed to its
/// `(chain, dex)` identity. This is the data-driven replacement for the 34
/// click handlers; the coverage test asserts each entry resolves.
pub const RETIRED_EXCHANGES: &[RetiredExchange] = &[
    // ── Base mainnet ─────────────────────────────────────────────────────
    RetiredExchange {
        chain_slug: "base",
        chain_id: 8453,
        chain_label: "Base",
        dex_slug: "aerodrome_v2",
        display_name: "Aerodrome V2",
        source: DeploymentSource::Registry {
            factory: address!("420DD381b31aEf6683db6B902084cB0FFECe40Da"),
        },
    },
    RetiredExchange {
        chain_slug: "base",
        chain_id: 8453,
        chain_label: "Base",
        dex_slug: "aerodrome_v3",
        display_name: "Aerodrome V3",
        source: DeploymentSource::Registry {
            factory: address!("5e7BB104d84c7CB9B682AaC2F3d509f5F406809A"),
        },
    },
    RetiredExchange {
        chain_slug: "base",
        chain_id: 8453,
        chain_label: "Base",
        dex_slug: "pancakeswap_v2",
        display_name: "Pancakeswap V2",
        source: DeploymentSource::Registry {
            factory: address!("02a84c1b3BBD7401a5f7fa98a384EBC70bB5749E"),
        },
    },
    RetiredExchange {
        chain_slug: "base",
        chain_id: 8453,
        chain_label: "Base",
        dex_slug: "pancakeswap_v3",
        display_name: "Pancakeswap V3",
        source: DeploymentSource::Registry {
            factory: address!("0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"),
        },
    },
    RetiredExchange {
        chain_slug: "base",
        chain_id: 8453,
        chain_label: "Base",
        dex_slug: "swapbased_v2",
        display_name: "SwapBased V2",
        source: DeploymentSource::Registry {
            factory: address!("04C9f118d21e8B767D2e50C946f0cC9F6C367300"),
        },
    },
    RetiredExchange {
        chain_slug: "base",
        chain_id: 8453,
        chain_label: "Base",
        dex_slug: "sushiswap_v2",
        display_name: "Sushiswap V2",
        source: DeploymentSource::Registry {
            factory: address!("71524B4f93c58fcbF659783284E38825f0622859"),
        },
    },
    RetiredExchange {
        chain_slug: "base",
        chain_id: 8453,
        chain_label: "Base",
        dex_slug: "sushiswap_v3",
        display_name: "Sushiswap V3",
        source: DeploymentSource::Registry {
            factory: address!("c35DADB65012eC5796536bD9864eD8773aBc74C4"),
        },
    },
    RetiredExchange {
        chain_slug: "base",
        chain_id: 8453,
        chain_label: "Base",
        dex_slug: "uniswap_v2",
        display_name: "Uniswap V2",
        source: DeploymentSource::Registry {
            factory: address!("8909Dc15e40173Ff4699343b6eB8132c65e18eC6"),
        },
    },
    RetiredExchange {
        chain_slug: "base",
        chain_id: 8453,
        chain_label: "Base",
        dex_slug: "uniswap_v3",
        display_name: "Uniswap V3",
        source: DeploymentSource::Registry {
            factory: address!("33128a8fC17869897dcE68Ed026d694621f6FDfD"),
        },
    },
    RetiredExchange {
        chain_slug: "base",
        chain_id: 8453,
        chain_label: "Base",
        dex_slug: "uniswap_v4",
        display_name: "Uniswap V4",
        source: DeploymentSource::V4Singleton {
            pool_manager: address!("498581fF718922c3f8e6A244956aF099B2652b2b"),
            state_view: address!("A3c0c9b65baD0b08107Aa264b0f3dB444b867A71"),
        },
    },
    // ── Ethereum mainnet ─────────────────────────────────────────────────
    RetiredExchange {
        chain_slug: "ethereum",
        chain_id: 1,
        chain_label: "Ethereum",
        dex_slug: "pancakeswap_v2",
        display_name: "Pancakeswap V2",
        source: DeploymentSource::Registry {
            factory: address!("1097053Fd2ea711dad45caCcc45EfF7548fCB362"),
        },
    },
    RetiredExchange {
        chain_slug: "ethereum",
        chain_id: 1,
        chain_label: "Ethereum",
        dex_slug: "pancakeswap_v3",
        display_name: "Pancakeswap V3",
        source: DeploymentSource::Registry {
            factory: address!("0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"),
        },
    },
    RetiredExchange {
        chain_slug: "ethereum",
        chain_id: 1,
        chain_label: "Ethereum",
        dex_slug: "sushiswap_v2",
        display_name: "Sushiswap V2",
        source: DeploymentSource::Registry {
            factory: address!("C0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"),
        },
    },
    RetiredExchange {
        chain_slug: "ethereum",
        chain_id: 1,
        chain_label: "Ethereum",
        dex_slug: "sushiswap_v3",
        display_name: "Sushiswap V3",
        source: DeploymentSource::Registry {
            factory: address!("bACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"),
        },
    },
    RetiredExchange {
        chain_slug: "ethereum",
        chain_id: 1,
        chain_label: "Ethereum",
        dex_slug: "uniswap_v2",
        display_name: "Uniswap V2",
        source: DeploymentSource::Registry {
            factory: address!("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"),
        },
    },
    RetiredExchange {
        chain_slug: "ethereum",
        chain_id: 1,
        chain_label: "Ethereum",
        dex_slug: "uniswap_v3",
        display_name: "Uniswap V3",
        source: DeploymentSource::Registry {
            factory: address!("1F98431c8aD98523631AE4a59f267346ea31F984"),
        },
    },
    RetiredExchange {
        chain_slug: "ethereum",
        chain_id: 1,
        chain_label: "Ethereum",
        dex_slug: "uniswap_v4",
        display_name: "Uniswap V4",
        source: DeploymentSource::V4Singleton {
            pool_manager: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
            state_view: address!("7fFE42C4a5DEeA5b0feC41C94C136Cf115597227"),
        },
    },
];

/// Resolve `(chain_id, dex_slug)` to a deployment, reading the registry record
/// (factory/deployer) where one exists.
///
/// # Errors
///
/// [`CliError::UnknownDeployment`] when the pair is not a retired exchange, or
/// when a registry-backed entry has no shipped `deployments.json` record.
pub fn resolve_deployment(chain_id: u64, dex_slug: &str) -> Result<ExchangeDeployment, CliError> {
    let entry = RETIRED_EXCHANGES
        .iter()
        .find(|e| e.chain_id == chain_id && e.dex_slug == dex_slug)
        .ok_or_else(|| CliError::UnknownDeployment {
            chain_id,
            name: dex_slug.to_string(),
        })?;
    match entry.source {
        DeploymentSource::Registry { factory } => {
            let record = deployments::lookup(chain_id, factory).ok_or_else(|| {
                CliError::UnknownDeployment {
                    chain_id,
                    name: dex_slug.to_string(),
                }
            })?;
            Ok(ExchangeDeployment {
                chain_id: entry.chain_id,
                chain_label: entry.chain_label,
                dex_slug: entry.dex_slug,
                display_name: entry.display_name,
                factory: record.factory,
                deployer: record.deployer,
                pool_manager: None,
            })
        }
        DeploymentSource::V4Singleton {
            pool_manager,
            state_view,
        } => Ok(ExchangeDeployment {
            chain_id: entry.chain_id,
            chain_label: entry.chain_label,
            dex_slug: entry.dex_slug,
            display_name: entry.display_name,
            factory: pool_manager,
            deployer: None,
            pool_manager: Some(PoolManagerDeployment {
                address: pool_manager,
                state_view,
            }),
        }),
    }
}

/// Execute an `exchange` command.
///
/// # Errors
///
/// [`CliError::UnknownChain`] / [`CliError::UnknownDeployment`] for an
/// unresolvable selector, or a DB failure from the discovery primitives.
pub(crate) fn execute(
    command: &ExchangeCommand,
    ctx: &CliContext<'_>,
    _prompter: &dyn Prompter,
) -> Result<ExchangeReport, CliError> {
    let path = ctx.database_path().value;
    match command {
        ExchangeCommand::Activate { chain, name } => {
            activate(&deployment_for(chain, name)?, &path)
        }
        ExchangeCommand::Deactivate { chain, name } => {
            deactivate(&deployment_for(chain, name)?, &path)
        }
        ExchangeCommand::List { chain } => list(chain.as_deref(), &path),
    }
}

/// Resolve an activate/deactivate arm's `(chain, name)` pair to its deployment.
fn deployment_for(chain: &str, name: &str) -> Result<ExchangeDeployment, CliError> {
    let chain_id = crate::block::resolve_chain_selector(chain)?;
    resolve_deployment(chain_id, name)
}

/// List every supported `(chain, DEX)` pair (optionally one chain's), joined
/// with its DB activation state.
fn list(
    chain_filter: Option<&str>,
    path: &std::path::Path,
) -> Result<ExchangeReport, CliError> {
    let chain_id = chain_filter.map(crate::block::resolve_chain_selector).transpose()?;
    let (db, _state) = DegenbotDb::open(path)?;
    let mut rows = Vec::new();
    for entry in RETIRED_EXCHANGES {
        if chain_id.is_some_and(|filtered| filtered != entry.chain_id) {
            continue;
        }
        let row = db.fetch_exchange_by_name(
            i64::try_from(entry.chain_id).map_err(|_| {
                CliError::InvalidArgument(format!("chain id {} is out of range", entry.chain_id))
            })?,
            entry.dex_slug,
        )?;
        let state = match row {
            Some(row) if row.active => ExchangeActiveState::Active,
            Some(_) => ExchangeActiveState::Inactive,
            None => ExchangeActiveState::NoEntry,
        };
        rows.push(ExchangeListRow {
            chain_id: entry.chain_id,
            chain_label: entry.chain_label,
            display_name: entry.display_name,
            dex_slug: entry.dex_slug,
            factory: resolve_deployment(entry.chain_id, entry.dex_slug)?.factory.to_string(),
            state,
        });
    }
    Ok(ExchangeReport::Listed { rows })
}

/// Activate (get-or-create the row, flip active true, upsert the V4 manager).
fn activate(
    deployment: &ExchangeDeployment,
    path: &std::path::Path,
) -> Result<ExchangeReport, CliError> {
    let chain = i64::try_from(deployment.chain_id).map_err(|_| {
        CliError::InvalidArgument(format!("chain id {} is out of range", deployment.chain_id))
    })?;
    let (db, _state) = DegenbotDb::open_for_writes(path)?;
    let row = db.upsert_exchange(
        chain,
        deployment.dex_slug,
        deployment.factory,
        deployment.deployer,
    )?;
    if row.active {
        return Ok(ExchangeReport::Activated {
            chain_id: deployment.chain_id,
            chain_label: deployment.chain_label,
            display_name: deployment.display_name,
            dex_slug: deployment.dex_slug,
            outcome: ActivateOutcome::AlreadyActive,
        });
    }
    db.set_exchange_active(row.id, true)?;
    if let Some(manager) = deployment.pool_manager {
        db.upsert_pool_manager(
            manager.address,
            chain,
            deployment.dex_slug,
            Some(manager.state_view),
            row.id,
        )?;
    }
    tracing::info!(
        chain_id = deployment.chain_id,
        name = deployment.dex_slug,
        "activated exchange"
    );
    Ok(ExchangeReport::Activated {
        chain_id: deployment.chain_id,
        chain_label: deployment.chain_label,
        display_name: deployment.display_name,
        dex_slug: deployment.dex_slug,
        outcome: ActivateOutcome::Activated,
    })
}

/// Deactivate (resolve by name, flip active false).
fn deactivate(
    deployment: &ExchangeDeployment,
    path: &std::path::Path,
) -> Result<ExchangeReport, CliError> {
    let chain = i64::try_from(deployment.chain_id).map_err(|_| {
        CliError::InvalidArgument(format!("chain id {} is out of range", deployment.chain_id))
    })?;
    let (db, _state) = DegenbotDb::open_for_writes(path)?;
    let Some(row) = db.fetch_exchange_by_name(chain, deployment.dex_slug)? else {
        return Ok(ExchangeReport::Deactivated {
            chain_id: deployment.chain_id,
            chain_label: deployment.chain_label,
            display_name: deployment.display_name,
            dex_slug: deployment.dex_slug,
            outcome: DeactivateOutcome::NoEntry,
        });
    };
    if !row.active {
        return Ok(ExchangeReport::Deactivated {
            chain_id: deployment.chain_id,
            chain_label: deployment.chain_label,
            display_name: deployment.display_name,
            dex_slug: deployment.dex_slug,
            outcome: DeactivateOutcome::AlreadyDeactivated,
        });
    }
    db.set_exchange_active(row.id, false)?;
    tracing::info!(
        chain_id = deployment.chain_id,
        name = deployment.dex_slug,
        "deactivated exchange"
    );
    Ok(ExchangeReport::Deactivated {
        chain_id: deployment.chain_id,
        chain_label: deployment.chain_label,
        display_name: deployment.display_name,
        dex_slug: deployment.dex_slug,
        outcome: DeactivateOutcome::Deactivated,
    })
}
