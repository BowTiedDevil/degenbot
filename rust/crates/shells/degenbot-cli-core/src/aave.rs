//! The `aave` command arms (ADR-051 D1).
//!
//! - `aave activate` — [`activate_aave_market`] (the one-time market seed).
//! - `aave deactivate` — the `(chain, name)` market read + the
//!   [`deactivate_aave_market`] row flip.
//! - `aave update` — the active-market walk over
//!   [`run_aave_update`], with the post-run zero-balance cleanup and the
//!   opt-in completion backup.
//! - `aave position show` — the market/user scalar row reads over the
//!   `degenbot-db::aave` read surface.
//! - `aave reset` — the market-scoped purge and the cold-boot substrate
//!   re-seed committed in ONE transaction through the market-reset module,
//!   so the NEXT update run takes the cold-boot path an empty database
//!   takes.
//! - `aave digest` — one market's completed-market record: per-table row
//!   counts + digests over the market tables, rendered as the manifest
//!   JSON a drive pins.

use std::sync::Arc;

use alloy::primitives::{address, Address};
use degenbot_aave::updater::verify::cleanup_zero_balance_positions_on_conn;
use degenbot_aave::{
    activate_aave_market, deactivate_aave_market, AaveMarketWriter, MarketReset, NoProgress,
    RunError, RunSpec, VerifyPolicy,
};
use degenbot_db::{
    aave_market_digest_for, ops, parse_completed_market_manifest, CompletedMarketDrive,
    CompletedMarketManifest, DbError, DegenbotDb,
};
use degenbot_rpc::provider::AlloyProvider;

use crate::block::{parse_to_block, resolve_to_block};
use crate::cancel::CancelHandle;
use crate::context::CliContext;
use crate::error::CliError;
use crate::prompt::{PromptPlan, Prompter};
use crate::report::{
    AavePositionLine, AaveReinit, AaveReport, AaveUpdateEntry, AaveUpdateOutcome, DeactivateOutcome,
};
use degenbot_db::AaveMarketPurgeCount;

/// The RPC retry budget the update arm's per-run transport build uses
/// (mirrors `crate::pool`'s budget).
const RPC_MAX_RETRIES: u32 = 5;

/// An Aave V3 deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AaveDeployment {
    pub chain_id: u64,
    pub chain_label: &'static str,
    /// The on-chain `getMarketId()` return — the name the auto-registration
    /// seam keys the bare inactive row on (and `aave activate` resolves via
    /// RPC; the two must match for the completion path to reuse the row).
    pub market_name: &'static str,
    pub pool_address_provider: Address,
    pub gho_token_address: Address,
}

/// The shipped Aave V3 deployments (Ethereum mainnet only).
pub const AAVE_DEPLOYMENTS: &[AaveDeployment] = &[AaveDeployment {
    chain_id: 1,
    chain_label: "Ethereum",
    market_name: "Aave Ethereum Market",
    pool_address_provider: address!("2f39d218133AFaB8F2B819B1066c7E434Ad94E9e"),
    gho_token_address: address!("40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f"),
}];

/// The `aave` command group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AaveCommand {
    Activate {
        chain_id: u64,
    },
    Deactivate {
        chain_id: u64,
        /// The `aave_v3_markets.name` to flip.
        market_name: String,
    },
    Digest {
        chain_id: u64,
        /// The `aave_v3_markets.name` to digest; `None` resolves the
        /// chain's only registered market.
        market_name: Option<String>,
        /// A previous completed-market record whose `market.drive` block is
        /// carried into the output verbatim (the drive facts are the
        /// drive's own bookkeeping, not DB state).
        drive_manifest: Option<std::path::PathBuf>,
    },
    Update {
        /// Max blocks per chunk before committing.
        chunk_size: u64,
        /// The widest block range a single `eth_getLogs` request may
        /// span (independent of `chunk_size`).
        max_block_range: u64,
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
    Reset {
        chain_id: u64,
        /// The `aave_v3_markets.name` to reset; `None` resolves the chain's
        /// only registered market.
        market_name: Option<String>,
        /// Preview the purge's per-relation counts and re-init without writing.
        dry_run: bool,
    },
    PositionShow {
        address: String,
        market: String,
        chain_id: u64,
    },
}

impl AaveCommand {
    /// No aave arm prompts.
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
        AaveCommand::Digest {
            chain_id,
            market_name,
            drive_manifest,
        } => digest(
            ctx,
            *chain_id,
            market_name.as_deref(),
            drive_manifest.as_deref(),
        ),
        AaveCommand::Update {
            chunk_size,
            max_block_range,
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
                max_block_range: *max_block_range,
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
    // The thin live-provider wrapper (ADR-068 D5): one build per activation on
    // the shared runtime, and the core only injects it. Mirrors the update
    // arm's per-market build below.
    let result = match crate::pool::shared_runtime_block_on(async {
        AlloyProvider::new(&rpc_url, RPC_MAX_RETRIES).await
    }) {
        Ok(Ok(provider)) => activate_aave_market(
            &database_path,
            chain,
            &deployment.pool_address_provider.to_checksum(None),
            &deployment.gho_token_address.to_checksum(None),
            &provider,
        ),
        Ok(Err(err)) => Err(RunError::from(err)),
        Err(cli_err) => return Err(cli_err),
    }
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

/// `aave digest`.
fn digest(
    ctx: &CliContext<'_>,
    chain_id: u64,
    market_name: Option<&str>,
    drive_manifest: Option<&std::path::Path>,
) -> Result<AaveReport, CliError> {
    let database_path = ctx.database_path()?.value;
    let chain = i64::try_from(chain_id)
        .map_err(|_| CliError::InvalidArgument(format!("chain id {chain_id} is out of range")))?;
    let drive = drive_manifest.map(carried_drive).transpose()?;
    let digest = {
        // The read handle locks down at open (`query_only=on`): the digest
        // arm observes the DB, it never writes it.
        let db = (DegenbotDb::open(&database_path)?).0;
        let conn = db.lock();
        aave_market_digest_for(&conn, Some(chain), market_name)?
    };
    Ok(AaveReport::Digest {
        manifest: CompletedMarketManifest { digest, drive },
    })
}

/// Parse a previous completed-market record with the typed reader and carry
/// its `market.drive` block — the one content the record has that the DB
/// cannot yield.
///
/// The record must be one this writer emitted (the serialization stamp is
/// the version discriminant) and must carry a drive block; a record without
/// one has nothing to carry.
fn carried_drive(path: &std::path::Path) -> Result<CompletedMarketDrive, CliError> {
    let raw = std::fs::read_to_string(path).map_err(CliError::Io)?;
    let manifest = parse_completed_market_manifest(&raw)
        .map_err(|error| CliError::InvalidArgument(format!("{}: {error}", path.display())))?;
    manifest.drive.ok_or_else(|| {
        CliError::InvalidArgument(format!("{} carries no market.drive block", path.display()))
    })
}

#[expect(clippy::struct_excessive_bools)]
struct UpdateArgs<'a> {
    chunk_size: u64,
    max_block_range: u64,
    to_block: &'a str,
    verify_chunk: bool,
    verify_all: bool,
    verify_all_interval: u64,
    stop_after_one_chunk: bool,
    dry_run: bool,
    enable_backup: bool,
}

/// The `aave update` flags' projection onto the run's [`VerifyPolicy`]: the
/// market-wide interval gate is live only when `--verify-all` is set.
impl From<&UpdateArgs<'_>> for VerifyPolicy {
    fn from(args: &UpdateArgs<'_>) -> Self {
        Self {
            verify_chunk: args.verify_chunk,
            verify_all_interval: args.verify_all.then_some(args.verify_all_interval),
            verify_all_at_completion: args.verify_all,
        }
    }
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
            // The market's ONE writer, taken before the transport is built: a
            // market another writer holds is refused without costing an RPC
            // connection.
            let writer = acquire_market_writer(&database_path, market.id)?;
            // The thin live-provider wrapper (ADR-068 D5): one build per
            // market run on the shared runtime, and the core only injects
            // it. A build failure surfaces as the same
            // `CliError::AaveUpdate` the run's own failures use.
            let run = match crate::pool::shared_runtime_block_on(async {
                AlloyProvider::new(&rpc_url, RPC_MAX_RETRIES).await
            }) {
                Ok(Ok(provider)) => writer.update(RunSpec {
                    chain_id: chain,
                    market_id: market.id,
                    to_block: resolved,
                    chunk_size: args.chunk_size,
                    max_block_range: args.max_block_range,
                    provider,
                    cancel: cancel.flag(),
                    progress: Arc::new(NoProgress),
                    verify: VerifyPolicy::from(args),
                    max_chunks,
                }),
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

/// `aave reset`.
///
/// Resolves the market by name, defaulting to the shipped deployment's market
/// when none is given; reads and prints the purge's per-relation counts as the
/// plan, then — under the market's writer lock — purges the market and
/// re-seeds its cold-boot substrate in ONE transaction
/// ([`AaveMarketWriter::reset_to_cold_boot`]): the purge and the activate
/// seam's completion commit together, so nothing can land between them. The
/// report renders the facts that transaction read back. No update run here: the
/// command returns in seconds, and the next `aave update` takes the cold-boot
/// path from the stamped cursor.
fn reset(
    ctx: &CliContext<'_>,
    _cancel: &CancelHandle,
    chain_id: u64,
    market_name: Option<&str>,
    dry_run: bool,
) -> Result<AaveReport, CliError> {
    let database_path = ctx.database_path()?.value;
    let chain = i64::try_from(chain_id)
        .map_err(|_| CliError::InvalidArgument(format!("chain id {chain_id} is out of range")))?;
    // The re-init completes the purged row through the shipped deployment's
    // substrate (the address provider + GHO constants), so the chain must be
    // one the deployment table names — the same precondition `aave
    // activate` has.
    let deployment = resolve_aave_deployment(chain_id)?;
    let market = {
        let db = (DegenbotDb::open(&database_path)?).0;
        // No name given: the chain's shipped deployment names the market.
        let name = market_name.unwrap_or(deployment.market_name);
        db.fetch_aave_market_by_name(chain, name)?
    };
    let Some(market) = market else {
        let requested =
            market_name.map_or_else(|| deployment.market_name.to_string(), ToString::to_string);
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

    // The market's ONE writer: held from here through the reset transaction, so
    // no reset or update of this market anywhere (this CLI, the PyO3 seam, a
    // Rust consumer) can interleave a purge with a re-seed or a chunk with it.
    let writer = acquire_market_writer(&database_path, market.id)?;

    // The module's activation RPC needs a live transport; the shipped
    // deployment constants supply the two addresses it reads (the on-chain
    // market name + the GHO token's metadata). The build is the same thin
    // provider wrapper the activate and update arms use.
    let rpc_url = ctx.node_request_uri_for(chain_id)?.value;
    let provider = match crate::pool::shared_runtime_block_on(async {
        AlloyProvider::new(&rpc_url, RPC_MAX_RETRIES).await
    }) {
        Ok(Ok(provider)) => provider,
        Ok(Err(err)) => return Err(CliError::AaveUpdate(RunError::from(err))),
        Err(cli_err) => return Err(cli_err),
    };

    // ONE open + ONE transaction: the writer purges the market and re-seeds
    // its cold-boot substrate on this same transaction — the rewind block is
    // the shipped deployment's bootstrap block, where `aave activate` stamps a
    // fresh market — and returns the facts it read back from it. A single
    // commit is what removes the old kill-between-transactions window; a
    // failure drops the transaction and the database stays byte-identical. The
    // counts are read on this transaction ahead of the write, so they are the
    // rows the purge removes.
    let MarketReset { removed, facts } = writer
        .reset_to_cold_boot(
            &deployment.pool_address_provider.to_checksum(None),
            &deployment.gho_token_address.to_checksum(None),
            &provider,
        )
        .map_err(map_run_error)?;
    tracing::info!(
        chain_id,
        market_id = market.id,
        cursor = facts.last_update_block.unwrap_or_default(),
        "reset Aave V3 market and re-stamped the cold-boot substrate"
    );
    Ok(AaveReport::Reset {
        chain_id,
        market_id: market.id,
        market_name: market.name,
        counts: removed,
        dry_run: false,
        reinit: Some(AaveReinit {
            market_activated: facts.market_activated,
            contract_row_present: facts.contract_row_present,
            last_update_block: facts
                .last_update_block
                .map(|block| i64::try_from(block).unwrap_or(i64::MAX)),
        }),
    })
}

/// Take `market_id`'s writer for `database_path`.
///
/// The arms hold it across their run, so this CLI is one of the writers the
/// market's lock excludes — never a path around it.
fn acquire_market_writer(
    database_path: &std::path::Path,
    market_id: i64,
) -> Result<AaveMarketWriter, CliError> {
    AaveMarketWriter::acquire(database_path, market_id).map_err(map_run_error)
}

/// The core run error's CLI face: a market another writer holds is the
/// operator-facing mid-update refusal (the writer's lock path included),
/// everything else stays the wrapped core failure.
fn map_run_error(err: RunError) -> CliError {
    match err {
        RunError::MarketHeld {
            market_id,
            lock_path,
        } => CliError::AaveMarketMidUpdate {
            market_id,
            lock_path,
        },
        other => CliError::AaveUpdate(other),
    }
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

/// The `<stem>-<to_block>.db.bak` sibling the completion backup writes.
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
    ops::backup_database(database_path, &backup, &mut |_| true)?;
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
