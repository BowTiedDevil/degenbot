//! The driver's fetch/bootstrap stage: cold-boot resolution of the
//! `POOL`/`POOL_CONFIGURATOR` contract rows from the `ProxyCreated` events,
//! plus the per-run fetch-spec assembly (contract addresses, the
//! scaled-token set, the GHO discount token).

use std::collections::HashMap;

use alloy::primitives::Address;
use degenbot_db::aave::AaveGhoAsset;
use degenbot_db::{DbError, DegenbotDb};
use degenbot_rpc::provider::{AlloyProvider, LogFetcher};

use super::RunError;
use crate::aave_fetch::AaveFetchSpec;
use crate::config_dispatch::{match_proxy_id, ProxyCreationResolution};

/// `POOL`/`POOL_CONFIGURATOR` rows that `build_fetch_spec` requires, the
/// bootstrap pass fetches `ProxyCreated` events from the `POOL_ADDRESS_PROVIDER`
/// over `[from_block, from_block + BOOTSTRAP_WINDOW]` + applies them
/// idempotently. Mainnet lands the bootstrap `ProxyCreated` events at
/// `from_block + 57/+60/+66` (blocks 16291127/16291130/16291136 — `from_block`
/// is the deploy + 1 = 16291071); the 2 000-block window gives ample margin.
/// Non-mainnet markets with a longer deploy→`ProxyCreated` gap may need a
/// larger window (a `BootstrapFailed` error surfaces the miss).
const BOOTSTRAP_WINDOW: u64 = 2_000;

/// Cold-boot the `POOL`/`POOL_CONFIGURATOR` contract rows. On a fresh
/// market, `activate` seeds only the `POOL_ADDRESS_PROVIDER`; `build_fetch_spec`
/// hard-errors without `POOL`/`POOL_CONFIGURATOR`. This pass fetches the
/// `ProxyCreated` events from the `POOL_ADDRESS_PROVIDER` address over the
/// bootstrap window `[from_block, from_block + BOOTSTRAP_WINDOW]`, decodes each
/// via [`match_proxy_id`] (which RPCs the `POOL_REVISION()`/`CONFIGURATOR_REVISION()`
/// on the implementation address), and applies the resolved rows via
/// [`DegenbotDb::apply_contract_inserted_if_absent_on_conn`] (idempotent — so the
/// chunk loop's later re-encounter of the same `ProxyCreated` events is a
/// no-op). No-op on a warm boot (both rows already present). Mirrors the Python
/// `update_aave_market` Phase-1 bootstrap (commands.py:1010-1062).
///
/// # Errors
///
/// Returns [`RunError::NotBootstrapped`] if the `POOL_ADDRESS_PROVIDER` row is
/// missing (the caller must seed it via `activate`), or [`RunError::BootstrapFailed`]
/// if `POOL`/`POOL_CONFIGURATOR` remain missing after the fetch (e.g. the
/// bootstrap window was too small for a non-mainnet market).
pub(super) async fn bootstrap_pool_contracts(
    db: &DegenbotDb,
    provider: &AlloyProvider,
    fetcher: &LogFetcher,
    market_id: i64,
    from_block: u64,
) -> Result<(), RunError> {
    // 1. Read the current contracts; skip if both bootstrap rows are present.
    let contracts = db.fetch_aave_contracts(market_id)?;
    let has_pool = contracts.iter().any(|c| c.name == "POOL");
    let has_configurator = contracts.iter().any(|c| c.name == "POOL_CONFIGURATOR");
    if has_pool && has_configurator {
        return Ok(()); // warm boot — nothing to do.
    }
    let address_provider = contracts
        .iter()
        .find(|c| c.name == "POOL_ADDRESS_PROVIDER")
        .ok_or(RunError::NotBootstrapped(market_id))?;
    let ap_address = address_provider.address;

    // 2. Fetch the PoolAddressProvider's events over the bootstrap window
    //    (async, no DB lock held across the `.await`). The fetch unions all
    //    address-provider topics; we filter to `ProxyCreated` below.
    let boot_end = from_block.saturating_add(BOOTSTRAP_WINDOW);
    let logs =
        crate::aave_fetch::fetch_address_provider_logs(fetcher, from_block, boot_end, ap_address)
            .await?;

    // 3. Decode + resolve each `ProxyCreated` (async RPC for the revision).
    //    `match_proxy_id` returns `None` for non-POOL/non-POOL_CONFIGURATOR ids
    //    (e.g. the `POOL_DATA_PROVIDER` proxy id) — those are skipped (the chunk
    //    loop's `PoolDataProviderUpdated`/`AddressSet` arms handle them).
    let mut resolutions: Vec<ProxyCreationResolution> = Vec::new();
    for log in &logs {
        let Some(degenbot_decoders::aave_event_decoder::DecodedAaveEvent::ProxyCreated(ev)) =
            degenbot_decoders::aave_event_decoder::decode_aave_log(log)
        else {
            continue;
        };
        if let Some(resolved) = match_proxy_id(
            &ev.id,
            &ev.proxy_address,
            &ev.implementation_address,
            provider,
            from_block,
        )
        .await?
        {
            resolutions.push(resolved);
        }
    }

    // 4. Apply the resolved rows idempotently in ONE transaction (so a partial
    //    bootstrap either commits all or none).
    if !resolutions.is_empty() {
        let mut guard = db.lock();
        let tx = guard.transaction().map_err(DbError::from)?;
        for r in &resolutions {
            DegenbotDb::apply_contract_inserted_if_absent_on_conn(
                &tx,
                market_id,
                &r.name,
                &r.address,
                Some(r.revision),
            )?;
        }
        tx.commit().map_err(DbError::from)?;
    }

    // 5. Re-verify: if either bootstrap row is STILL missing, the window was
    //    too small (or the market isn't mainnet-shaped) → surface the miss.
    let contracts2 = db.fetch_aave_contracts(market_id)?;
    let have_pool = contracts2.iter().any(|c| c.name == "POOL");
    let have_cfg = contracts2.iter().any(|c| c.name == "POOL_CONFIGURATOR");
    if !have_pool || !have_cfg {
        return Err(RunError::BootstrapFailed(market_id));
    }
    Ok(())
}

/// Resolve the [`AaveFetchSpec`] for `market_id`: the `POOL`/
/// `POOL_CONFIGURATOR` / `POOL_ADDRESS_PROVIDER` / `PRICE_ORACLE` contract
/// addresses (from `aave_v3_contracts`), the chain's aToken+vToken addresses, + the GHO
/// asset's stkAAVE address. Mirrors the Python `update_aave_market`'s contract
/// + `known_scaled_token_addresses` resolution (commands.py:1008-1058).
///
/// Returns `(spec, gho_asset)` — the `gho_asset` is the chain's GHO token row
/// (`None` for non-GHO markets); the orchestrator passes it to the discount
/// pre-pass + `process_transaction`.
pub(super) fn build_fetch_spec(
    db: &DegenbotDb,
    market_id: i64,
    chain_id: i64,
) -> Result<(AaveFetchSpec, Option<AaveGhoAsset>), RunError> {
    let contracts = db.fetch_aave_contracts(market_id)?;
    // Index by name (the Python `get_contract(market, name)` shape).
    let mut by_name: HashMap<&str, &degenbot_db::rows::AaveV3ContractRow> = HashMap::new();
    for c in &contracts {
        by_name.insert(c.name.as_str(), c);
    }
    let pool = by_name.get("POOL").ok_or_else(|| {
        RunError::Db(DbError::MissingRow(
            "POOL contract row not found for market {market_id}".to_string(),
        ))
    })?;
    let configurator = by_name.get("POOL_CONFIGURATOR").ok_or_else(|| {
        RunError::Db(DbError::MissingRow(
            "POOL_CONFIGURATOR contract row not found".to_string(),
        ))
    })?;
    let address_provider = by_name.get("POOL_ADDRESS_PROVIDER").ok_or_else(|| {
        RunError::Db(DbError::MissingRow(
            "POOL_ADDRESS_PROVIDER contract row not found".to_string(),
        ))
    })?;
    let oracle_address = by_name.get("PRICE_ORACLE").map(|c| c.address);

    let scaled_token_addresses: Vec<Address> = db
        .fetch_aave_scaled_token_addresses(chain_id)?
        .into_iter()
        .filter_map(|s| s.parse::<Address>().ok())
        .collect();

    // The GHO asset (chain-unique) + the stkAAVE address.
    let gho_asset = db.fetch_aave_gho_asset(chain_id)?;
    let stk_aave_address = gho_asset
        .as_ref()
        .and_then(|g| g.v_gho_discount_token.as_deref())
        .and_then(|s| s.parse::<Address>().ok());

    let spec = AaveFetchSpec {
        pool_address: pool.address,
        configurator_address: configurator.address,
        address_provider_address: address_provider.address,
        oracle_address,
        scaled_token_addresses,
        stk_aave_address,
    };
    Ok((spec, gho_asset))
}
