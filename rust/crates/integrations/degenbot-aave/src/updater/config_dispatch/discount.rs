//! The GHO-discount channel.
//!
//! The per-tx discount snapshot pre-pass ([`build_discount_snapshot`], the
//! 3-way hybrid: DB cache → RPC `getDiscountPercent` (pre-deprecation only)
//! → V4+ deprecation zero), the three discount-config event dispatchers (each
//! reading the GHO asset through the ctx-owned always-fresh accessor), and the
//! stkAAVE transfer channel (the canonical + only stkAAVE balance-mutation
//! path) with the NULL-balance backfill. The post-apply discount refresh is NOT
//! here: it composes at the transaction sequencer (`run::resolution`), which owns
//! every settled read position.
//!
//! The pre-pass consumes the vToken revision its caller re-resolved per
//! transaction (stage 1 of `ChunkContext::begin_dispatch`) — with the
//! deprecation applied, the snapshot must serve the applied revision + the
//! recorded corpus must carry ZERO `getDiscountPercent` entries (a
//! chunk-start revision snapshot produces exactly the mutated replay's RPC
//! shape).
//!
//! Surface table: `tests/fixtures/cassettes/wave4/UPGRADE-MAP.md` §(A)
//! row 7 (consumer side of the per-tx revision re-resolve) + §(C) + the
//! deprecation negative probe.

#![expect(clippy::doc_markdown)]

use std::collections::{HashMap, HashSet};

use crate::projection::ConfigurationEffect;
use alloy::primitives::{Address, Bytes, U256};
use degenbot_rpc::chain_evidence::BlockReads;

use super::context::ChunkContext;
use super::{
    checksum, discount_to_i64, encode_single_address_call, eth_calls_batched_or_direct,
    require_word0_u256, ConfigDispatchError,
};

/// `DiscountPercentUpdated(user, old, new)` →
/// [`ConfigurationEffect::GhoDiscountPercentUpdated`] (`newDiscountPercent` is
/// topic[2] — the decoder already extracted it).
pub(super) fn dispatch_discount_percent_updated(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3DiscountPercentUpdatedEvent,
) -> Result<ConfigurationEffect, ConfigDispatchError> {
    let user_str = checksum(&decoded.user);
    let user_id = ctx.substrate_mut().user_id_or_create(&user_str, 0)?;
    Ok(ConfigurationEffect::GhoDiscountPercentUpdated {
        user_id,
        new_discount_percent: discount_to_i64(decoded.new_discount_percent),
    })
}

/// Discount-token (stkAAVE) ERC20 `Transfer(from, to, value)` →
/// [`ConfigurationEffect::StkAaveTransfer`] — the canonical + only stkAAVE
/// balance-mutation channel. Mirrors `stkaave.process_stk_aave_transfer_event`
/// EXACTLY: processes EVERY Transfer event (including both zero-leg arms),
/// treating `from == ZERO_ADDRESS` / `to == ZERO_ADDRESS` as a half-event
/// (skip the zero side, always mutate the real user) via `Option<i64>`
/// user_ids that are `None` iff the corresponding address is `ZERO_ADDRESS`.
///
/// Scoping:
/// - Returns `None` if the emitter is NOT `gho_asset.v_gho_discount_token` —
///   aToken/vToken/scaled-token Transfers are handled by the ops path
///   (`OperationReducer::reduce_transaction`); only the stkAAVE discount token is this fn's
///   scope (matches the Python `assert contract_address == discount_token`).
///
/// A zero-leg `Transfer` is a half-event: the `ZERO_ADDRESS` side is skipped
/// and the real user mutated. It must NOT be dedupe-skipped in favour of a
/// paired `Staked`/`Redeem` semantic event — that pairing is not an invariant.
/// Some actions emit ONLY the `Transfer(X→0)` event with no paired `Redeem`,
/// leaving the sender's `stk_aave_balance` stuck at its pre-burn cache value
/// → wrong `calculate_gho_discount_rate` → GHO-burn delta overshoots
/// `prev_scaled_balance` → a negative balance. Staked/Redeem never mutate
/// balances (they're fetched only for classification in
/// `fetch_stk_aave_events`); all balance mutation flows through this fn.
pub(super) async fn dispatch_stk_aave_transfer(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3Erc20TransferEvent,
) -> Result<Option<ConfigurationEffect>, ConfigDispatchError> {
    // Read the GHO asset through the ctx-owned always-fresh accessor: a same-tx
    // `DiscountTokenUpdated` at an earlier logIndex bumps
    // `v_gho_discount_token` through an apply that marks the overlay dirty, so
    // this read sees it (the per-call re-resolution is a cache hit until then).
    let gho = ctx.gho_asset();
    // Scope: only the discount-token (stkAAVE) emitter.
    let Some(g) = gho.as_ref() else {
        return Ok(None);
    };
    let Some(discount_token) = g
        .v_gho_discount_token
        .as_deref()
        .and_then(|s| s.parse::<Address>().ok())
    else {
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
    // side). Dedupe-skipping the zero-leg in favour of a paired
    // Staked/Redeem semantic event is not safe: some actions emit ONLY the
    // `Transfer(X→0)` event with no paired `Redeem`, leaving the sender's
    // balance stuck at the pre-burn cache value → wrong
    // `calculate_gho_discount_rate` → GHO-burn delta overshoots
    // `prev_scaled_balance` → a negative balance.
    let from_user_id = if decoded.from == Address::ZERO {
        None // the mint-from-zero leg — skipped at apply.
    } else {
        Some(
            ctx.substrate_mut()
                .user_id_or_create(&checksum(&decoded.from), 0)?,
        )
    };
    let to_user_id = if decoded.to == Address::ZERO {
        None // the burn-to-zero leg — skipped at apply.
    } else {
        Some(
            ctx.substrate_mut()
                .user_id_or_create(&checksum(&decoded.to), 0)?,
        )
    };
    // Pre-apply backfill of either side when the column is `NULL` — mirrors
    // Python's `get_or_init_stk_aave_balance` at `block_number - 1` (without
    // it, an apply would treat `NULL` as zero and omit the pre-event on-chain
    // balance). No-op when the column is already populated.
    if let Some(uid) = from_user_id {
        backfill_user_stk_aave_balance_if_none(ctx, uid, discount_token).await?;
    }
    if let Some(uid) = to_user_id {
        backfill_user_stk_aave_balance_if_none(ctx, uid, discount_token).await?;
    }
    // The degenerate `Transfer(0x0 → 0x0)` lands both `None` (no real user) —
    // still emitted (counts as processed) but the apply is a no-op.
    Ok(Some(ConfigurationEffect::StkAaveTransfer {
        from_user_id,
        to_user_id,
        amount: decoded.value,
    }))
}

/// RPC-fetch `balanceOf(user)` at `block_number - 1` and queue it as a
/// `StkAaveBalanceObserved` observation when the user's `stk_aave_balance` is
/// still unset. An unset balance means the user was created by an earlier event
/// but never touched by a stkAAVE `Staked`/`Transfer` event YET; the on-chain
/// value at the previous block is the authoritative balance before any
/// current-block events. No-op when the balance is already projected (the
/// current-block deltas mutate it from there).
async fn backfill_user_stk_aave_balance_if_none(
    ctx: &mut ChunkContext<'_>,
    user_id: i64,
    discount_token: Address,
) -> Result<(), ConfigDispatchError> {
    let addr_str = ctx.substrate_mut().user_address(user_id)?;
    // The projected balance (not a live probe): a discount-token replacement in
    // this chunk invalidated it, and the reseed must come from the NEW ledger.
    let projected = ctx.substrate_mut().user_stk_aave_balance(&addr_str)?;
    if matches!(projected, Some(Some(_))) {
        return Ok(());
    }
    let user_addr: Address = addr_str.parse().map_err(|_| {
        ConfigDispatchError::DecodeShape(format!("bad user_address in backfill: {addr_str}"))
    })?;
    let balance = ctx
        .read_stk_aave_balance_at_parent(discount_token, user_addr)
        .await?;
    ctx.push_observation(user_id, balance);
    Ok(())
}

/// `DiscountTokenUpdated(old, new)` → [`ConfigurationEffect::GhoDiscountTokenUpdated`].
///
/// # Emitter-validation guard
///
/// `fetch_discount_config_logs` is chain-wide (no address filter — the events
/// emit from any contract). The load-bearing guard filters spurious
/// chain-wide discount events: returns `Ok(None)` when the GHO vToken FK is
/// missing OR the decoded
/// emitter (`decoded.v_token_address` = `log.address()`) doesn't match
/// `gho_asset.v_token_address`. The apply fn never sees the skip.
#[expect(clippy::unnecessary_wraps)]
pub(super) fn dispatch_discount_token_updated(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3DiscountTokenUpdatedEvent,
) -> Result<Option<ConfigurationEffect>, ConfigDispatchError> {
    // The GHO asset through the ctx-owned always-fresh accessor — the guard
    // below compares against the just-linked `v_token_address` (a same-tx
    // `ReserveInitialized` at an earlier logIndex is visible), never a per-tx
    // snapshot. No row → `Ok(None)` (a chain-wide fetch can surface events from
    // chains without a seeded GHO market).
    let Some(g) = ctx.gho_asset() else {
        return Ok(None);
    };
    if g.v_token_address.as_deref() != Some(&checksum(&decoded.v_token_address)) {
        return Ok(None);
    }
    Ok(Some(ConfigurationEffect::GhoDiscountTokenUpdated {
        gho_token_id: g.id,
        new_discount_token: Some(checksum(&decoded.new_discount_token)),
    }))
}

/// `DiscountRateStrategyUpdated(old, new)` →
/// [`ConfigurationEffect::GhoDiscountRateStrategyUpdated`].
///
/// Same emitter-validation guard as [`dispatch_discount_token_updated`] — see
/// its docs; the chain-wide fetch + the emitter-address guard apply
/// identically.
#[expect(clippy::unnecessary_wraps)]
pub(super) fn dispatch_discount_rate_strategy_updated(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3DiscountRateStrategyUpdatedEvent,
) -> Result<Option<ConfigurationEffect>, ConfigDispatchError> {
    let Some(g) = ctx.gho_asset() else {
        return Ok(None);
    };
    if g.v_token_address.as_deref() != Some(&checksum(&decoded.v_token_address)) {
        return Ok(None);
    }
    Ok(Some(ConfigurationEffect::GhoDiscountRateStrategyUpdated {
        gho_token_id: g.id,
        new_strategy: Some(checksum(&decoded.new_strategy)),
    }))
}

// ── the discount pre-pass (the 3-way hybrid) ───────────────────────────────

/// Build the per-tx GHO discount snapshot — the `HashMap<Address, U256>` that
/// `OperationReducer::reduce_transaction` consumes as the `discounts` field: scan the
/// tx's GHO
/// vToken Mint/Burn events, + for each user, resolve the START discount (the
/// value in effect at tx start, BEFORE any `DISCOUNT_PERCENT_UPDATED` in this
/// tx — the `GhoDiscountContext` applies the in-tx overrides via
/// `effective_discount`).
///
/// # The 3-way hybrid
///
/// - **path #1 (DB-cache):** the user exists in `aave_v3_users` → read
///   `gho_discount` (the snapshot at tx start).
/// - **path #2 (RPC):** the user is NOT in the DB → `getDiscountPercent(user)`
///   `eth_call` at the tx's `block_number` (the user has an existing GHO debt
///   position first encountered in this chunk's tx range). Mirrors the
///   Python `raw_call(getDiscountPercent)`. Only when `vtoken_revision < 4`.
/// - **path #3 (V4+ deprecation):** `vtoken_revision >= 4` → the discount
///   mechanism is deprecated → 0 — the `is_discount_supported` gate
///   (revision < 4).
///
/// Returns an empty map when there's no GHO vToken (`gho_vtoken_address` is
/// `None`) or the revision couldn't be resolved.
pub(super) async fn build_discount_snapshot(
    reads: &BlockReads<'_>,
    tx_logs: &[&alloy::rpc::types::Log],
    gho_vtoken_address: Option<Address>,
    vtoken_revision: Option<u32>,
    substrate: &mut crate::updater::run::substrate::ChunkSubstrate,
) -> Result<HashMap<Address, U256>, ConfigDispatchError> {
    use degenbot_decoders::aave_event_decoder::{AAVE_BURN_TOPIC, AAVE_MINT_TOPIC};

    let Some(vtoken_addr) = gho_vtoken_address else {
        return Ok(HashMap::new());
    };
    let Some(rev) = vtoken_revision else {
        return Ok(HashMap::new());
    };

    let mut snapshot: HashMap<Address, U256> = HashMap::new();
    // path #2 is DEFERRED: the users the projection cannot settle collect here
    // (first-encounter order) and resolve in ONE transport pass after the
    // scan. The projection reads keep their per-log position; nothing mutates
    // the read position between them, so the deferral cannot observe different
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
        // The one classification (path #1 settled / #2 chain read / #3
        // deprecated): a checkpoint restates its three inputs, so the restored
        // driver and this loop cannot disagree about which path answers a user.
        let user_str = checksum(&user);
        let settled = substrate.user_gho_discount(&user_str)?;
        match crate::projection::classify_discount_prepass(
            crate::projection::DiscountPrepassInputs {
                vtoken: Some(vtoken_addr),
                revision: Some(rev),
                settled,
            },
        ) {
            crate::projection::DiscountPath::Settled(discount) => {
                snapshot.insert(user, U256::from(u64::try_from(discount).unwrap_or(0)));
            }
            crate::projection::DiscountPath::ChainRead => {
                rpc_pending.insert(user);
                rpc_users.push(user);
            }
            crate::projection::DiscountPath::Deprecated
            | crate::projection::DiscountPath::NoGhoToken
            | crate::projection::DiscountPath::UnresolvedRevision => {
                snapshot.insert(user, U256::ZERO);
            }
        }
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
    let rets = eth_calls_batched_or_direct(reads, &calldatas).await?;
    for (user, ret) in rpc_users.iter().zip(rets) {
        let discount = require_word0_u256(
            &ret,
            &format!("getDiscountPercent(address) for user {}", checksum(user)),
            &vtoken_addr,
        )?;
        snapshot.insert(*user, discount);
    }
    Ok(snapshot)
}

/// Decode the first topic word as an `Address` (the low 20 bytes).
fn topic_to_address(topic: alloy::primitives::B256) -> Address {
    Address::from_slice(&topic.as_slice()[12..])
}
