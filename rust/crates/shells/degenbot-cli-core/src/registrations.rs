//! The supported-registration seam (ADR-051 D5 extension): the console no
//! longer requires the operator to CREATE rows. Every supported `(chain, DEX)`
//! pair ([`RETIRED_EXCHANGES`]) and every supported Aave V3 market
//! ([`AAVE_DEPLOYMENTS`]) that is NOT found in the DB is registered
//! **inactive** by [`ensure_supported_registrations`] — the operator then
//! flips rows active with `exchange activate` / `aave activate`.
//!
//! # Where the ensure runs
//!
//! At the three write seams that define the operational lifecycle:
//!
//! 1. `database reset` — a fresh DB is born fully registered.
//! 2. `pool update` — the updater arm self-serves its exchange rows.
//! 3. `aave update` — the updater arm self-serves its market rows.
//!
//! `exchange list` / `aave position show` stay read-only; they render the
//! registered state as of the last write seam.
//!
//! An auto-registered Aave row is BARE (no `POOL_ADDRESS_PROVIDER` / GHO
//! substrate, NULL `last_update_block`); `aave activate` completes it (see
//! `degenbot_aave::activate_aave_market_on_conn`). An auto-registered
//! exchange row is complete by construction (registry factory/deployer
//! identity; the V4 singleton's `pool_managers` row comes with it).

use degenbot_db::DegenbotDb;

use crate::aave::AAVE_DEPLOYMENTS;
use crate::error::CliError;
use crate::exchange::{resolve_deployment, RETIRED_EXCHANGES};

/// How many rows one ensure pass registered (created).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistrationReport {
    /// Newly registered `(chain, DEX)` exchange pairs.
    pub exchanges: usize,
    /// Newly registered Aave V3 markets.
    pub aave_markets: usize,
}

/// Register every supported exchange + Aave market that is not found in the
/// DB at `path`, as an inactive row. Idempotent: pass 2 registers nothing.
///
/// # Errors
///
/// [`CliError`] on a DB failure (the ensure runs INSIDE write arms, so a
/// registration failure fails the arm before any updater work).
pub fn ensure_supported_registrations(
    path: &std::path::Path,
) -> Result<RegistrationReport, CliError> {
    let (db, _state) = DegenbotDb::open_for_writes(path)?;
    let mut report = RegistrationReport {
        exchanges: 0,
        aave_markets: 0,
    };

    for entry in RETIRED_EXCHANGES {
        let chain = i64::try_from(entry.chain_id).map_err(|_| {
            CliError::InvalidArgument(format!("chain id {} is out of range", entry.chain_id))
        })?;
        if db.fetch_exchange_by_name(chain, entry.dex_slug)?.is_some() {
            continue;
        }
        let deployment = resolve_deployment(entry.chain_id, entry.dex_slug)?;
        let row = db.upsert_exchange(
            chain,
            deployment.dex_slug,
            deployment.factory,
            deployment.deployer,
        )?;
        if let Some(manager) = deployment.pool_manager {
            db.upsert_pool_manager(
                manager.address,
                chain,
                deployment.dex_slug,
                Some(manager.state_view),
                row.id,
            )?;
        }
        report.exchanges += 1;
        tracing::info!(
            chain_id = entry.chain_id,
            name = entry.dex_slug,
            "registered supported exchange (inactive)"
        );
    }

    for deployment in AAVE_DEPLOYMENTS {
        let chain = i64::try_from(deployment.chain_id).map_err(|_| {
            CliError::InvalidArgument(format!("chain id {} is out of range", deployment.chain_id))
        })?;
        if db
            .fetch_aave_market_by_name(chain, deployment.market_name)?
            .is_some()
        {
            continue;
        }
        db.register_aave_market(chain, deployment.market_name)?;
        report.aave_markets += 1;
        tracing::info!(
            chain_id = deployment.chain_id,
            name = deployment.market_name,
            "registered supported aave market (inactive)"
        );
    }

    Ok(report)
}
