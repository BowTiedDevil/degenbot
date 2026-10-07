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
//! - `aave reset` — the market-scoped purge followed by the same cold-boot
//!   update a fresh empty database takes.

use std::sync::Arc;

use alloy::primitives::{address, Address};
use degenbot_aave::updater::verify::cleanup_zero_balance_positions_on_conn;
use degenbot_aave::{
    activate_aave_market, deactivate_aave_market, run_aave_update, NoProgress, RunError,
    ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK,
};
use degenbot_db::{ops, DbError, DegenbotDb};
use degenbot_rpc::provider::AlloyProvider;

use crate::block::{parse_to_block, resolve_to_block};
use crate::cancel::CancelHandle;
use crate::context::CliContext;
use crate::error::CliError;
use crate::prompt::{PromptPlan, Prompter};
use crate::report::{
    AavePositionLine, AaveReport, AaveUpdateEntry, AaveUpdateOutcome, DeactivateOutcome,
};
use degenbot_db::AaveMarketPurgeCount;

/// The RPC retry budget the update arm's per-run transport build uses
/// (mirrors `crate::pool`'s budget).
const RPC_MAX_RETRIES: u32 = 5;

/// An Aave V3 deployment (the Python `aave/deployments.py` constants).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AaveDeployment {
    /// The chain id.
    pub chain_id: u64,
    /// The human chain label.
    pub chain_label: &'static str,
    /// The on-chain `getMarketId()` return — the name the auto-registration
    /// seam keys the bare inactive row on (and `aave activate` resolves via
    /// RPC; the two must match for the completion path to reuse the row).
    pub market_name: &'static str,
    /// The `PoolAddressProvider` contract.
    pub pool_address_provider: Address,
    /// The chain's GHO token.
    pub gho_token_address: Address,
}

/// The shipped Aave V3 deployments (Ethereum mainnet only, mirroring Python).
pub const AAVE_DEPLOYMENTS: &[AaveDeployment] = &[AaveDeployment {
    chain_id: 1,
    chain_label: "Ethereum",
    // The on-chain `getMarketId()` return — the static name the auto-
    // registration seam keys the bare inactive row on (the RPC-fetched name
    // `aave activate` resolves must match for the completion path to reuse
    // the row instead of creating a second one).
    market_name: "Aave Ethereum Market",
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
    /// Purge one market's populated state, then take the cold-boot path a
    /// fresh empty database takes for it.
    Reset {
        /// The chain id.
        chain_id: u64,
        /// The `aave_v3_markets.name` to reset; `None` resolves the chain's
        /// only registered market.
        market_name: Option<String>,
        /// Preview the purge's per-relation counts and re-init without writing.
        dry_run: bool,
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
        AaveCommand::Reset {
            chain_id,
            market_name,
            dry_run,
        } => reset(ctx, cancel, *chain_id, market_name.as_deref(), *dry_run),
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
    let database_path = ctx.database_path()?.value;
    let rpc_url = ctx.node_request_uri_for(chain_id)?.value;
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
    let database_path = ctx.database_path()?.value;
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
    let database_path = ctx.database_path()?.value;
    // Self-serve registration: every supported Aave market not found in the
    // DB registers inactive (a bare row awaiting `aave activate`), so the
    // update never depends on prior CREATEs.
    crate::registrations::ensure_supported_registrations(&database_path)?;
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
        let rpc_url = ctx.node_request_uri_for(chain_unsigned)?.value;
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
            // The thin live-provider wrapper (ADR-068 D5): one build per
            // market run on the shared runtime — the same transport count the
            // core's old internal `AlloyProvider::new` site produced — and
            // the core only injects it. A build failure surfaces as the same
            // `CliError::AaveUpdate` the run's own failures use.
            let run = match crate::pool::shared_runtime_block_on(async {
                AlloyProvider::new(&rpc_url, RPC_MAX_RETRIES).await
            }) {
                Ok(Ok(provider)) => run_aave_update(
                    &database_path,
                    chain,
                    market.id,
                    resolved,
                    args.chunk_size,
                    provider,
                    cancel.flag(),
                    Arc::new(NoProgress),
                    args.verify_chunk,
                    interval,
                    args.verify_all,
                    max_chunks,
                ),
                Ok(Err(err)) => Err(RunError::from(err)),
                Err(cli_err) => return Err(cli_err),
            };
            match run {
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

/// The re-init's chunk size — the same cold-boot default `aave update` uses,
/// so a reset's re-population commits under the chunk-atomicity contract.
const RESET_CHUNK_SIZE: u64 = crate::block::DEFAULT_CHUNK_SIZE;

/// An exclusive, reset-scoped advisory lock over one database path.
///
/// The lock is a file created with `create_new` beside the database, holding
/// the owner's pid. It guards reset-against-reset on a shared database: a
/// second reset (or a crashed first one, whose file survives) sees the file and
/// is refused with the path, rather than racing a purge against an in-flight
/// re-init. The updater arms do not take this lock, so it does not by itself
/// exclude a concurrent `aave update`; the purge's own transaction is what keeps
/// a racing writer from observing a partially-purged market.
struct ResetLock {
    path: std::path::PathBuf,
}

impl ResetLock {
    /// Take the lock, or report the holder's file path.
    fn acquire(database_path: &std::path::Path, market_id: i64) -> Result<Self, CliError> {
        let mut path = database_path.to_path_buf();
        let stem = database_path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        path.set_file_name(format!("{stem}.aave-reset.lock"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                use std::io::Write as _;
                let _ = writeln!(file, "{}", std::process::id());
                Ok(Self { path })
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(CliError::AaveMarketMidUpdate {
                    market_id,
                    lock_path: path.display().to_string(),
                })
            }
            Err(err) => Err(CliError::Io(err)),
        }
    }
}

impl Drop for ResetLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// `aave reset`.
///
/// Resolves the market the way `deactivate` does, reads and prints the purge's
/// per-relation counts, purges under ONE transaction, then re-runs the
/// cold-boot update path for that market.
#[expect(clippy::too_many_lines)]
fn reset(
    ctx: &CliContext<'_>,
    cancel: &CancelHandle,
    chain_id: u64,
    market_name: Option<&str>,
    dry_run: bool,
) -> Result<AaveReport, CliError> {
    let database_path = ctx.database_path()?.value;
    let chain = i64::try_from(chain_id)
        .map_err(|_| CliError::InvalidArgument(format!("chain id {chain_id} is out of range")))?;
    let market = {
        let db = (DegenbotDb::open(&database_path)?).0;
        if let Some(name) = market_name {
            db.fetch_aave_market_by_name(chain, name)?
        } else {
            // No name given: the chain's shipped deployment names the market.
            let name = resolve_aave_deployment(chain_id)?.market_name;
            db.fetch_aave_market_by_name(chain, name)?
        }
    };
    let Some(market) = market else {
        let requested = match market_name {
            Some(name) => name.to_string(),
            None => resolve_aave_deployment(chain_id)
                .map_or_else(|_| "<unknown>".to_string(), |d| d.market_name.to_string()),
        };
        return Err(CliError::UnknownAaveMarket {
            chain_id,
            market_name: requested,
        });
    };

    // The plan is read first and reported, so the operator sees the blast radius
    // before the purge. The counts come off a read-only handle.
    let counts: Vec<AaveMarketPurgeCount> = {
        let db = (DegenbotDb::open(&database_path)?).0;
        db.count_aave_market_rows(market.id)?
    };
    for count in &counts {
        tracing::info!(
            chain_id,
            market_id = market.id,
            table = count.table,
            rows = count.rows,
            dry_run,
            "aave reset plan"
        );
    }
    if dry_run {
        return Ok(AaveReport::Reset {
            chain_id,
            market_id: market.id,
            market_name: market.name,
            counts,
            dry_run: true,
            reinit: None,
        });
    }

    // Taken BEFORE the purge and held across the re-init, so two resets of this
    // database cannot interleave a purge with this run's chunk loop.
    let _lock = ResetLock::acquire(&database_path, market.id)?;

    // The rewind block is the shipped deployment's bootstrap block. Every
    // market `resolve_aave_deployment` can name is that deployment, so the
    // rewind lands where `aave activate` would stamp a fresh market.
    let removed = {
        let (db, _state) = DegenbotDb::open_for_writes(&database_path)?;
        let mut guard = db.lock();
        let tx = guard.transaction().map_err(DbError::from)?;
        let removed = DegenbotDb::reset_aave_market_on_conn(
            &tx,
            market.id,
            ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK,
        )?;
        tx.commit().map_err(DbError::from)?;
        removed
    };

    // Re-init: the SAME cold-boot path an empty database takes for this market —
    // the chunk loop's bootstrap pass resolves the pool/configurator contracts
    // from the address provider over the bootstrap window, then the loop's first
    // chunk writes the market's initial state. The purge rewound
    // `last_update_block`, so the loop starts where a fresh activation does.
    let rpc_url = ctx.node_request_uri_for(chain_id)?.value;
    let provider = match crate::pool::shared_runtime_block_on(async {
        AlloyProvider::new(&rpc_url, RPC_MAX_RETRIES).await
    }) {
        Ok(Ok(provider)) => provider,
        Ok(Err(err)) => return Err(CliError::AaveUpdate(RunError::from(err))),
        Err(cli_err) => return Err(cli_err),
    };
    let reinit = match run_aave_update(
        &database_path,
        chain,
        market.id,
        None,
        RESET_CHUNK_SIZE,
        provider,
        cancel.flag(),
        Arc::new(NoProgress),
        false,
        None,
        false,
        None,
    ) {
        Ok(report) => AaveUpdateOutcome::Advanced {
            from_block: report.from_block,
            to_block: report.to_block,
            chunks_committed: report.chunks_committed,
            total_events_applied: report.total_events_applied,
        },
        Err(RunError::Cancelled) => AaveUpdateOutcome::Cancelled,
        Err(err) => return Err(CliError::AaveUpdate(err)),
    };
    tracing::info!(
        chain_id,
        market_id = market.id,
        "reset Aave V3 market and re-ran the cold-boot update"
    );
    Ok(AaveReport::Reset {
        chain_id,
        market_id: market.id,
        market_name: market.name,
        counts: removed,
        dry_run: false,
        reinit: Some(reinit),
    })
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
    let database_path = ctx.database_path()?.value;
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
