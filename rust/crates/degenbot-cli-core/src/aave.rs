//! The `aave` command arms (ADR-051 D1;).
//!
//! Ports `cli/aave.py`:
//!
//! - `aave activate` — [`activate_aave_market`] (the one-time market seed).
//! - `aave deactivate` — the `(chain, name)` market read + the
//!   [`deactivate_aave_market`] row flip.
//! - `aave update` — the active-market walk over
//!   [`run_aave_update`], with the post-run zero-balance cleanup and the
//!   opt-in completion backup.
//! - `aave position show` — the market/user scalar row reads ported onto the
//!   `degenbot-db::aave` read surface.

use std::sync::Arc;

use alloy::primitives::{address, Address};
use degenbot_aave::updater::verify::cleanup_zero_balance_positions_on_conn;
use degenbot_aave::{
    activate_aave_market, deactivate_aave_market, run_aave_update, NoProgress, RunError,
};
use degenbot_db::{ops, DbError, DegenbotDb};

use crate::block::{parse_to_block, resolve_to_block};
use crate::cancel::CancelHandle;
use crate::context::CliContext;
use crate::error::CliError;
use crate::prompt::{PromptPlan, Prompter};
use crate::report::{
    AavePositionLine, AaveReport, AaveUpdateEntry, AaveUpdateOutcome, DeactivateOutcome,
};

/// An Aave V3 deployment (the Python `aave/deployments.py` constants).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AaveDeployment {
    /// The chain id.
    pub chain_id: u64,
    /// The human chain label.
    pub chain_label: &'static str,
    /// The `PoolAddressProvider` contract.
    pub pool_address_provider: Address,
    /// The chain's GHO token.
    pub gho_token_address: Address,
}

/// The shipped Aave V3 deployments (Ethereum mainnet only, mirroring Python).
pub const AAVE_DEPLOYMENTS: &[AaveDeployment] = &[AaveDeployment {
    chain_id: 1,
    chain_label: "Ethereum",
    pool_address_provider: address!("2f39d218133AFaB8F2B819B1066c7E434Ad94E9e"),
    gho_token_address: address!("40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f"),
}];

/// The `aave` command group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AaveCommand {
    /// Activate (or re-activate) the chain's Aave V3 market.
    Activate {
        /// The chain id.
        chain_id: u64,
    },
    /// Deactivate a market by chain + name.
    Deactivate {
        /// The chain id.
        chain_id: u64,
        /// The `aave_v3_markets.name` to flip.
        market_name: String,
    },
    /// Update positions for every active market.
    Update {
        /// Max blocks per chunk before committing.
        chunk_size: u64,
        /// The raw `--to-block` identifier.
        to_block: String,
        /// The per-chunk touched-position verify.
        verify_chunk: bool,
        /// The market-wide interval + completion verify.
        verify_all: bool,
        /// The interval for the market-wide gate.
        verify_all_interval: u64,
        /// Stop after the first committed chunk.
        stop_after_one_chunk: bool,
        /// Preview only (skips the Rust call entirely).
        dry_run: bool,
        /// Back up the DB once per market at the end of the run.
        enable_backup: bool,
    },
    /// Display a user's collateral + debt positions.
    PositionShow {
        /// The user address.
        address: String,
        /// The market name.
        market: String,
        /// The chain id.
        chain_id: u64,
    },
}

impl AaveCommand {
    /// The arm's confirmation policy: no Python aave handler prompts.
    #[must_use]
    pub const fn prompt_plan(&self, _ctx: &CliContext<'_>) -> PromptPlan {
        PromptPlan::None
    }
}

/// Resolve the Aave V3 deployment for `chain_id`.
///
/// # Errors
///
/// [`CliError::UnknownDeployment`] when the chain has no shipped deployment.
pub fn resolve_aave_deployment(chain_id: u64) -> Result<&'static AaveDeployment, CliError> {
    AAVE_DEPLOYMENTS
        .iter()
        .find(|d| d.chain_id == chain_id)
        .ok_or_else(|| CliError::UnknownDeployment {
            chain_id,
            name: "ethereum_aave_v3".to_string(),
        })
}

/// Execute an `aave` command.
///
/// # Errors
///
/// [`CliError::UnknownDeployment`] / [`CliError::InvalidAddress`] /
/// [`CliError::InvalidArgument`] for bad inputs, [`CliError::Config`] for an
/// unresolved driver-domain value, [`CliError::NoActiveAaveMarkets`] when
/// `aave update` finds nothing, and [`CliError::AaveUpdate`] for a core
/// failure.
pub(crate) fn execute(
    command: &AaveCommand,
    ctx: &CliContext<'_>,
    _prompter: &dyn Prompter,
    cancel: &CancelHandle,
) -> Result<AaveReport, CliError> {
    match command {
        AaveCommand::Activate { chain_id } => activate(ctx, *chain_id),
        AaveCommand::Deactivate {
            chain_id,
            market_name,
        } => deactivate(ctx, *chain_id, market_name),
        AaveCommand::Update {
            chunk_size,
            to_block,
            verify_chunk,
            verify_all,
            verify_all_interval,
            stop_after_one_chunk,
            dry_run,
            enable_backup,
        } => update(
            ctx,
            cancel,
            &UpdateArgs {
                chunk_size: *chunk_size,
                to_block,
                verify_chunk: *verify_chunk,
                verify_all: *verify_all,
                verify_all_interval: *verify_all_interval,
                stop_after_one_chunk: *stop_after_one_chunk,
                dry_run: *dry_run,
                enable_backup: *enable_backup,
            },
        ),
        AaveCommand::PositionShow {
            address,
            market,
            chain_id,
        } => position_show(ctx, address, market, *chain_id),
    }
}

/// `aave activate`.
fn activate(ctx: &CliContext<'_>, chain_id: u64) -> Result<AaveReport, CliError> {
    let deployment = resolve_aave_deployment(chain_id)?;
    let database_path = ctx.database_path().value;
    let rpc_url = ctx.node_http_uri_for(chain_id)?.value;
    let chain = i64::try_from(chain_id)
        .map_err(|_| CliError::InvalidArgument(format!("chain id {chain_id} is out of range")))?;
    let result = activate_aave_market(
        &database_path,
        chain,
        &deployment.pool_address_provider.to_checksum(None),
        &deployment.gho_token_address.to_checksum(None),
        &rpc_url,
    )
    .map_err(CliError::AaveUpdate)?;
    tracing::info!(
        chain_id,
        market_id = result.market_id,
        "activated Aave V3 market"
    );
    Ok(AaveReport::Activated {
        chain_id,
        chain_label: deployment.chain_label,
        market_id: result.market_id,
        market_name: result.market_name,
        created: result.created,
    })
}

/// `aave deactivate`.
fn deactivate(
    ctx: &CliContext<'_>,
    chain_id: u64,
    market_name: &str,
) -> Result<AaveReport, CliError> {
    let database_path = ctx.database_path().value;
    let chain = i64::try_from(chain_id)
        .map_err(|_| CliError::InvalidArgument(format!("chain id {chain_id} is out of range")))?;
    let market = {
        let db = (DegenbotDb::open(&database_path)?).0;
        db.fetch_aave_market_by_name(chain, market_name)?
    };
    let Some(market) = market else {
        return Ok(AaveReport::Deactivated {
            chain_id,
            market_id: None,
            outcome: DeactivateOutcome::NoEntry,
        });
    };
    if !market.active {
        return Ok(AaveReport::Deactivated {
            chain_id,
            market_id: Some(market.id),
            outcome: DeactivateOutcome::AlreadyDeactivated,
        });
    }
    deactivate_aave_market(&database_path, market.id).map_err(CliError::AaveUpdate)?;
    tracing::info!(
        chain_id,
        market_id = market.id,
        "deactivated Aave V3 market"
    );
    Ok(AaveReport::Deactivated {
        chain_id,
        market_id: Some(market.id),
        outcome: DeactivateOutcome::Deactivated,
    })
}

/// The `aave update` flag bundle.
#[expect(clippy::struct_excessive_bools)]
struct UpdateArgs<'a> {
    chunk_size: u64,
    to_block: &'a str,
    verify_chunk: bool,
    verify_all: bool,
    verify_all_interval: u64,
    stop_after_one_chunk: bool,
    dry_run: bool,
    enable_backup: bool,
}

/// `aave update`.
#[expect(clippy::too_many_lines)]
fn update(
    ctx: &CliContext<'_>,
    cancel: &CancelHandle,
    args: &UpdateArgs<'_>,
) -> Result<AaveReport, CliError> {
    let database_path = ctx.database_path().value;
    let markets = {
        let db = (DegenbotDb::open(&database_path)?).0;
        db.fetch_active_aave_markets()?
    };
    if markets.is_empty() {
        return Err(CliError::NoActiveAaveMarkets);
    }
    let spec = parse_to_block(args.to_block)?;
    let interval = if args.verify_all {
        Some(args.verify_all_interval)
    } else {
        None
    };
    let max_chunks = if args.stop_after_one_chunk {
        Some(1)
    } else {
        None
    };
    let mut entries: Vec<AaveUpdateEntry> = Vec::new();
    let mut chain_ids: Vec<i64> = Vec::new();
    for market in &markets {
        if !chain_ids.contains(&market.chain_id) {
            chain_ids.push(market.chain_id);
        }
    }
    for chain in chain_ids {
        if cancel.is_cancelled() {
            break;
        }
        let chain_unsigned = u64::try_from(chain).map_err(|_| {
            CliError::InvalidArgument(format!("market chain id {chain} is out of range"))
        })?;
        let rpc_url = ctx.node_http_uri_for(chain_unsigned)?.value;
        let resolved = resolve_to_block(spec, &rpc_url)?;
        for market in markets.iter().filter(|m| m.chain_id == chain) {
            if cancel.is_cancelled() {
                return Ok(AaveReport::Updated { entries });
            }
            let Some(last_update_block) = market.last_update_block else {
                entries.push(AaveUpdateEntry {
                    chain_id: chain,
                    market_id: market.id,
                    market_name: market.name.clone(),
                    outcome: AaveUpdateOutcome::NeedsBootstrap,
                });
                continue;
            };
            if args.dry_run {
                entries.push(AaveUpdateEntry {
                    chain_id: chain,
                    market_id: market.id,
                    market_name: market.name.clone(),
                    outcome: AaveUpdateOutcome::DryRun {
                        last_update_block,
                        to_block: resolved,
                    },
                });
                continue;
            }
            match run_aave_update(
                &database_path,
                chain,
                market.id,
                resolved,
                args.chunk_size,
                &rpc_url,
                cancel.flag(),
                Arc::new(NoProgress),
                args.verify_chunk,
                interval,
                args.verify_all,
                max_chunks,
            ) {
                Ok(report) => {
                    entries.push(AaveUpdateEntry {
                        chain_id: chain,
                        market_id: market.id,
                        market_name: market.name.clone(),
                        outcome: AaveUpdateOutcome::Advanced {
                            from_block: report.from_block,
                            to_block: report.to_block,
                            chunks_committed: report.chunks_committed,
                            total_events_applied: report.total_events_applied,
                        },
                    });
                    cleanup_zero_balance_positions(&database_path, market.id)?;
                    if args.enable_backup {
                        let backup = backup_for_block(&database_path, report.to_block)?;
                        tracing::info!(
                            chain_id = chain,
                            to_block = report.to_block,
                            backup = %backup.display(),
                            "created Aave database backup at block"
                        );
                    }
                }
                Err(RunError::Cancelled) => {
                    entries.push(AaveUpdateEntry {
                        chain_id: chain,
                        market_id: market.id,
                        market_name: market.name.clone(),
                        outcome: AaveUpdateOutcome::Cancelled,
                    });
                    return Ok(AaveReport::Updated { entries });
                }
                Err(err) => return Err(CliError::AaveUpdate(err)),
            }
        }
    }
    Ok(AaveReport::Updated { entries })
}

/// Delete the market's zero-balance collateral + debt rows under one transaction.
fn cleanup_zero_balance_positions(
    database_path: &std::path::Path,
    market_id: i64,
) -> Result<(), CliError> {
    let (db, _state) = DegenbotDb::open_for_writes(database_path)?;
    let mut guard = db.lock();
    let tx = guard.transaction().map_err(DbError::from)?;
    cleanup_zero_balance_positions_on_conn(&tx, market_id)?;
    tx.commit().map_err(DbError::from)?;
    Ok(())
}

/// The `<stem>-<to_block>.db.bak` sibling the Python completion backup writes.
fn backup_for_block(
    database_path: &std::path::Path,
    to_block: u64,
) -> Result<std::path::PathBuf, CliError> {
    let stem = database_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut backup = database_path.to_path_buf();
    backup.set_file_name(format!("{stem}-{to_block}.db.bak"));
    ops::backup_database(database_path, &backup)?;
    Ok(backup)
}

/// `aave position show`.
fn position_show(
    ctx: &CliContext<'_>,
    raw_address: &str,
    market: &str,
    chain_id: u64,
) -> Result<AaveReport, CliError> {
    let address: Address = raw_address
        .parse()
        .map_err(|_| CliError::InvalidAddress(raw_address.to_string()))?;
    let user_address = address.to_checksum(None);
    let chain = i64::try_from(chain_id)
        .map_err(|_| CliError::InvalidArgument(format!("chain id {chain_id} is out of range")))?;
    let database_path = ctx.database_path().value;
    let db = (DegenbotDb::open(&database_path)?).0;
    let Some(market_row) = db.fetch_aave_market_by_name(chain, market)? else {
        return Ok(AaveReport::PositionNoMarket {
            market: market.to_string(),
            chain_id,
        });
    };
    let Some(user) = db.fetch_aave_user_by_address(market_row.id, &user_address)? else {
        return Ok(AaveReport::PositionNoUser {
            user_address,
            market: market.to_string(),
            chain_id,
        });
    };
    let collateral = db
        .fetch_aave_collateral_positions(user.id)?
        .into_iter()
        .map(|p| AavePositionLine {
            symbol: p.underlying_symbol.unwrap_or_else(|| "Unknown".to_string()),
            balance: p.balance,
        })
        .collect();
    let debt = db
        .fetch_aave_debt_positions(user.id)?
        .into_iter()
        .map(|p| AavePositionLine {
            symbol: p.underlying_symbol.unwrap_or_else(|| "Unknown".to_string()),
            balance: p.balance,
        })
        .collect();
    Ok(AaveReport::Position {
        user_address,
        market: market.to_string(),
        chain_id,
        collateral,
        debt,
    })
}
