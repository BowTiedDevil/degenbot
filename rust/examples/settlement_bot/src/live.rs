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

use crate::claims::{VerificationError, VerifyErrorKind};
use crate::discovery::{BatchedPathFinder, BuiltGraph, DiscoveryParams};
use crate::ledger::{BuildFailure, RegistrationLedger};
use crate::pipeline::{
    CandidateOutcome, PipelineReport, PrepareOutcome, PreparedCandidate, RegistrationPipeline,
};
use degenbot::bot_core::construction_io::ConstructionIo;
use degenbot::bot_core::pool_builder::builder::{
    build_v2, build_v3, build_v4, V4PoolBuildIdentity,
};
use degenbot::bot_core::state_lock::LockSite;
use degenbot::db::discovery_read::DiscoveryPoolRow;
use degenbot::db::snapshot::TickMapDb;
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
) -> PipelineReport {
    let mut report = PipelineReport::default();
    let mut finder = BatchedPathFinder::new(&built.graph, params);
    while let Some(batch) = finder.next_batch() {
        for path in &batch {
            match pipeline.prepare_candidate(built, path, input_token_lower, weth_lower) {
                PrepareOutcome::Ready(candidate) => {
                    let v4_hops = candidate.v4_hops;
                    let outcome =
                        build_and_register(driver, built, rows, pipeline, &candidate, ctx).await;
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
        }
        tokio::task::yield_now().await;
    }
    report
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
                let refusal =
                    RegistrationLedger::classify_build_refusal(&failure, node.kind, Some(detail));
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
async fn build_one(
    driver: &EngineDriver,
    row: &DiscoveryPoolRow,
    ctx: &LiveContext<'_>,
) -> Result<u64, BuildFailure> {
    match row {
        DiscoveryPoolRow::V2(r) => {
            let params = build_v2(ctx.chain_id, r.pool.address, ctx.io, ctx.block)
                .await
                .map_err(|e| BuildFailure::Transient(e.to_string()))?;
            driver
                .bot()
                .state_arc()
                .write_at(LockSite::Core)
                .register_v2_pool(&params)
                .map_err(|e| BuildFailure::Transient(format!("{e:?}")))
        }
        DiscoveryPoolRow::V3(r) => {
            let params = build_v3(ctx.chain_id, r.pool.address, ctx.db, ctx.io, ctx.block)
                .await
                .map_err(|e| BuildFailure::Transient(e.to_string()))?;
            driver
                .bot()
                .state_arc()
                .write_at(LockSite::Core)
                .register_v3_pool(&params)
                .map_err(|e| BuildFailure::Transient(format!("{e:?}")))
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
            let result = build_v4(identity, ctx.db, ctx.io, ctx.block)
                .await
                .map_err(|e| BuildFailure::Transient(e.to_string()))?;
            driver
                .bot()
                .state_arc()
                .write_at(LockSite::Core)
                .register_v4_pool(&result.params)
                .map_err(map_v4_register_error)
        }
    }
}

/// Map the typed V4 admission refusal to the driver build-failure taxonomy.
fn map_v4_register_error(err: degenbot::bot_core::RegisterV4PoolError) -> BuildFailure {
    use degenbot::bot_core::RegisterV4PoolError;
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
            let policy = pipeline.retry_policy.clone();
            let result = pipeline
                .verify_claims
                .run_exclusive(&key, move || {
                    let policy = policy.clone();
                    async move {
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
                    }
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
            let policy = pipeline.retry_policy.clone();
            let result = pipeline
                .verify_claims
                .run_exclusive(&key, move || {
                    let policy = policy.clone();
                    async move {
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
                    }
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
