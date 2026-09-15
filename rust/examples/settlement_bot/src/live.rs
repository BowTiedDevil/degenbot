//! The smoke-gated live registration arm of the G3 pipeline (ergo
//! `XFEJUG`).
//!
//! Mirrors `build_paths.py::PathRegistrationPipeline._registration_unit`'s
//! build/verify/register turns when a live node is present:
//!
//! 1. build each candidate hop through the core `pool_builder`
//!    (`build_v2`/`build_v3`/`build_v4`) and register it into the shared
//!    `BotState` (the Rust builders publish into the engine's state);
//! 2. run the ADR-022 verify lifecycle for each V3/V4 hop under the tokio
//!    at-most-once claim table + the RPC-only retry dance;
//! 3. `EngineDriver::register_and_solve_path` with the `(pool_id, zfo)` hop
//!    list (the Python `register_crawl_path` shape).
//!
//! This module is only reached behind `SMOKE_RPC_URL`; the offline CI gate
//! exercises the shared preparation stages instead.

use std::time::Instant;

use crate::claims::{VerificationError, VerifyErrorKind};
use crate::discovery::{BatchedPathFinder, BuiltGraph, DiscoveryParams};
use crate::ledger::{BuildFailure, RegistrationLedger, RegistrationOutcome};
use crate::pipeline::{
    CandidateOutcome, PipelineReport, PrepareOutcome, PreparedCandidate, RegistrationPipeline,
};
use crate::progress::{progress_line, ProgressCadence};
use alloy::primitives::Address;
use degenbot::bot_core::construction_io::ConstructionIo;
use degenbot::bot_core::pool_builder::builder::{
    build_v2, build_v3, build_v4, V4PoolBuildIdentity,
};
use degenbot::bot_core::state_lock::LockSite;
use degenbot::bot_core::{Bot, RegisterV2PoolError, RegisterV3PoolError, RegisterV4PoolError};
use degenbot::db::discovery_read::DiscoveryPoolRow;
use degenbot::db::snapshot::TickMapDb;
use degenbot::pathfinding::PoolKind;
use degenbot::solvers::mixed::PoolHop;
use degenbot::EngineDriver;

/// The live construction context (constructed once per boot).
pub struct LiveContext<'a> {
    /// The settled chain id.
    pub chain_id: u64,
    /// The snapshot block used for the builds/lifecycles (`None` = head).
    pub block: Option<u64>,
    /// The construction I/O (DB + RPC adapters).
    pub io: &'a ConstructionIo,
    /// The optional DB tick-map handle (Tracked arms).
    pub db: Option<&'a dyn TickMapDb>,
}

/// Run the live pipeline over the batched discovery enumeration.
#[expect(
    clippy::too_many_arguments,
    reason = "linear live driver mirroring the Python build_paths signature surface"
)]
pub async fn run_live(
    driver: &EngineDriver,
    built: &BuiltGraph,
    rows: &[DiscoveryPoolRow],
    pipeline: &mut RegistrationPipeline,
    params: &DiscoveryParams,
    input_token_lower: &str,
    weth_lower: &str,
    ctx: &LiveContext<'_>,
    progress: &mut ProgressCadence,
) -> PipelineReport {
    let mut report = PipelineReport::default();
    let mut finder = BatchedPathFinder::new(&built.graph, params);
    // The Registered-path cap (RSP-11) is a BENIGN STOP of discovery: the
    // first `RegistryFull` refusal ends the crawl, exactly as Python's
    // `run_registration` checks `self.capped` at the top of its loop. Without
    // this the loop would keep grinding through every remaining candidate for
    // no engine growth (the observed 12.6M-path silent crawl).
    'crawl: while let Some(batch) = finder.next_batch() {
        for path in &batch {
            let mut stop = false;
            match pipeline.prepare_candidate(built, path, input_token_lower, weth_lower) {
                PrepareOutcome::Ready(candidate) => {
                    let v4_hops = candidate.v4_hops;
                    let outcome =
                        build_and_register(driver, built, rows, pipeline, &candidate, ctx).await;
                    stop = crawl_stops_on(&outcome);
                    RegistrationPipeline::absorb(&mut report, &outcome, v4_hops);
                }
                PrepareOutcome::Skip(outcome) => {
                    RegistrationPipeline::absorb(&mut report, &outcome, 0);
                }
                PrepareOutcome::Reject(outcome) => {
                    RegistrationPipeline::absorb(&mut report, &outcome, 0);
                    report.policy_rejected += 1;
                }
                PrepareOutcome::DirectionFatal(message) => {
                    report.direction_errors += 1;
                    *report
                        .skip_reasons
                        .entry(format!("direction-fatal:{message}"))
                        .or_insert(0) += 1;
                }
            }
            // INN6TK: the time-throttled summary fires even when nothing
            // registers, so a discovery-heavy skip-fest stays visible.
            if progress.due(Instant::now()) {
                println!("{}", progress_line(&report));
            }
            if stop {
                break 'crawl;
            }
        }
        tokio::task::yield_now().await;
    }
    // The completion summary is forced regardless of the cadence (Python's
    // `pipeline.emit_registration_progress(force=True)`).
    println!("{}", progress_line(&report));
    report
}

/// Whether the crawl must stop after this unit outcome.
///
/// The typed [`CandidateOutcome::Cap`] is the fold of the engine registry's
/// `PathRegistrationError::RegistryFull` refusal: the BENIGN STOP of
/// discovery (Python latches `capped` and breaks `run_registration`), never
/// counted as a per-candidate error and never a reason to keep crawling a
/// full registry.
#[must_use]
pub fn crawl_stops_on(outcome: &CandidateOutcome) -> bool {
    matches!(outcome, CandidateOutcome::Cap)
}

/// Build each hop, register it into `BotState`, verify it under the claims
/// table, then register the path.
async fn build_and_register(
    driver: &EngineDriver,
    built: &BuiltGraph,
    rows: &[DiscoveryPoolRow],
    pipeline: &mut RegistrationPipeline,
    candidate: &PreparedCandidate,
    ctx: &LiveContext<'_>,
) -> CandidateOutcome {
    let mut pool_ids: Vec<u64> = Vec::with_capacity(candidate.pool_indices.len());
    for node_idx in &candidate.pool_indices {
        let node = &built.nodes[*node_idx];
        let row = &rows[node.row_index];
        match build_one(driver, row, ctx).await {
            Ok(pool_id) => pool_ids.push(pool_id),
            Err(failure) => {
                let detail = build_failure_detail(&failure);
                let refusal = RegistrationLedger::classify_build_refusal(
                    &failure,
                    node.kind,
                    Some(detail.clone()),
                );
                emit_build_refusal_sample(node.kind, &node.identity, refusal.outcome, &detail);
                if refusal.stable {
                    pipeline.ledger.memoize_unregistrable(
                        node.memo_key.as_deref(),
                        refusal.outcome,
                        refusal.counts_as_skip,
                    );
                }
                return CandidateOutcome::Skip {
                    outcome: refusal.outcome,
                    counts_as_skip: refusal.counts_as_skip,
                };
            }
        }
    }

    for node_idx in &candidate.pool_indices {
        let node = &built.nodes[*node_idx];
        let row = &rows[node.row_index];
        if let Err(outcome) = verify_one(driver, pipeline, row, ctx).await {
            return outcome;
        }
    }

    let hops: Vec<PoolHop> = pool_ids
        .iter()
        .zip(candidate.zfos.iter())
        .map(|(pool_id, zero_for_one)| PoolHop {
            pool_id: *pool_id,
            zero_for_one: *zero_for_one,
        })
        .collect();

    match driver.register_and_solve_path(hops) {
        Ok((_path_id, created)) => {
            pipeline
                .ledger
                .memoize_registered_path(candidate.hop_sig.clone());
            CandidateOutcome::Registered { created }
        }
        Err(err) => {
            if matches!(
                err,
                degenbot::bot::arb_engine::lifecycle::PathRegistrationError::RegistryFull { .. }
            ) {
                CandidateOutcome::Cap
            } else {
                CandidateOutcome::RegisterFailed {
                    detail: err.to_string(),
                }
            }
        }
    }
}

/// Build + register one hop into the shared `BotState`.
///
/// The registry-of-record reuse pre-check runs BEFORE any build/RPC (the
/// IRUMXD single-registry discipline, mirroring the Python `build_v2_pool`
/// adapter's reuse fast path): a hop the engine already registered is answered
/// by the existing `pool_id`, never re-built and never refused. The
/// build+register turn only runs on a genuine miss. If a concurrent writer
/// registers the hop between the pre-check and the write (`AlreadyRegistered`
/// despite the pre-check — a benign race artifact, never a pool fact), the
/// failed admission folds back to the freshly-readable `pool_id`.
async fn build_one(
    driver: &EngineDriver,
    row: &DiscoveryPoolRow,
    ctx: &LiveContext<'_>,
) -> Result<u64, BuildFailure> {
    let bot = driver.bot();
    if let Some(pool_id) = reuse_registered_pool(bot, row) {
        return Ok(pool_id);
    }
    // Match the Python `_build_delegated` adapter: `ctx.block == None` means
    // head, but the builder's `update_block` would degrade to 0 on a literal
    // `None` — mis-keying the registration seed so the backfill drain
    // double-counts a head-fresh liquidity scalar. Resolve head concretely.
    let build_block = resolve_build_block(ctx).await?;
    match row {
        DiscoveryPoolRow::V2(r) => {
            let params = build_v2(ctx.chain_id, r.pool.address, ctx.io, Some(build_block))
                .await
                .map_err(|e| BuildFailure::Transient(e.to_string()))?;
            let result = bot
                .state_arc()
                .write_at(LockSite::Core)
                .register_v2_pool(&params);
            fold_already_registered(
                result,
                |e| matches!(e, RegisterV2PoolError::AlreadyRegistered { .. }),
                || reuse_address_pool(bot, &r.pool.address),
                |e| BuildFailure::Transient(format!("{e:?}")),
            )
        }
        DiscoveryPoolRow::V3(r) => {
            let params = build_v3(
                ctx.chain_id,
                r.pool.address,
                ctx.db,
                ctx.io,
                Some(build_block),
            )
            .await
            .map_err(|e| BuildFailure::Transient(e.to_string()))?;
            let result = bot
                .state_arc()
                .write_at(LockSite::Core)
                .register_v3_pool(&params);
            fold_already_registered(
                result,
                |e| matches!(e, RegisterV3PoolError::AlreadyRegistered { .. }),
                || reuse_address_pool(bot, &r.pool.address),
                |e| BuildFailure::Transient(format!("{e:?}")),
            )
        }
        DiscoveryPoolRow::V4(r) => {
            let mut pool_id = [0_u8; 32];
            pool_id.copy_from_slice(r.pool_hash.as_slice());
            let identity = V4PoolBuildIdentity {
                pool_manager: r.manager.address,
                state_view: r.manager.state_view.unwrap_or_default(),
                pool_id,
                currency0: r.token0.address,
                currency1: r.token1.address,
                fee: u32::try_from(r.fee_currency0).unwrap_or(0),
                tick_spacing: i32::try_from(r.tick_spacing).unwrap_or(0),
                hook_address: r.hooks,
            };
            let result = build_v4(identity, ctx.db, ctx.io, Some(build_block))
                .await
                .map_err(|e| BuildFailure::Transient(e.to_string()))?;
            let registered = bot
                .state_arc()
                .write_at(LockSite::Core)
                .register_v4_pool(&result.params);
            fold_already_registered(
                registered,
                |e| matches!(e, RegisterV4PoolError::AlreadyRegistered { .. }),
                || reuse_v4_pool(bot, r.manager.address, &pool_id),
                map_v4_register_error,
            )
        }
    }
}

/// Resolve the builder block exactly as the Python `_build_delegated` adapter
/// does: an explicit `ctx.block` passes through; `None` (head) is resolved to
/// a concrete block number so the builder's `update_block` cannot degrade to
/// `0` (`build_v3`'s `block.unwrap_or(0)`), which would mis-key the
/// registration seed and make the backfill drain double-count the head-fresh
/// liquidity scalar (the observed in-range invariant panic).
async fn resolve_build_block(ctx: &LiveContext<'_>) -> Result<u64, BuildFailure> {
    match ctx.block {
        Some(block) => Ok(block),
        None => ctx
            .io
            .get_block_number()
            .await
            .map_err(|e| BuildFailure::Transient(format!("head block: {e}"))),
    }
}

/// The registry-of-record reuse lookup for one candidate hop, dispatched by
/// family — address-keyed for V2/V3, `(pool_manager, pool_id)`-keyed for V4.
fn reuse_registered_pool(bot: &Bot, row: &DiscoveryPoolRow) -> Option<u64> {
    match row {
        DiscoveryPoolRow::V2(r) => reuse_address_pool(bot, &r.pool.address),
        DiscoveryPoolRow::V3(r) => reuse_address_pool(bot, &r.pool.address),
        DiscoveryPoolRow::V4(r) => {
            let mut pool_id = [0_u8; 32];
            pool_id.copy_from_slice(r.pool_hash.as_slice());
            reuse_v4_pool(bot, r.manager.address, &pool_id)
        }
    }
}

/// Reuse an already-registered address-keyed (V2/V3) pool identity.
fn reuse_address_pool(bot: &Bot, address: &Address) -> Option<u64> {
    bot.state_arc()
        .read_at(LockSite::Core)
        .registered_pool_by_address(address)
        .map(|(pool_id, _family)| pool_id)
}

/// Reuse an already-registered `(pool_manager, pool_id)`-keyed V4 identity.
fn reuse_v4_pool(bot: &Bot, pool_manager: Address, pool_id: &[u8; 32]) -> Option<u64> {
    bot.state_arc()
        .read_at(LockSite::Core)
        .try_registered_v4(pool_manager, pool_id)
        .map(|registered| registered.pool_id)
}

/// The V4 `StateView` contract address the registration verify lifecycle must
/// use, resolved from the discovered `pool_managers` rows — the same
/// per-manager column the V4 build path already trusts
/// (`DiscoveryV4Row::manager.state_view`).
///
/// Python sources this fact from a chain deployment constant
/// (`EthereumMainnetUniswapV4.state_view.address`) and passes it to
/// `EngineRegistry.start(..., verify_state_view=...)`
/// (`src/degenbot/runner/bot_runner.py:470`). The fresh Rust driver boots with
/// no deployment registry, so the example resolves the same fact from the
/// snapshot enumeration instead of hard-coding a chain address; the first
/// manager row that carries one wins (one `StateView` per `pool_manager`, and
/// the verify lifecycle takes a single address).
///
/// `None` when the enumeration carries no V4 manager row with a `state_view`;
/// the caller then leaves the driver unconfigured and every V4 verify fails
/// fast with its D-C no-config refusal (loud, never silently skipped).
#[must_use]
pub fn resolve_verify_state_view(rows: &[DiscoveryPoolRow]) -> Option<Address> {
    rows.iter().find_map(|row| match row {
        DiscoveryPoolRow::V4(v4) => v4.manager.state_view,
        DiscoveryPoolRow::V2(_) | DiscoveryPoolRow::V3(_) => None,
    })
}

/// Fold an admission result into a registry reuse when the engine reports a
/// concurrent `AlreadyRegistered`.
///
/// The pre-check can race a concurrent writer (the engine admits pools from
/// other tasks), so `AlreadyRegistered` here is benign: `reuse_registered`
/// re-reads the registry AFTER the failed write guard is dropped and returns
/// the id the winner installed. Only a re-read miss (a refusal with no
/// registry entry to reuse) stays a transient build failure.
fn fold_already_registered<T, E: std::fmt::Debug>(
    result: Result<T, E>,
    is_already_registered: impl FnOnce(&E) -> bool,
    reuse_registered: impl FnOnce() -> Option<T>,
    map_refusal: impl FnOnce(E) -> BuildFailure,
) -> Result<T, BuildFailure> {
    match result {
        Ok(pool_id) => Ok(pool_id),
        Err(err) if is_already_registered(&err) => reuse_registered().ok_or_else(|| {
            BuildFailure::Transient(format!(
                "already-registered admission race with no registry entry to reuse ({err:?})"
            ))
        }),
        Err(err) => Err(map_refusal(err)),
    }
}

/// Map the typed V4 admission refusal to the driver build-failure taxonomy.
fn map_v4_register_error(err: RegisterV4PoolError) -> BuildFailure {
    match err {
        RegisterV4PoolError::DynamicFee { .. } => BuildFailure::DynamicFee,
        RegisterV4PoolError::HookedPool { .. } => BuildFailure::HookedPool,
        RegisterV4PoolError::FeeExceedsEncoderLimit { .. }
        | RegisterV4PoolError::SpecViolation(_) => BuildFailure::HighFee,
        other @ RegisterV4PoolError::AlreadyRegistered { .. } => {
            BuildFailure::Transient(format!("{other:?}"))
        }
    }
}

/// Run one hop's verify lifecycle under the claim table + retry dance.
#[expect(
    clippy::redundant_locals,
    reason = "Copy captures must be re-bound before `async move` inside the retry closure"
)]
async fn verify_one(
    driver: &EngineDriver,
    pipeline: &mut RegistrationPipeline,
    row: &DiscoveryPoolRow,
    ctx: &LiveContext<'_>,
) -> Result<(), CandidateOutcome> {
    match row {
        DiscoveryPoolRow::V3(r) => {
            let key = format!(
                "v3:{}",
                degenbot::core::address_utils::address_to_checksum_string(&r.pool.address)
            );
            if pipeline.ledger.pool_verified(&key) {
                return Ok(());
            }
            let address = r.pool.address;
            let block = ctx.block;
            let policy = pipeline.retry_policy;
            let result = pipeline
                .verify_claims
                .run_exclusive(&key, move || async move {
                    crate::retry::retry_verification_call(&policy, |_attempt| {
                        let driver = driver;
                        let address = address;
                        let block = block;
                        async move {
                            driver
                                .run_v3_registration_lifecycle(address, block)
                                .await
                                .map_err(|e| map_driver_error(&e))
                        }
                    })
                    .await
                })
                .await;
            match result {
                Ok(()) => {
                    pipeline.ledger.memoize_verified_pool(key);
                    Ok(())
                }
                Err(err) => Err(CandidateOutcome::RegisterFailed {
                    detail: format!("verify-v3: {err}"),
                }),
            }
        }
        DiscoveryPoolRow::V4(r) => {
            let key = format!("v4:{:#x}", r.pool_hash);
            if pipeline.ledger.pool_verified(&key) {
                return Ok(());
            }
            let manager = r.manager.address;
            let mut pool_id = [0_u8; 32];
            pool_id.copy_from_slice(r.pool_hash.as_slice());
            let block = ctx.block;
            let policy = pipeline.retry_policy;
            let result = pipeline
                .verify_claims
                .run_exclusive(&key, move || async move {
                    crate::retry::retry_verification_call(&policy, |_attempt| {
                        let driver = driver;
                        let manager = manager;
                        let pool_id = pool_id;
                        let block = block;
                        async move {
                            driver
                                .run_v4_registration_lifecycle(manager, pool_id, block)
                                .await
                                .map_err(|e| map_driver_error(&e))
                        }
                    })
                    .await
                })
                .await;
            match result {
                Ok(()) => {
                    pipeline.ledger.memoize_verified_pool(key);
                    Ok(())
                }
                Err(err) => Err(CandidateOutcome::RegisterFailed {
                    detail: format!("verify-v4: {err}"),
                }),
            }
        }
        DiscoveryPoolRow::V2(_) => Ok(()),
    }
}

/// The human-readable detail for a build failure (log-only).
fn build_failure_detail(failure: &BuildFailure) -> String {
    match failure {
        BuildFailure::Transient(message) => message.clone(),
        BuildFailure::HookedPool => "hook admission refusal".to_string(),
        BuildFailure::DynamicFee => "dynamic-fee admission refusal".to_string(),
        BuildFailure::HighFee => "fee exceeds the encoder limit".to_string(),
    }
}

/// At most this many distinct samples are ever retained (and printed) per
/// tag, so the sampler's cardinality stays bounded even across a multi-hour
/// crawl.
const REG_DEBUG_MAX_DISTINCT: usize = 3;

/// Diagnostic-only sampler gated by `DEGENBOT_REG_DEBUG_SAMPLES=1`: prints
/// up to `REG_DEBUG_MAX_DISTINCT` distinct `(tag, detail)` build-refusal
/// pairs (with the failing pool's identity + kind) so the top-level
/// `build-v2-refused`/`build-v4-refused` tags can be traced to their
/// underlying `PoolBuilderError`/registration error.
///
/// Purely observational: it never mutates a counter, memo, or classification,
/// and does nothing unless the env gate is set, so production behavior is
/// unchanged.
fn emit_build_refusal_sample(
    kind: PoolKind,
    identity: &str,
    outcome: RegistrationOutcome,
    detail: &str,
) {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<String, usize>>> =
        std::sync::OnceLock::new();
    if std::env::var("DEGENBOT_REG_DEBUG_SAMPLES").ok().as_deref() != Some("1") {
        return;
    }
    let seen = SEEN.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()));
    let mut guard = match seen.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let emitted = guard.entry(outcome.as_str().to_string()).or_insert(0);
    if *emitted < REG_DEBUG_MAX_DISTINCT {
        *emitted += 1;
        println!(
            "[reg-debug] tag={} kind={kind:?} pool={identity} detail={detail}",
            outcome.as_str()
        );
    }
}

/// Diagnostic-only sampler gated by `DEGENBOT_REG_DEBUG_SAMPLES=1`: prints
/// up to `REG_DEBUG_MAX_DISTINCT` DISTINCT register-failure detail strings
/// (the `PathRegistrationError` display text returned by
/// `EngineDriver::register_and_solve_path`) so the `register-fail` skip
/// class — historically 7.1M members with no witness — can be traced to the
/// engine-side refusal that produced it.
///
/// Bounded cardinality: at most `REG_DEBUG_MAX_DISTINCT` distinct detail
/// strings are ever retained; repeats only bump a counter and are never
/// printed, so a live crawl cannot grow this map without bound.
///
/// Purely observational: it never mutates a counter, memo, or classification,
/// and does nothing unless the env gate is set, so production behavior is
/// unchanged.
pub(crate) fn emit_register_failure_sample(detail: &str) {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<String, usize>>> =
        std::sync::OnceLock::new();
    if std::env::var("DEGENBOT_REG_DEBUG_SAMPLES").ok().as_deref() != Some("1") {
        return;
    }
    let seen = SEEN.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()));
    let mut guard = match seen.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(count) = guard.get_mut(detail) {
        *count += 1;
        return;
    }
    if guard.len() < REG_DEBUG_MAX_DISTINCT {
        guard.insert(detail.to_string(), 1);
        println!("[reg-debug] tag=register-fail detail={detail}");
    }
}

/// Map a `DriverError` from a lifecycle call to the typed verify failure.
#[must_use]
pub fn map_driver_error(err: &degenbot::DriverError) -> VerificationError {
    match err {
        degenbot::DriverError::Verify(degenbot::RegistrationLifecycleError::Verify(
            degenbot::bot_core::liquidity_verifier::LiquidityVerifyError::Mismatch(_),
        )) => VerificationError::new(VerifyErrorKind::Mismatch, err.to_string()),
        degenbot::DriverError::Verify(degenbot::RegistrationLifecycleError::Verify(
            degenbot::bot_core::liquidity_verifier::LiquidityVerifyError::Rpc { .. },
        )) => VerificationError::new(VerifyErrorKind::Rpc, err.to_string()),
        _ => VerificationError::new(VerifyErrorKind::Other, err.to_string()),
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "test setup asserts the fixture registration succeeds"
)]
mod tests {
    use super::*;
    use crate::ledger::RegistrationOutcome;
    use degenbot::bot_core::RegisterV2PoolParams;

    /// A minimal in-spec V2 fixture keyed by `address`.
    fn v2_params(address: Address) -> RegisterV2PoolParams {
        RegisterV2PoolParams {
            address,
            token0: Address::from([0xaau8; 20]),
            token1: Address::from([0xbbu8; 20]),
            reserve0: alloy::primitives::aliases::U112::from(1_000_000u64),
            reserve1: alloy::primitives::aliases::U112::from(2_000_000u64),
            ..Default::default()
        }
    }

    /// Register the fixture into the bot's shared `BotState` (test setup).
    fn register_v2(bot: &Bot, address: Address) -> u64 {
        bot.state_arc()
            .write_at(LockSite::Core)
            .register_v2_pool(&v2_params(address))
            .expect("test setup: register V2 pool")
    }

    /// RSP-14: a V4 manager row carrying a `state_view` supplies the driver's
    /// verify address. Before this, the live arm booted `driver.start` with
    /// `verify_state_view = None`, so every V4 hop's verify folded the core
    /// `RegistrationLifecycleError::MissingStateView` into a per-candidate
    /// `register-fail` (the observed 46 612-member skip class).
    #[test]
    fn verify_state_view_resolves_from_the_first_v4_manager_row() {
        let view = Address::from([0x77u8; 20]);
        assert_eq!(
            resolve_verify_state_view(&[v4_row_with_state_view(Some(view))]),
            Some(view)
        );
        assert_eq!(resolve_verify_state_view(&[]), None);
    }

    /// A V4 manager row without a `state_view` resolves to `None`: the driver
    /// stays unconfigured and the core lifecycle reports its loud
    /// `MissingStateView` refusal rather than a silently skipped verify.
    #[test]
    fn verify_state_view_is_none_without_a_manager_state_view() {
        assert_eq!(
            resolve_verify_state_view(&[v4_row_with_state_view(None)]),
            None
        );
    }

    /// A minimal V4 discovery row whose manager carries `state_view`.
    fn v4_row_with_state_view(state_view: Option<Address>) -> DiscoveryPoolRow {
        use degenbot::db::discovery_read::DiscoveryV4Row;
        use degenbot::db::rows::{Erc20TokenRow, ExchangeRow, PoolManagerRow};
        let token = |id: i64, byte: u8| Erc20TokenRow {
            id,
            chain: 1,
            address: Address::from([byte; 20]),
            name: None,
            symbol: None,
            decimals: Some(18),
        };
        DiscoveryPoolRow::V4(DiscoveryV4Row {
            managed_pool_id: 1,
            pool_hash: alloy::primitives::B256::ZERO,
            hooks: Address::ZERO,
            manager: PoolManagerRow {
                id: 1,
                address: Address::from([0x44u8; 20]),
                chain: 1,
                kind: "uniswap_v4".to_string(),
                state_view,
                exchange_id: 1,
            },
            token0: token(1, 0xaa),
            token1: token(2, 0xbb),
            exchange: ExchangeRow {
                id: 1,
                chain_id: 1,
                name: "uniswap_v4".to_string(),
                active: true,
                last_update_block: None,
                factory: Address::ZERO,
                deployer: None,
            },
            fee_currency0: 0,
            fee_currency1: 0,
            fee_denominator: 1_000_000,
            tick_spacing: 10,
            liquidity_update_block: None,
            liquidity_update_log_index: None,
        })
    }

    /// RSP-12 shortcut: a hop registered by an earlier candidate is answered
    /// by the registry-of-record lookup, so `build_one` returns the existing
    /// engine id BEFORE `build_v2` (no second RPC round-trip, no refusal).
    #[test]
    fn already_registered_pool_is_reused_without_a_build() {
        let bot = Bot::new(1);
        let address = Address::from([0x11u8; 20]);
        // First encounter: no registry entry, so the build+register turn runs.
        assert_eq!(reuse_address_pool(&bot, &address), None);
        let id = register_v2(&bot, address);
        // Second candidate: the shortcut reuses the engine identity.
        assert_eq!(reuse_address_pool(&bot, &address), Some(id));
    }

    /// RSP-12 race fold: `AlreadyRegistered` despite the pre-check is a benign
    /// race, so the fold re-reads the registry and reuses the winner's id
    /// instead of refusing (which the ledger would count as a skip).
    #[test]
    fn already_registered_race_folds_to_reuse_not_a_skip() {
        let bot = Bot::new(1);
        let address = Address::from([0x22u8; 20]);
        let id = register_v2(&bot, address);
        let folded = fold_already_registered(
            Err::<u64, _>(RegisterV2PoolError::AlreadyRegistered { address }),
            |e| matches!(e, RegisterV2PoolError::AlreadyRegistered { .. }),
            || reuse_address_pool(&bot, &address),
            |e| BuildFailure::Transient(format!("{e:?}")),
        );
        assert_eq!(folded, Ok(id));
    }

    #[test]
    fn registry_full_is_a_benign_crawl_stop() {
        // `build_and_register` folds the engine registry's `RegistryFull`
        // refusal to `CandidateOutcome::Cap`; that outcome stops the crawl
        // and latches the report's capped witness via `absorb`.
        assert!(crawl_stops_on(&CandidateOutcome::Cap));
        assert!(!crawl_stops_on(&CandidateOutcome::Registered {
            created: true
        }));
        assert!(!crawl_stops_on(&CandidateOutcome::Skip {
            outcome: RegistrationOutcome::V4NoHash,
            counts_as_skip: true,
        }));

        let mut report = PipelineReport::default();
        RegistrationPipeline::absorb(&mut report, &CandidateOutcome::Cap, 0);
        assert!(report.capped, "the cap stop latches the benign witness");
        assert_eq!(report.cap_skip_count, 1);
        assert_eq!(report.skip_count, 1);
    }
}
