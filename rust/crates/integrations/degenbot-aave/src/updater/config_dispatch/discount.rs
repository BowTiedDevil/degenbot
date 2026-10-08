//! The GHO-discount channel.
//!
//! The per-tx discount snapshot pre-pass ([`build_discount_snapshot`], the
//! 3-way hybrid: DB cache → RPC `getDiscountPercent` (pre-deprecation only)
//! → V4+ deprecation zero), the three discount-config event dispatchers with
//! their read-your-own-writes fresh-resolution wrappers, the stkAAVE
//! transfer channel (the canonical + only stkAAVE balance-mutation path)
//! with the NULL-balance backfill, + the post-apply discount refresh.
//!
//! The pre-pass consumes the vToken revision its caller re-resolved per
//! transaction (stage 1 of `ChunkContext::dispatch_transaction`) — the W6
//! pin: with the deprecation applied, the snapshot must serve the applied
//! revision + the recorded corpus must carry ZERO `getDiscountPercent`
//! entries (a chunk-start revision snapshot produces exactly the mutated
//! replay's RPC shape).
//!
//! Surface table: `tests/fixtures/cassettes/wave4/UPGRADE-MAP.md` §(A)
//! row 7 (consumer side of the per-tx revision re-resolve) + §(C) W6 + the
//! deprecation negative probe.

#![expect(clippy::missing_errors_doc, clippy::doc_markdown)]

use std::collections::{HashMap, HashSet};

use crate::gho_processor::calculate_gho_discount_rate;
use crate::ray_mul;
use crate::run::AaveChunkEvent;
use crate::updater::run::substrate::ChunkSubstrate;
use alloy::primitives::{Address, Bytes, U256};
use degenbot_db::aave::AaveGhoAsset;
use degenbot_db::{DbError, DebtPositionRefreshContext, DegenbotDb};
use degenbot_rpc::provider::AlloyProvider;
use rusqlite::{Connection, OptionalExtension as _};

use super::{
    checksum, discount_to_i64, encode_single_address_call, eth_calls_batched_or_direct,
    word0_to_u256, ConfigDispatchError, GHO_DISCOUNT_DEPRECATION_REVISION,
};

/// `DiscountPercentUpdated(user, old, new)` →
/// [`AaveChunkEvent::GhoDiscountPercentUpdated`]. Mirrors
/// `_process_discount_percent_updated_event` (`newDiscountPercent` is topic[2]
/// — the decoder already extracted it).
pub fn dispatch_discount_percent_updated(
    market_id: i64,
    block_number: u64,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3DiscountPercentUpdatedEvent,
    conn: &Connection,
    substrate: &mut ChunkSubstrate,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let user_str = checksum(&decoded.user);
    let user_id = substrate.user_id_or_create(conn, market_id, &user_str, 0)?;
    let _ = block_number;
    Ok(AaveChunkEvent::GhoDiscountPercentUpdated {
        user_id,
        new_discount_percent: discount_to_i64(decoded.new_discount_percent),
    })
}

/// Discount-token (stkAAVE) ERC20 `Transfer(from, to, value)` →
/// [`AaveChunkEvent::StkAaveTransfer`] — the canonical + only stkAAVE
/// balance-mutation channel. Mirrors `stkaave.process_stk_aave_transfer_event`
/// EXACTLY: processes EVERY Transfer event (including both zero-leg arms),
/// treating `from == ZERO_ADDRESS` / `to == ZERO_ADDRESS` as a half-event
/// (skip the zero side, always mutate the real user) via `Option<i64>`
/// user_ids that are `None` iff the corresponding address is `ZERO_ADDRESS`.
///
/// Scoping:
/// - Returns `None` if the emitter is NOT `gho_asset.v_gho_discount_token` —
///   aToken/vToken/scaled-token Transfers are handled by the ops path
///   (`process_transaction`); only the stkAAVE discount token is this fn's
///   scope (matches the Python `assert contract_address == discount_token`).
///
/// Crash #3: the prior design dedupe-skipped the zero-leg
/// here + processed the paired `Staked`/`Redeem` semantic events via separate
/// dispatch fns. That required an empirically-falsified invariant — every
/// zero-leg Transfer must pair with a semantic event. Some actions emit ONLY
/// the `Transfer(X→0)` event with no paired `Redeem` (verified via cast logs
/// across the 16.59M→18M range), leaving the sender's `stk_aave_balance`
/// stuck at its pre-burn cache value → wrong `calculate_gho_discount_rate`
/// → GHO-burn delta overshoots `prev_scaled_balance` → crashes. The dispatch
/// handlers are now retired (the Python never processed Staked/Redeem for
/// balance mutation — they're fetched only for classification in
/// `fetch_stk_aave_events`); all balance mutation flows through this fn now.
pub fn dispatch_stk_aave_transfer(
    market_id: i64,
    block_number: u64,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3Erc20TransferEvent,
    gho_asset: Option<&AaveGhoAsset>,
    conn: &Connection,
    substrate: &mut ChunkSubstrate,
) -> Result<Option<AaveChunkEvent>, ConfigDispatchError> {
    let _ = block_number;
    // Scope: only the discount-token (stkAAVE) emitter.
    let Some(g) = gho_asset else {
        return Ok(None);
    };
    let Some(token_str) = g.v_gho_discount_token.as_deref() else {
        return Ok(None);
    };
    let Ok(discount_token) = token_str.parse::<Address>() else {
        return Ok(None);
    };
    if decoded.token_address != discount_token {
        return Ok(None); // not a stkAAVE Transfer → the ops path handles it.
    }
    // Matches the Python `if from_address == to_address: return`.
    if decoded.from == decoded.to {
        return Ok(None);
    }
    // Half-event handling for the zero-leg: skip the
    // ZERO_ADDRESS side, always resolve + mutate the real user. Mirrors
    // `process_stk_aave_transfer_event` — the Python skips ZERO_ADDRESS
    // entirely (`from_user=None`/`to_user=None` collapses to a no-op on that
    // side). The prior design dedupe-skipped the zero-leg here + processed the
    // paired Staked/Redeem semantic events (those dispatch fns are now
    // retired); that required an empirically-falsified invariant — every
    // zero-leg Transfer must pair with a semantic event. Some actions emit
    // ONLY the `Transfer(X→0)` event with no paired `Redeem` (verified via
    // cast logs across the 16.59M→18M range), leaving the sender's balance
    // stuck at the pre-burn cache value → wrong `calculate_gho_discount_rate`
    // → GHO-burn delta overshoots `prev_scaled_balance` → crash #3.
    let from_user_id = if decoded.from == Address::ZERO {
        None // the mint-from-zero leg — skipped at apply.
    } else {
        Some(substrate.user_id_or_create(conn, market_id, &checksum(&decoded.from), 0)?)
    };
    let to_user_id = if decoded.to == Address::ZERO {
        None // the burn-to-zero leg — skipped at apply.
    } else {
        Some(substrate.user_id_or_create(conn, market_id, &checksum(&decoded.to), 0)?)
    };
    // The degenerate `Transfer(0x0 → 0x0)` lands both `None` (no real user) —
    // still emitted (counts as processed) but the apply is a no-op.
    Ok(Some(AaveChunkEvent::StkAaveTransfer {
        from_user_id,
        to_user_id,
        amount: decoded.value,
    }))
}

/// [`dispatch_stk_aave_transfer`] wrapper that re-resolves the GHO asset
/// FRESH from `conn` AND pre-apply backfills each side's `stk_aave_balance`
/// from on-chain `balanceOf(user)` at `block_number - 1` when the column is
/// `NULL`. Crash #4: the per-tx `gho_asset` snapshot is taken once
/// before `dispatch_config_events`'s per-event loop; a same-tx
/// `DiscountTokenUpdated` at an earlier logIndex bumps `v_gho_discount_token`
/// AFTER the snapshot, so a stale `None` snapshot would make dispatch skip the
/// matched transfer (the sibling per-chunk refresh of
/// `spec.stk_aave_address` handles cross-chunk staleness from the cold-boot
/// `NULL`; this handles same-tx staleness). The pre-apply backfill mirrors
/// Python's `get_or_init_stk_aave_balance` (stkaave.py:116-122): without it,
/// `apply_stk_aave_transfer_on_conn` treats `NULL` as `U256::ZERO` and applies
/// `0 ± amount`, omitting the pre-event on-chain balance. For the crash
/// user's mint-then-burn pattern, `burn_value == on_chain_initial +
/// mint_value` — without the `on_chain_initial` backfill, the burn apply
/// underflows → chunk-rollback → wrong balance → GHO-burn overshoot crash
/// (byte-exact magnitude). The backfill is a no-op when the column is already
/// non-`NULL` (set by a prior same-tx Transfer or the (C) refresh).
pub(crate) async fn dispatch_stk_aave_transfer_with_backfill(
    provider: &AlloyProvider,
    conn: &Connection,
    market_id: i64,
    block_number: u64,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3Erc20TransferEvent,
    gho_asset: Option<&AaveGhoAsset>,
    substrate: &mut ChunkSubstrate,
) -> Result<Option<AaveChunkEvent>, ConfigDispatchError> {
    // None-guard: no GHO market row at tx start — skip (a chain-wide fetch can
    // surface events from chains without a seeded GHO market).
    if gho_asset.is_none() {
        return Ok(None);
    }
    // Re-resolve gho_asset FRESH from conn (read-your-own-writes — a same-tx
    // DiscountTokenUpdated at an earlier logIndex bumps v_gho_discount_token
    // AFTER the per-tx snapshot was taken). Mirrors
    // `dispatch_discount_token_updated_with_fresh_resolution`.
    let fresh = substrate.gho_asset(conn)?;
    let Some(g) = fresh.as_ref() else {
        return Ok(None);
    };
    // Re-use the sync dispatch fn for the emitter-validation guard +
    // StkAaveTransfer emission (skip the re-resolution since we just did it).
    let event = dispatch_stk_aave_transfer(
        market_id,
        block_number,
        decoded,
        Some(g),
        conn,
        &mut *substrate,
    )?;
    let Some(AaveChunkEvent::StkAaveTransfer {
        from_user_id,
        to_user_id,
        amount: _,
    }) = event
    else {
        return Ok(event);
    };
    // Pre-apply backfill of either side when the column is `NULL` — mirrors
    // Python's `get_or_init_stk_aave_balance` at `block_number - 1`.
    let discount_token: Address = match g
        .v_gho_discount_token
        .as_deref()
        .and_then(|s| s.parse().ok())
    {
        Some(addr) => addr,
        None => {
            return Ok(Some(AaveChunkEvent::StkAaveTransfer {
                from_user_id,
                to_user_id,
                amount: decoded.value,
            }));
        }
    };
    if let Some(uid) = from_user_id {
        backfill_user_stk_aave_balance_if_none(provider, conn, uid, block_number, discount_token)
            .await?;
    }
    if let Some(uid) = to_user_id {
        backfill_user_stk_aave_balance_if_none(provider, conn, uid, block_number, discount_token)
            .await?;
    }
    Ok(Some(AaveChunkEvent::StkAaveTransfer {
        from_user_id,
        to_user_id,
        amount: decoded.value,
    }))
}

/// RPC-fetch `balanceOf(user)` at `block_number - 1` and store via
/// [`DegenbotDb::set_user_stk_aave_balance_on_conn`] when the user's
/// `stk_aave_balance` column is `NULL`. Mirrors Python's
/// `get_or_init_stk_aave_balance` (stkaave.py:24-39) — a `NULL` column means
/// the user was created by an earlier event but never touched by a stkAAVE
/// `Staked`/`Transfer` event YET; the on-chain value at the previous block is
/// the authoritative balance before any current-block events. No-op when the
/// column is already populated (the apply will mutate the cached value).
pub(crate) async fn backfill_user_stk_aave_balance_if_none(
    provider: &AlloyProvider,
    conn: &Connection,
    user_id: i64,
    block_number: u64,
    discount_token: Address,
) -> Result<(), ConfigDispatchError> {
    let row: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT address, stk_aave_balance FROM aave_v3_users WHERE id = ?1",
            [user_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(DbError::Sqlite)?;
    let Some((addr_str, balance_str)) = row else {
        return Err(ConfigDispatchError::DecodeShape(format!(
            "aave_v3_users id={user_id} (backfill target — row vanished)"
        )));
    };
    // Non-NULL → caller (apply_stk_aave_transfer_*) will use the cached value.
    if balance_str.is_some() {
        return Ok(());
    }
    let user_addr: Address = addr_str.parse().map_err(|_| {
        ConfigDispatchError::DecodeShape(format!("bad user_address in backfill: {addr_str}"))
    })?;
    let calldata = encode_single_address_call("balanceOf(address)", &user_addr);
    let ret = provider
        .eth_call(
            &discount_token,
            calldata,
            Some(block_number.saturating_sub(1)),
        )
        .await?;
    let balance = word0_to_u256(&ret).unwrap_or(alloy::primitives::U256::ZERO);
    DegenbotDb::set_user_stk_aave_balance_on_conn(conn, user_id, balance)?;
    Ok(())
}

/// `DiscountTokenUpdated(old, new)` → [`AaveChunkEvent::GhoDiscountTokenUpdated`].
/// Mirrors `_process_discount_token_updated_event`.
///
/// # Emitter-validation guard
///
/// `fetch_discount_config_logs` is chain-wide (no address filter — the events
/// emit from any contract). The load-bearing Python guard
/// `if gho_asset.v_token is None or gho_asset.v_token.address != event["address"]:
/// return` filters spurious chain-wide discount events; this dispatch mirrors
/// it: returns `Ok(None)` when the GHO vToken FK is missing OR the decoded
/// emitter (`decoded.v_token_address` = `log.address()`) doesn't match
/// `gho_asset.v_token_address`. The apply fn never sees the skip.
pub fn dispatch_discount_token_updated(
    gho_asset: &AaveGhoAsset,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3DiscountTokenUpdatedEvent,
) -> Result<Option<AaveChunkEvent>, ConfigDispatchError> {
    if gho_asset.v_token_address.as_deref() != Some(&checksum(&decoded.v_token_address)) {
        return Ok(None);
    }
    Ok(Some(AaveChunkEvent::GhoDiscountTokenUpdated {
        gho_token_id: gho_asset.id,
        new_discount_token: Some(checksum(&decoded.new_discount_token)),
    }))
}

/// `DiscountRateStrategyUpdated(old, new)` →
/// [`AaveChunkEvent::GhoDiscountRateStrategyUpdated`]. Mirrors
/// `_process_discount_rate_strategy_updated_event`.
///
/// Same emitter-validation guard as [`dispatch_discount_token_updated`] — see
/// its docs; the chain-wide fetch + the Python's `event["address"]` guard
/// apply identically (divergence #2).
pub fn dispatch_discount_rate_strategy_updated(
    gho_asset: &AaveGhoAsset,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3DiscountRateStrategyUpdatedEvent,
) -> Result<Option<AaveChunkEvent>, ConfigDispatchError> {
    if gho_asset.v_token_address.as_deref() != Some(&checksum(&decoded.v_token_address)) {
        return Ok(None);
    }
    Ok(Some(AaveChunkEvent::GhoDiscountRateStrategyUpdated {
        gho_token_id: gho_asset.id,
        new_strategy: Some(checksum(&decoded.new_strategy)),
    }))
}

/// [`dispatch_discount_token_updated`] wrapper that re-resolves the GHO vToken
/// address FRESH from `conn` before the emitter-validation guard
/// (read-your-own-writes — a same-tx `ReserveInitialized` at an earlier
/// logIndex links `v_token_id` AFTER the per-tx `gho_asset` snapshot was
/// taken, so the snapshot's `v_token_address` is stale `None` → the guard
/// would skip the INITIAL-set event where `old=None→stkAAVE`). Mirrors
/// Python's live ORM (`gho_asset.v_token` lazy-loads the just-applied FK).
/// The `gho_asset` param is the None-guard (no GHO market row at tx start
/// = skip).
pub(super) fn dispatch_discount_token_updated_with_fresh_resolution(
    gho_asset: Option<&AaveGhoAsset>,
    ev: &degenbot_decoders::aave_event_decoder::AaveV3DiscountTokenUpdatedEvent,
    conn: &Connection,
    substrate: &mut ChunkSubstrate,
) -> Result<Option<AaveChunkEvent>, ConfigDispatchError> {
    // None-guard: no GHO market row at tx start = skip (the chain-wide fetch
    // can surface events from chains without a seeded GHO market).
    if gho_asset.is_none() {
        return Ok(None);
    }
    // Re-resolve FRESH from conn (read-your-own-writes — a same-tx
    // `ReserveInitialized` at an earlier logIndex links `v_token_id` AFTER the
    // per-tx snapshot was taken, so the snapshot's `v_token_address` is stale
    // `None` → the guard would skip the INITIAL-set event where
    // `old=None→stkAAVE`). Mirrors Python's live ORM (`gho_asset.v_token`
    // lazy-loads the just-applied FK). The discount-config events are rare
    // (a handful over the whole drive), so the per-event conn lookup is
    // negligible.
    let fresh = substrate.gho_asset(conn)?;
    match fresh.as_ref() {
        Some(g) => dispatch_discount_token_updated(g, ev),
        None => Ok(None),
    }
}

/// [`dispatch_discount_rate_strategy_updated`] wrapper with the same
/// read-your-own-writes re-resolution as
/// [`dispatch_discount_token_updated_with_fresh_resolution`] — see its docs.
pub(super) fn dispatch_discount_rate_strategy_updated_with_fresh_resolution(
    gho_asset: Option<&AaveGhoAsset>,
    ev: &degenbot_decoders::aave_event_decoder::AaveV3DiscountRateStrategyUpdatedEvent,
    conn: &Connection,
    substrate: &mut ChunkSubstrate,
) -> Result<Option<AaveChunkEvent>, ConfigDispatchError> {
    if gho_asset.is_none() {
        return Ok(None);
    }
    let fresh = substrate.gho_asset(conn)?;
    match fresh.as_ref() {
        Some(g) => dispatch_discount_rate_strategy_updated(g, ev),
        None => Ok(None),
    }
}

// ── the discount pre-pass (the 3-way hybrid) ───────────────────────────────

/// Build the per-tx GHO discount snapshot — the `HashMap<Address, U256>` that
/// `process_transaction` (C3) consumes as the `discounts` parameter. Mirrors
/// the Python `_process_transaction` L140-210 pre-pass: scan the tx's GHO
/// vToken Mint/Burn events, + for each user, resolve the START discount (the
/// value in effect at tx start, BEFORE any `DISCOUNT_PERCENT_UPDATED` in this
/// tx — the `GhoDiscountContext` applies the in-tx overrides via
/// `effective_discount`).
///
/// # The 3-way hybrid
///
/// - **path #1 (DB-cache):** the user exists in `aave_v3_users` → read
///   `gho_discount` (the snapshot at tx start). Mirrors the Python
///   `gho_users.get(user_address)` → `user.gho_discount`.
/// - **path #2 (RPC):** the user is NOT in the DB → `getDiscountPercent(user)`
///   `eth_call` at the tx's `block_number` (the user has an existing GHO debt
///   position first encountered in this chunk's tx range). Mirrors the
///   Python `raw_call(getDiscountPercent)`. Only when `vtoken_revision < 4`.
/// - **path #3 (V4+ deprecation):** `vtoken_revision >= 4` → the discount
///   mechanism is deprecated → 0. Mirrors the Python
///   `is_discount_supported(session, market)` gate (revision < 4).
///
/// Returns an empty map when there's no GHO vToken (`gho_vtoken_address` is
/// `None`) or the revision couldn't be resolved.
pub async fn build_discount_snapshot(
    provider: &AlloyProvider,
    tx_logs: &[&alloy::rpc::types::Log],
    block_number: u64,
    gho_vtoken_address: Option<Address>,
    vtoken_revision: Option<u32>,
    market_id: i64,
    conn: &Connection,
) -> Result<HashMap<Address, U256>, ConfigDispatchError> {
    use degenbot_decoders::aave_event_decoder::{AAVE_BURN_TOPIC, AAVE_MINT_TOPIC};

    let Some(vtoken_addr) = gho_vtoken_address else {
        return Ok(HashMap::new());
    };
    let Some(rev) = vtoken_revision else {
        return Ok(HashMap::new());
    };
    let discount_supported = rev < GHO_DISCOUNT_DEPRECATION_REVISION;

    let mut snapshot: HashMap<Address, U256> = HashMap::new();
    // path #2 is DEFERRED: the users the DB cache cannot answer collect here
    // (first-encounter order) and resolve in ONE transport pass after the
    // scan. The DB reads keep their per-log position + order; nothing writes
    // to `conn` between them, so the deferral cannot observe different DB
    // state — the only change is the transport shape (2+ same-block
    // `getDiscountPercent` calls fold into one Multicall3 `aggregate3`; a
    // single pending user keeps the exact sequential request shape).
    let mut rpc_users: Vec<Address> = Vec::new();
    let mut rpc_pending: HashSet<Address> = HashSet::new();
    for log in tx_logs {
        let topics = log.topics();
        if topics.is_empty() || log.address() != vtoken_addr {
            continue;
        }
        let user = if topics[0] == AAVE_MINT_TOPIC && topics.len() >= 3 {
            // Mint: topics[2] = onBehalfOf (the user).
            topic_to_address(topics[2])
        } else if topics[0] == AAVE_BURN_TOPIC && topics.len() >= 2 {
            // Burn: topics[1] = from (the user).
            topic_to_address(topics[1])
        } else {
            continue;
        };
        if snapshot.contains_key(&user) || rpc_pending.contains(&user) {
            continue; // first-encounter wins (matches the Python `if user_address not in user_discounts`)
        }
        // path #1: DB-cache.
        let user_str = checksum(&user);
        if let Some(db_discount) =
            DegenbotDb::fetch_user_gho_discount_by_address_on_conn(conn, market_id, &user_str)?
        {
            snapshot.insert(user, U256::from(u64::try_from(db_discount).unwrap_or(0)));
            continue;
        }
        // path #3: V4+ → 0 (must check before path #2 — the Python gates path
        // #2 with `is_discount_supported`).
        if !discount_supported {
            snapshot.insert(user, U256::ZERO);
            continue;
        }
        // path #2: deferred — collect the user; the batched read below.
        rpc_pending.insert(user);
        rpc_users.push(user);
    }
    // path #2 resolution: one transport pass over the deferred users. Every
    // call is independent (same target, same block; no call's result gates
    // another), so the batch is semantically identical to the sequential
    // reads; a failed sub-call is LOUD (a hard error), never a silent zero
    // default.
    let calldatas: Vec<(Address, Bytes)> = rpc_users
        .iter()
        .map(|user| {
            (
                vtoken_addr,
                encode_single_address_call("getDiscountPercent(address)", user),
            )
        })
        .collect();
    let rets = eth_calls_batched_or_direct(provider, &calldatas, block_number).await?;
    for (user, ret) in rpc_users.iter().zip(rets) {
        let discount = word0_to_u256(&ret).unwrap_or(U256::ZERO);
        snapshot.insert(*user, discount);
    }
    Ok(snapshot)
}

/// Decode the first topic word as an `Address` (the low 20 bytes).
fn topic_to_address(topic: alloy::primitives::B256) -> Address {
    Address::from_slice(&topic.as_slice()[12..])
}

// ── C3.3: the (C) discount-refresh post-apply pass ───────────────────

/// The (C) discount-refresh post-apply pass. For one `GhoRefreshDiscount`
/// signal (one V1-V3 GHO mint/burn that set `should_refresh_discount`),
/// recompute `aave_v3_users.gho_discount` from the POST-APPLY debt balance +
/// the user's stkAAVE balance (mirrors Python's `_refresh_discount_rate` +
/// `get_or_init_stk_aave_balance`).
///
/// 1. Read the debt position's `(user_id, user_address, scaled_balance,
///    last_index, stk_aave_balance)` from `conn` — the POST-apply values
///    (the `ScaledTokenMint`/`Burn` apply landed them just before this call).
/// 2. `get_or_init_stk_aave_balance`: if `stk_aave_balance` is `None` (the
///    user never touched by a stkAAVE `Staked`/`Transfer` yet — the 890
///    `None` + the 1042 missing), `balanceOf(address)` `eth_call` on the
///    discount-token (stkAAVE) at **`block_number - 1`** (Python reads at the
///    PREVIOUS block — the balance check is BEFORE any events in the current
///    block; an off-by-one here causes a divergence class) → SET
///    `aave_v3_users.stk_aave_balance`.
/// 3. `debt_balance = ray_mul(scaled_balance, last_index, None)` (HALF_UP —
///    Python's `math_libs.ray_mul` default; matches the byte-exact
///    `accrue_debt_on_action` path).
/// 4. `gho_discount = calculate_gho_discount_rate(debt_balance,
///    stk_aave_balance)` → `i64`.
/// 5. Write `aave_v3_users.gho_discount` (the DB-cache path #1 of
///    `build_discount_snapshot` reads this on the next tx → correct discount
///    → GHO debt accrual converges).
///
/// `discount_token` is the chain's (freshly-per-tx-resolved)
/// `v_gho_discount_token` — `None` (no discount token configured) → no-op.
/// `market_id` is unused (the refresh target is `aave_v3_users.id`).
#[expect(clippy::missing_errors_doc)]
pub async fn refresh_gho_discount(
    provider: &AlloyProvider,
    conn: &Connection,
    #[expect(unused_variables)] market_id: i64,
    position_id: i64,
    block_number: u64,
    discount_token: Option<Address>,
) -> Result<(), ConfigDispatchError> {
    let Some(discount_token) = discount_token else {
        return Ok(());
    };
    // 1. POST-apply debt-position context.
    let ctx: DebtPositionRefreshContext =
        DegenbotDb::lookup_debt_position_refresh_context_on_conn(conn, position_id)?;
    // 2. get_or_init_stk_aave_balance (balanceOf at block-1 if None).
    let stk_aave_balance = if let Some(b) = ctx.stk_aave_balance {
        b
    } else {
        let user_addr: Address = ctx.user_address.parse().map_err(|_| {
            ConfigDispatchError::DecodeShape(format!(
                "bad user_address in refresh: {}",
                ctx.user_address
            ))
        })?;
        let calldata = encode_single_address_call("balanceOf(address)", &user_addr);
        let ret = provider
            .eth_call(
                &discount_token,
                calldata,
                Some(block_number.saturating_sub(1)),
            )
            .await?;
        let balance = word0_to_u256(&ret).unwrap_or(U256::ZERO);
        DegenbotDb::set_user_stk_aave_balance_on_conn(conn, ctx.user_id, balance)?;
        balance
    };
    // 3. debt_balance = ray_mul(scaled, last_index) — HALFUP (Python default).
    let debt_index = ctx.last_index.unwrap_or(U256::ZERO);
    let debt_balance = ray_mul(ctx.scaled_balance, debt_index, None).map_err(|e| {
        ConfigDispatchError::DecodeShape(format!("ray_mul overflow in refresh: {e}"))
    })?;
    // 4. + 5. compute + write gho_discount.
    let rate = calculate_gho_discount_rate(debt_balance, stk_aave_balance).map_err(|e| {
        ConfigDispatchError::DecodeShape(format!("calculate_gho_discount_rate: {e}"))
    })?;
    DegenbotDb::apply_gho_discount_percent_updated_on_conn(
        conn,
        ctx.user_id,
        discount_to_i64(rate),
    )?;
    Ok(())
}
