//! Stage 1 — batch assembly: raw rows + payload rows → typed verdicts +
//! sim-ready candidates.
//!
//! One home for the per-row pre-sim policy both consumers applied driftedly:
//! the `PyO3` seam skipped empty-hops + payload-served silently and raised on a
//! resolve miss; the settlement bot's `plan_batch` folded the resolve miss
//! into `SkipEmptyHops` and pre-applied suppression + thin margin on top of
//! the fan-out's own re-application (a double `is_suppressed` read whose
//! retry-stamp side effect could re-suppress a path due for its interval
//! retry). Here each policy applies EXACTLY ONCE, in the fan-out's documented
//! order, and every skip emits a typed [`AssemblyVerdict`] record.
//!
//! Policy inputs stay DISTINCT typed values (task decision b): the
//! batch-local payload-served set and the cross-block `PathSuppression`
//! registry are different decisions with different feedback loops, injected
//! separately — never merged into one knob.

use std::collections::{HashMap, HashSet};

use crate::record::PathInfoView;

use degenbot_arbitrage::{DispatchCandidate, FeeOnTransferRegistry, PoolDivergence};
use degenbot_executor::composers::{EncodeOptions, PathInfo};
use degenbot_submission::{PoolKey, SubmitCandidate};

use crate::record::{AssemblyVerdict, BatchOutcome, SimReceipt, SimulateVerdict};
use crate::row::{PayloadFailure, PayloadRow, RawResult};

/// Resolves a path id to its registered typed hops (the engine registry on
/// the Python side; a session map on the Rust side).
pub trait PathResolver: Send + Sync {
    /// The registered path, or `None` when the id is not registered.
    fn resolve(&self, path_id: u64) -> Option<PathInfo>;
}

/// A batch-structure corruption: a row whose per-hop lengths disagree with the
/// resolved path's hop count. The assembly stage aborts the whole batch (the
/// loud-abort rule — see the resolve-miss note on [`AssemblyVerdict`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssemblyError {
    /// The offending row's path id.
    pub path_id: u64,
    /// The mismatch detail.
    pub detail: String,
}

impl std::fmt::Display for AssemblyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "batch assembly corrupt at path {}: {}",
            self.path_id, self.detail
        )
    }
}

impl std::error::Error for AssemblyError {}

/// The stage-1 inputs — every policy value, injected.
pub struct AssemblyInputs<'a> {
    /// The raw solver rows.
    pub rows: &'a [RawResult],
    /// The inline-sim payload rows.
    pub payloads: &'a [PayloadRow],
    /// The path resolver.
    pub resolver: &'a dyn PathResolver,
    /// The encode options stamped onto every candidate.
    pub opts: EncodeOptions,
    /// The batch-local payload-served set (decision b: THIS batch's already-
    /// simulated paths — an idempotency/dedup rule with no state of its own).
    pub payload_served: &'a HashSet<u64>,
    /// The cross-block suppression registry (decision b: persistent feedback
    /// state, read exactly once per batch — the retry stamp is a side effect).
    pub suppression: &'a mut degenbot_submission::PathSuppression,
    /// The per-pool solver-divergence memo (skip + lifetime-tally read).
    pub divergence: &'a std::sync::Mutex<PoolDivergence>,
    /// The per-token fee-on-transfer registry (skip + lifetime-tally read).
    pub fot: &'a std::sync::Mutex<FeeOnTransferRegistry>,
    /// The block the batch is consumed at.
    pub current_block: u64,
    /// The thin-margin floor in bps (0 disables).
    pub min_profit_margin_bps: u64,
    /// The per-batch sim cap (the plain count the driver injects; clamped to
    /// the fan-out's own `MAX_SIMULATE_CONCURRENT` so the cap is applied once,
    /// here, where the dropped rows are known).
    pub max_candidates: usize,
    /// The session executor contract address — the join stamps it identically
    /// on every payload row (the `execute()` target).
    pub executor_address: alloy::primitives::Address,
}

/// The stage-1 product: the batch's records so far (assembly verdicts +
/// payload-arm simulate verdicts), the sim-ready candidates, and the
/// payload-joined submit rows.
#[derive(Debug, Default)]
pub struct AssembledBatch {
    /// The resolved `PathInfo` per sim-ready candidate id — the join map the
    /// sim leaf needs post-fan-out (the `SimResult` carries only a hop count).
    pub path_info_by_id: HashMap<u64, PathInfo>,
    /// One record per processed row, in input order (raw rows, then payload
    /// rows). Candidates that reach the sim stage get their record AFTER the
    /// fan-out (the sim leaf fills the verdicts); cap-dropped rows produce no
    /// record — the same drain-invisible drop today's truncate applies.
    pub outcomes: Vec<BatchOutcome>,
    /// The sim-ready candidates, in input order, post cap.
    pub candidates: Vec<DispatchCandidate>,
    /// The payload arm's joined submit rows (gas-profitable inline sims).
    pub payload_submits: Vec<SubmitCandidate>,
}

/// Assemble one batch (stage 1). See the module docs for the policy order and
/// the resolve-miss/shape-corruption split.
///
/// # Errors
///
/// [`AssemblyError`] when a row's hop lengths disagree with its resolved
/// path's hop count (batch corruption — the loud-abort rule).
///
/// # Panics
///
/// When a resolved candidate's path info is missing from the stage's join
/// map — an internal invariant (the map holds every resolved candidate).
#[expect(
    clippy::too_many_lines,
    reason = "the stage reads best as one ordered policy pass"
)]
#[expect(
    clippy::expect_used,
    reason = "the join map holds every resolved candidate — internal invariant"
)]
pub fn assemble_batch(inputs: &AssemblyInputs<'_>) -> Result<AssembledBatch, AssemblyError> {
    let mut batch = AssembledBatch::default();
    let mut path_info_by_id: HashMap<u64, PathInfo> = HashMap::new();

    // ── Raw rows ─────────────────────────────────────────────────────────
    // Order: empty-hops, payload-served, resolve, shape — the PyO3 seam's
    // documented order (the skip predicates before the resolve), with the
    // resolve miss now typed instead of raised.
    for row in inputs.rows {
        // The pre-resolve skip predicates — one home shared with the PyO3
        // assembly delegate ([`classify_raw_row`]).
        match classify_raw_row(row, inputs.payload_served) {
            RawRowClass::Skip(verdict) => {
                batch.outcomes.push(BatchOutcome {
                    path_id: row.path_id,
                    block: inputs.current_block,
                    assembly: verdict,
                    simulate: None,
                    submit: None,
                    path_info: PathInfoView::empty(),
                });
                continue;
            }
            RawRowClass::Build => {}
        }
        let Some(path_info) = inputs.resolver.resolve(row.path_id) else {
            batch.outcomes.push(BatchOutcome {
                path_id: row.path_id,
                block: inputs.current_block,
                assembly: AssemblyVerdict::SkipResolveMiss,
                simulate: None,
                submit: None,
                path_info: PathInfoView::empty(),
            });
            continue;
        };
        // The loud-abort shape guard + the candidate construction — one home
        // shared with the PyO3 assembly delegate ([`build_raw_candidate`]).
        let candidate = build_raw_candidate(row, &path_info, inputs.opts)?;
        path_info_by_id.insert(row.path_id, path_info.clone());
        batch.candidates.push(candidate);
    }

    // ── Pre-sim policy, each applied exactly once, in fan-out order ──────

    // Suppression. The read-only projection (no retry stamp) attributes the
    // skip so the STAMPING read stays single-sited in the fan-out's step 1: a
    // not-due suppressed path is skipped here (and again, identically, by the
    // fan-out), while a retry-due path is admitted by BOTH reads — the Python
    // retry semantics, with no double-read re-suppression. (`is_suppressed`
    // stamps `last_retry_block` as a side effect; calling it here AND in the
    // fan-out would re-suppress the path the first stamp just admitted.)
    let mut divergence = inputs
        .divergence
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut fot = inputs
        .fot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut survivors: Vec<DispatchCandidate> = Vec::with_capacity(batch.candidates.len());
    for c in batch.candidates.drain(..) {
        if inputs
            .suppression
            .is_suppressed_without_stamp(c.path_id, inputs.current_block)
        {
            batch.outcomes.push(BatchOutcome {
                path_id: c.path_id,
                block: inputs.current_block,
                assembly: AssemblyVerdict::SkipSuppressed,
                simulate: None,
                submit: None,
                path_info: PathInfoView::from(
                    path_info_by_id.get(&c.path_id).expect("resolved above"),
                ),
            });
        } else {
            survivors.push(c);
        }
    }

    // Pool divergence — drop candidates routing through a pool flagged
    // `SolverCalc` within the decay window; bump the lifetime tally per drop
    // (mirrors the fan-out's bookend so the registry's counter stays fed).
    let mut post_divergence: Vec<DispatchCandidate> = Vec::with_capacity(survivors.len());
    for c in survivors.drain(..) {
        let divergent = c
            .path_info
            .hops
            .iter()
            .filter_map(degenbot_arbitrage::hop_pool_key)
            .any(|k| divergence.is_divergent(k, inputs.current_block));
        if divergent {
            divergence.record_dropped();
            batch.outcomes.push(BatchOutcome {
                path_id: c.path_id,
                block: inputs.current_block,
                assembly: AssemblyVerdict::SkipDivergentPool,
                simulate: None,
                submit: None,
                path_info: PathInfoView::from(
                    path_info_by_id.get(&c.path_id).expect("resolved above"),
                ),
            });
        } else {
            post_divergence.push(c);
        }
    }

    // Fee-on-transfer — drop candidates whose any hop's input token is
    // FoT-confirmed (same lifetime-tally mirror).
    let mut post_fot: Vec<DispatchCandidate> = Vec::with_capacity(post_divergence.len());
    for c in post_divergence.drain(..) {
        let is_fot = c.path_info.hops.iter().any(|hop| {
            fot.is_fot(
                degenbot_arbitrage::hop_input_token(hop),
                inputs.current_block,
            )
        });
        if is_fot {
            fot.record_dropped();
            batch.outcomes.push(BatchOutcome {
                path_id: c.path_id,
                block: inputs.current_block,
                assembly: AssemblyVerdict::SkipFeeOnTransfer,
                simulate: None,
                submit: None,
                path_info: PathInfoView::from(
                    path_info_by_id.get(&c.path_id).expect("resolved above"),
                ),
            });
        } else {
            post_fot.push(c);
        }
    }

    // Thin margin — the core leaf, applied once (the fan-out re-applies as a
    // no-op on the survivors). The leaf consumes its input vec, so the
    // skip-attribution pass runs over a clone (bounded by the batch cap).
    let (mut kept, _dropped) = degenbot_arbitrage::filter_thin_margin_results(
        post_fot.clone(),
        inputs.min_profit_margin_bps,
    );
    let kept_ids: HashSet<u64> = kept.iter().map(|c| c.path_id).collect();
    for c in &post_fot {
        if !kept_ids.contains(&c.path_id) {
            batch.outcomes.push(BatchOutcome {
                path_id: c.path_id,
                block: inputs.current_block,
                assembly: AssemblyVerdict::SkipThinMargin,
                simulate: None,
                submit: None,
                path_info: PathInfoView::from(
                    path_info_by_id.get(&c.path_id).expect("resolved above"),
                ),
            });
        }
    }

    // Cap — the driver-injected plain count, clamped to the fan-out's own
    // ceiling so this is the ONLY truncation. Dropped rows produce no record
    // (the drain-invisible drop today's `truncate` applies — today's
    // candidate_count excludes them, so the record folds stay in parity).
    let cap = inputs
        .max_candidates
        .min(degenbot_arbitrage::MAX_SIMULATE_CONCURRENT);
    kept.truncate(cap);
    batch.candidates = kept;

    // ── Payload rows ─────────────────────────────────────────────────────
    // The already-simulated arm: join each payload to the submit-lane shape
    // and emit its record with the categorized simulate verdict. A resolve
    // miss here is decision (a)'s LOUD-ABORT arm (see
    // [`AssemblyVerdict::SkipResolveMiss`]): a payload row is engine-born —
    // the engine simulated the path inline — so a miss evidences
    // batch/registry divergence, and the pool keys could never be derived.
    for p in inputs.payloads {
        let Some(path_info) = inputs.resolver.resolve(p.path_id) else {
            return Err(AssemblyError {
                path_id: p.path_id,
                detail: format!(
                    "path_id {} is not registered in this engine; \
                     the payload pool keys cannot be derived",
                    p.path_id
                ),
            });
        };
        let view = PathInfoView::from(&path_info);
        if let Some(detail) = p.failure_detail() {
            batch.outcomes.push(BatchOutcome {
                path_id: p.path_id,
                block: inputs.current_block,
                assembly: AssemblyVerdict::Assembled,
                simulate: Some(SimulateVerdict::Failed(Box::new(detail))),
                submit: None,
                path_info: view,
            });
            continue;
        }
        let receipt = SimReceipt {
            gross_profit: p.gross_profit,
            net_profit: p.net_profit,
            gas_used: p.gas_used,
            priority_fee: p.priority_fee,
        };
        if degenbot_arbitrage::is_gas_profitable(p.net_profit) {
            batch.payload_submits.push(join_sim_result(
                &p.sim_result(),
                Some(&path_info),
                inputs.executor_address,
            ));
            batch.outcomes.push(BatchOutcome {
                path_id: p.path_id,
                block: inputs.current_block,
                assembly: AssemblyVerdict::Assembled,
                simulate: Some(SimulateVerdict::Profitable(receipt)),
                submit: None,
                path_info: view,
            });
        } else {
            batch.outcomes.push(BatchOutcome {
                path_id: p.path_id,
                block: inputs.current_block,
                assembly: AssemblyVerdict::Assembled,
                simulate: Some(SimulateVerdict::GasUnprofitable(receipt)),
                submit: None,
                path_info: view,
            });
        }
    }

    batch.path_info_by_id = path_info_by_id;
    Ok(batch)
}

/// Join a sim result + its originating path hops → a submit candidate (the
/// mutual-exclusion pool keys derive from the hops: V4 → the salted
/// `pool_id_hex`, V2/V3 → the EIP-55 Display form — the one keying rule both
/// entry arms share; the core owns the join and the `PyO3` seam drives it).
/// `path_info` is `None` only for a path id the fan-out could not recover
/// (an empty mutual-exclusion set — see the body).
#[must_use]
pub fn join_sim_result(
    r: &degenbot_arbitrage::SimResult,
    path_info: Option<&PathInfo>,
    executor_address: alloy::primitives::Address,
) -> SubmitCandidate {
    // `None` = the originating path could not be recovered (the core only
    // re-emits path ids it was handed) — a degenerate but safe fall-through:
    // isolation falls back to no mutual-exclusion for that one path.
    let path_pools: HashSet<PoolKey> = path_info
        .map(|p| derive_path_pools(&p.hops))
        .unwrap_or_default();
    SubmitCandidate {
        path_id: r.path_id,
        gross_profit: r.gross_profit,
        net_profit: r.net_profit,
        gas_used: r.gas_used, // UN-inflated (1.5× applied at submit time).
        priority_fee: r.priority_fee,
        base_fee_next: r.base_fee_next,
        execute_calldata: r.execute_calldata.clone(),
        executor_address,
        access_list: r.access_list.clone(),
        path_pools,
    }
}

/// Derive the mutual-exclusion pool-key set from a path's hops (V4 → the
/// `pool_id_hex` string; V2/V3 → the checksummed Display form).
#[must_use]
pub fn derive_path_pools(hops: &[degenbot_executor::composers::HopInfo]) -> HashSet<PoolKey> {
    hops.iter()
        .map(|h| match h {
            degenbot_executor::composers::HopInfo::V4(v4) => PoolKey::new(v4.pool_id_hex.clone()),
            degenbot_executor::composers::HopInfo::V2(v2) => {
                PoolKey::new(format!("{}", v2.pool_address))
            }
            degenbot_executor::composers::HopInfo::V3(v3) => {
                PoolKey::new(format!("{}", v3.pool_address))
            }
        })
        .collect()
}

/// The per-row classification of one raw solver row — the pre-resolve skip
/// predicates of stage 1 (one home shared by [`assemble_batch`] and the
/// `PyO3` assembly delegate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawRowClass {
    /// The row passed every pre-resolve predicate: resolve + build it.
    Build,
    /// Skip the row with this typed verdict (empty hops or payload-served).
    Skip(AssemblyVerdict),
}

/// Classify one raw solver row against the batch-local payload-served set —
/// the skip predicates that run BEFORE the path resolve.
#[must_use]
pub fn classify_raw_row<S: std::hash::BuildHasher>(
    row: &RawResult,
    payload_served: &HashSet<u64, S>,
) -> RawRowClass {
    if !row.has_hops() {
        return RawRowClass::Skip(AssemblyVerdict::SkipEmptyHops);
    }
    // Already simulated inline by the engine; its record comes from the
    // payload arm, not the sim batch.
    if payload_served.contains(&row.path_id) {
        return RawRowClass::Skip(AssemblyVerdict::SkipPayloadServed);
    }
    RawRowClass::Build
}

/// The loud-abort shape guard + candidate construction for one RESOLVED raw
/// row — the corruption arm of decision (a): per-hop lengths that disagree
/// with the resolved path's hop count prove the batch and the registry
/// diverged, and abort the whole batch in the caller's frame. The outputs/
/// inputs messages keep the historical wording both consumers' logs pinned.
///
/// # Errors
///
/// [`AssemblyError`] when any per-hop length mismatches the resolved hop
/// count.
pub fn build_raw_candidate(
    row: &RawResult,
    path_info: &PathInfo,
    opts: EncodeOptions,
) -> Result<DispatchCandidate, AssemblyError> {
    let hops = path_info.hops.len();
    if row.hop_outputs.len() != hops {
        return Err(AssemblyError {
            path_id: row.path_id,
            detail: format!(
                "hop_outputs length ({}) != path {} hops ({hops})",
                row.hop_outputs.len(),
                row.path_id
            ),
        });
    }
    if row.consumed_inputs.len() != hops {
        return Err(AssemblyError {
            path_id: row.path_id,
            detail: format!(
                "consumed_inputs length ({}) != path {} hops ({hops})",
                row.consumed_inputs.len(),
                row.path_id
            ),
        });
    }
    if row.state_nonces.len() != hops {
        return Err(AssemblyError {
            path_id: row.path_id,
            detail: format!(
                "state_nonces length ({}) != path {} hops ({hops})",
                row.state_nonces.len(),
                row.path_id
            ),
        });
    }
    Ok(row.to_candidate(path_info.clone(), opts))
}

/// The payload arm's per-entry categorization verdict (the driver seam
/// renders `"submit"` / `"unprofitable"` from it and reads nothing else —
/// the threshold comparison ran crate-side, once, for both entry arms).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadArm {
    /// Net profit reached the floor — the row joined to the submit lane.
    Submit,
    /// Onchain-valid but below the net threshold.
    Unprofitable,
}

/// The payload arm's merged record set: joined submit rows + per-entry
/// verdicts + failure rows + the resolved path infos (first-appearance order
/// over DISTINCT path ids — the render source). A pure-Rust projection with
/// no Python types: the `PyO3` seam wraps it, the pure-Rust bot reads it.
#[derive(Debug, Default)]
pub struct MergedPayloadOutcome {
    /// The joined submit rows (gas-profitable inline sims), in entry order.
    pub submits: Vec<SubmitCandidate>,
    /// Valid sims below the net threshold.
    pub unprofitable_count: usize,
    /// The failure rows (path id + the crate failure record).
    pub failures: Vec<(u64, PayloadFailure)>,
    /// Per-entry verdicts (non-failure entries, in entry order).
    pub verdicts: Vec<(u64, PayloadArm)>,
    /// The resolved path infos, first-appearance over distinct path ids.
    pub path_infos: Vec<(u64, PathInfo)>,
}

/// Merge the inline-sim payload rows (stage 1's payload arm, standalone):
/// resolve each row's path, join it to the submit-lane shape through
/// [`join_sim_result`], and categorize with the core's `is_gas_profitable`
/// predicate — the ONE rule both entry arms share.
///
/// Decision (a), payload arm: a resolve miss here is the LOUD-ABORT arm, not
/// the raw-row typed skip — a payload row is engine-born (the engine
/// simulated the path inline), so a miss evidences batch/registry divergence,
/// and the pool keys could never be derived.
///
/// # Errors
///
/// [`AssemblyError`] when a payload's `path_id` is not registered (the
/// corruption arm above).
pub fn merge_payload_results(
    payloads: &[PayloadRow],
    resolver: &dyn PathResolver,
    executor_address: alloy::primitives::Address,
) -> Result<MergedPayloadOutcome, AssemblyError> {
    let mut merged = MergedPayloadOutcome::default();
    let mut seen: HashSet<u64> = HashSet::with_capacity(payloads.len());
    for p in payloads {
        let Some(path_info) = resolver.resolve(p.path_id) else {
            return Err(AssemblyError {
                path_id: p.path_id,
                detail: format!(
                    "path_id {} is not registered in this engine; \
                     the payload pool keys cannot be derived",
                    p.path_id
                ),
            });
        };
        // One resolve per DISTINCT path id (multiple entries may share a
        // path), in first-appearance order.
        if seen.insert(p.path_id) {
            merged.path_infos.push((p.path_id, path_info.clone()));
        }
        if let Some(failure) = &p.failure {
            merged.failures.push((p.path_id, failure.clone()));
            continue;
        }
        let joined = join_sim_result(&p.sim_result(), Some(&path_info), executor_address);
        if degenbot_arbitrage::is_gas_profitable(p.net_profit) {
            merged.submits.push(joined);
            merged.verdicts.push((p.path_id, PayloadArm::Submit));
        } else {
            merged.unprofitable_count += 1;
            merged.verdicts.push((p.path_id, PayloadArm::Unprofitable));
        }
    }
    Ok(merged)
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests assert on known-valid inputs; parse_address fixtures are valid"
)]
mod tests {
    use super::*;
    use crate::row::{PayloadFailure, PayloadRow};
    use alloy::primitives::Address;
    use degenbot_arbitrage::{FeeOnTransferRegistry, PoolDivergence};
    use degenbot_core::address_utils::parse_address;
    use degenbot_executor::composers::{EncodeOptions, HopInfo, V2HopInfo};
    use degenbot_submission::PathSuppression;
    use std::sync::{Arc, Mutex};

    const EXECUTOR: Address =
        alloy::primitives::address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    fn v2_path() -> PathInfo {
        let pool = parse_address("0x1111111111111111111111111111111111111111").unwrap();
        let t0 = parse_address("0x2222222222222222222222222222222222222222").unwrap();
        let t1 = parse_address("0x3333333333333333333333333333333333333333").unwrap();
        PathInfo::new(vec![HopInfo::V2(V2HopInfo {
            pool_address: pool,
            token0_address: t0,
            token1_address: t1,
            fee: 30,
            zfo: true,
        })])
    }

    struct MapResolver(std::collections::HashMap<u64, PathInfo>);
    impl PathResolver for MapResolver {
        fn resolve(&self, path_id: u64) -> Option<PathInfo> {
            self.0.get(&path_id).cloned()
        }
    }

    fn resolver(paths: &[u64]) -> MapResolver {
        MapResolver(paths.iter().map(|&p| (p, v2_path())).collect())
    }

    fn row(path_id: u64, optimal_input: u128, profit: u128) -> RawResult {
        // One hop — matching the single-hop `v2_path` fixture (the shape
        // guard compares row lengths against the RESOLVED hop count).
        RawResult {
            path_id,
            optimal_input,
            profit,
            hop_outputs: vec![profit],
            consumed_inputs: vec![optimal_input],
            solve_block: 100,
            state_nonces: vec![1],
        }
    }

    struct Policy {
        suppression: Arc<Mutex<PathSuppression>>,
        divergence: Arc<Mutex<PoolDivergence>>,
        fot: Arc<Mutex<FeeOnTransferRegistry>>,
    }

    impl Policy {
        fn fresh() -> Self {
            Self {
                suppression: Arc::new(Mutex::new(PathSuppression::new())),
                divergence: Arc::new(Mutex::new(PoolDivergence::new())),
                fot: Arc::new(Mutex::new(FeeOnTransferRegistry::new())),
            }
        }
    }

    fn assemble(
        rows: &[RawResult],
        payloads: &[PayloadRow],
        res: &MapResolver,
        p: &Policy,
    ) -> Result<AssembledBatch, AssemblyError> {
        let served: HashSet<u64> = payloads.iter().map(|x| x.path_id).collect();
        let mut suppression = p.suppression.lock().unwrap();
        assemble_batch(&AssemblyInputs {
            rows,
            payloads,
            resolver: res,
            opts: EncodeOptions::default(),
            payload_served: &served,
            suppression: &mut suppression,
            divergence: &p.divergence,
            fot: &p.fot,
            current_block: 100,
            min_profit_margin_bps: 0,
            max_candidates: 50,
            executor_address: EXECUTOR,
        })
    }

    /// Decision (a): a resolve miss is a typed skip + counted — the unification
    /// of the `PyO3` seam's `ValueError` and the bot's `SkipEmptyHops` fold.
    #[test]
    fn resolve_miss_is_a_typed_skip_not_an_abort() {
        let res = resolver(&[]);
        let p = Policy::fresh();
        let rows = [row(7, 1_000, 100)];
        let batch = assemble(&rows, &[], &res, &p).unwrap();
        assert_eq!(batch.outcomes.len(), 1);
        assert_eq!(batch.outcomes[0].assembly, AssemblyVerdict::SkipResolveMiss);
        assert!(batch.candidates.is_empty());
    }

    /// Decision (a), corruption arm: a row whose hop lengths disagree with the
    /// resolved path's hop count proves the batch and registry diverged —
    /// loud abort, re-raised in the driver's frame.
    #[test]
    fn row_shape_mismatch_is_a_loud_abort() {
        let res = resolver(&[7]);
        let p = Policy::fresh();
        let mut bad = row(7, 1_000, 100);
        bad.hop_outputs.push(999); // 3 outputs vs 1 resolved hop
        let err = assemble(&[bad], &[], &res, &p).unwrap_err();
        assert_eq!(err.path_id, 7);
    }

    #[test]
    fn empty_hops_skip_before_any_policy() {
        let res = resolver(&[7]);
        let p = Policy::fresh();
        let mut r = row(7, 1_000, 100);
        r.hop_outputs.clear();
        r.consumed_inputs.clear();
        r.state_nonces.clear();
        let batch = assemble(&[r], &[], &res, &p).unwrap();
        assert_eq!(batch.outcomes[0].assembly, AssemblyVerdict::SkipEmptyHops);
    }

    #[test]
    fn payload_served_row_is_skipped_and_the_payload_row_carries_the_record() {
        let res = resolver(&[7]);
        let p = Policy::fresh();
        let payload = PayloadRow {
            path_id: 7,
            gross_profit: alloy::primitives::U256::from(600_u64),
            net_profit: alloy::primitives::U256::from(500_u64),
            gas_used: 300_000,
            priority_fee: 2,
            base_fee_next: 30,
            execute_calldata: alloy::primitives::Bytes::from_static(&[0xab]),
            access_list: None,
            failure: None,
        };
        let batch = assemble(&[row(7, 1_000, 100)], &[payload], &res, &p).unwrap();
        let verdicts: Vec<AssemblyVerdict> = batch.outcomes.iter().map(|o| o.assembly).collect();
        assert!(verdicts.contains(&AssemblyVerdict::SkipPayloadServed));
        // The payload arm produced the joined submit row.
        assert_eq!(batch.payload_submits.len(), 1);
        assert_eq!(batch.payload_submits[0].path_id, 7);
        // Its pool keys derive from the resolved hops.
        assert!(!batch.payload_submits[0].path_pools.is_empty());
        // And its record carries the profitable receipt.
        let payload_record = batch
            .outcomes
            .iter()
            .find(|o| o.assembly == AssemblyVerdict::Assembled)
            .expect("payload record");
        assert!(matches!(
            payload_record.simulate,
            Some(SimulateVerdict::Profitable(_))
        ));
    }

    #[test]
    fn suppressed_path_skips_and_the_retry_read_happens_once() {
        let res = resolver(&[5]);
        let p = Policy::fresh();
        for _ in 0..degenbot_submission::PATH_SUPPRESS_THRESHOLD {
            p.suppression.lock().unwrap().record_failure(5);
        }
        // Due for a retry at the interval boundary: the single read stamps the
        // retry and ADMITS the candidate (Python retry semantics) — a second
        // read (as the pre-filter + fan-out double-read would do) would
        // re-suppress it. The assembly must read once.
        let batch = assemble(&[row(5, 1_000, 10_000)], &[], &res, &p).unwrap();
        assert_eq!(batch.candidates.len(), 1);
        assert!(batch
            .outcomes
            .iter()
            .all(|o| o.assembly != AssemblyVerdict::SkipSuppressed));

        // A NOT-due suppressed path (inside the retry interval, block 50 <
        // last_retry 0 + interval) skips — and because the projection is
        // no-stamp, a LATER batch at the due block still admits the retry.
        // Only the suppression lock is held here (assemble_batch locks the
        // divergence/FoT registries itself — pre-locking them deadlocks).
        let served: HashSet<u64> = HashSet::new();
        {
            let mut suppression = p.suppression.lock().unwrap();
            let batch = assemble_batch(&AssemblyInputs {
                rows: &[row(5, 1_000, 10_000)],
                payloads: &[],
                resolver: &res,
                opts: EncodeOptions::default(),
                payload_served: &served,
                suppression: &mut suppression,
                divergence: &p.divergence,
                fot: &p.fot,
                current_block: 50,
                min_profit_margin_bps: 0,
                max_candidates: 50,
                executor_address: EXECUTOR,
            })
            .unwrap();
            assert_eq!(batch.outcomes[0].assembly, AssemblyVerdict::SkipSuppressed);
            assert!(batch.candidates.is_empty());
        }
        // The guard is scoped: the retry-due read at the later block admits.
        let batch = assemble(&[row(5, 1_000, 10_000)], &[], &res, &p).unwrap();
        assert_eq!(batch.candidates.len(), 1);
    }

    #[test]
    fn thin_margin_rows_skip() {
        let res = resolver(&[1, 2]);
        let p = Policy::fresh();
        // 100/1_000_000 = 1 bps → dropped at a 50 bps floor; 10_000/1_000_000
        // = 100 bps → kept.
        let rows = [row(1, 1_000_000, 100), row(2, 1_000_000, 10_000)];
        let served: HashSet<u64> = HashSet::new();
        let mut suppression = p.suppression.lock().unwrap();
        let batch = assemble_batch(&AssemblyInputs {
            rows: &rows,
            payloads: &[],
            resolver: &res,
            opts: EncodeOptions::default(),
            payload_served: &served,
            suppression: &mut suppression,
            divergence: &p.divergence,
            fot: &p.fot,
            current_block: 100,
            min_profit_margin_bps: 50,
            max_candidates: 50,
            executor_address: EXECUTOR,
        })
        .unwrap();
        let by_id: std::collections::HashMap<u64, AssemblyVerdict> = batch
            .outcomes
            .iter()
            .map(|o| (o.path_id, o.assembly))
            .collect();
        assert_eq!(by_id[&1], AssemblyVerdict::SkipThinMargin);
        // The KEPT row emits no record at stage 1 (its simulate/submit
        // verdicts arrive only after the fan-out) — drain-invisible here.
        assert!(!by_id.contains_key(&2));
        assert_eq!(batch.candidates.len(), 1);
        assert_eq!(batch.candidates[0].path_id, 2);
    }

    #[test]
    fn cap_truncates_silently_matching_today_s_drain_invisible_drop() {
        let res = resolver(&[1, 2, 3]);
        let p = Policy::fresh();
        let rows = [
            row(1, 1_000_000, 10_000),
            row(2, 1_000_000, 10_000),
            row(3, 1_000_000, 10_000),
        ];
        let served: HashSet<u64> = HashSet::new();
        let mut suppression = p.suppression.lock().unwrap();
        let batch = assemble_batch(&AssemblyInputs {
            rows: &rows,
            payloads: &[],
            resolver: &res,
            opts: EncodeOptions::default(),
            payload_served: &served,
            suppression: &mut suppression,
            divergence: &p.divergence,
            fot: &p.fot,
            current_block: 100,
            min_profit_margin_bps: 0,
            max_candidates: 2,
            executor_address: EXECUTOR,
        })
        .unwrap();
        assert_eq!(batch.candidates.len(), 2);
        // Rows the cap dropped produce NO record — the same drain-invisible
        // drop today's truncate applies (candidate_count excludes them).
        assert!(batch.outcomes.is_empty());
    }

    #[test]
    fn payload_failure_row_yields_a_failed_record() {
        let res = resolver(&[9]);
        let p = Policy::fresh();
        let payload = PayloadRow {
            path_id: 9,
            gross_profit: alloy::primitives::U256::ZERO,
            net_profit: alloy::primitives::U256::ZERO,
            gas_used: 0,
            priority_fee: 0,
            base_fee_next: 0,
            execute_calldata: alloy::primitives::Bytes::new(),
            access_list: None,
            failure: Some(PayloadFailure {
                bucket: "inline-fail".to_string(),
                fail_index: None,
                revert_data: alloy::primitives::Bytes::new(),
            }),
        };
        let batch = assemble(&[], &[payload], &res, &p).unwrap();
        assert_eq!(batch.outcomes.len(), 1);
        assert!(matches!(
            batch.outcomes[0].simulate,
            Some(SimulateVerdict::Failed(_))
        ));
    }

    // ── Moved from the PyO3 seam (the join's one home is here now) ────────

    fn v2_hop(addr: &str) -> degenbot_executor::composers::HopInfo {
        degenbot_executor::composers::HopInfo::V2(V2HopInfo {
            pool_address: addr.parse().unwrap(),
            token0_address: addr.parse().unwrap(),
            token1_address: addr.parse().unwrap(),
            fee: 30,
            zfo: true,
        })
    }

    fn v3_hop(addr: &str) -> degenbot_executor::composers::HopInfo {
        degenbot_executor::composers::HopInfo::V3(degenbot_executor::composers::V3HopInfo {
            pool_address: addr.parse().unwrap(),
            token0_address: addr.parse().unwrap(),
            token1_address: addr.parse().unwrap(),
            fee: 3000,
            zfo: true,
        })
    }

    fn v4_hop(pool_id_hex: &str) -> degenbot_executor::composers::HopInfo {
        degenbot_executor::composers::HopInfo::V4(degenbot_executor::composers::V4HopInfo {
            pool_manager_address: "0x000000000004444c5dc75cb358380d2e3de08a90"
                .parse()
                .unwrap(),
            pool_id_hex: pool_id_hex.to_string(),
            currency0_address: "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
                .parse()
                .unwrap(),
            currency1_address: "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
                .parse()
                .unwrap(),
            fee: 500,
            tick_spacing: 10,
            hook_address: alloy::primitives::Address::ZERO,
            zfo: true,
        })
    }

    /// Byte-identity of the keying rule: the V4 key is the `pool_id_hex`
    /// string and the V2/V3 key the EIP-55 (checksummed) Display form —
    /// the exact sets the old Python set-comprehension mirrored. Mixed-family
    /// paths key the same on both entry arms (both call THIS walk).
    #[test]
    fn derive_path_pools_mixed_families_byte_identity() {
        let checksum_addr = alloy::primitives::address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
        assert_eq!(
            format!("{checksum_addr}"),
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
        );
        let v4_id = "0x4f88f7c99022eace4740c6898f59ce6a2e798a1e64ce54589720b7153eb224a7";

        let hops_v4v2 = vec![
            v4_hop(v4_id),
            v2_hop("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"),
        ];
        let hops_v3 = vec![
            v3_hop("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"),
            v2_hop("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
        ];

        let set_v4v2 = derive_path_pools(&hops_v4v2);
        let set_v3 = derive_path_pools(&hops_v3);
        assert!(set_v4v2.contains(&PoolKey::new(v4_id)));
        assert!(set_v4v2.contains(&PoolKey::new("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")));
        assert!(set_v3.contains(&PoolKey::new("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")));
        assert!(set_v3.contains(&PoolKey::new("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")));
        assert_eq!(set_v4v2.len(), 2);
        assert_eq!(set_v3.len(), 2);
        let only_v4: std::collections::HashSet<_> = set_v4v2
            .difference(&set_v3)
            .map(PoolKey::to_string)
            .collect();
        assert_eq!(only_v4, std::iter::once(v4_id.to_string()).collect());
    }

    /// Join entry-point parity: a payload `SimResult` reassembled row and an
    /// FFI survivor for the same path id pass through the SAME
    /// [`join_sim_result`] — the `path_pools` sets are equal by construction.
    #[test]
    fn join_sim_result_sets_are_entry_independent() {
        let v4_id = "0x4f88f7c99022eace4740c6898f59ce6a2e798a1e64ce54589720b7153eb224a7";
        let path_info = PathInfo::new(vec![
            v4_hop(v4_id),
            v2_hop("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"),
        ]);
        let sim = degenbot_arbitrage::SimResult {
            path_id: 7,
            gross_profit: alloy::primitives::U256::from(600_000_000_000_u64),
            net_profit: alloy::primitives::U256::from(500_000_000_000_u64),
            gas_used: 300_000,
            priority_fee: 2,
            base_fee_next: 30,
            execute_calldata: alloy::primitives::Bytes::from(vec![0xab, 0x58, 0x98, 0xe8, 0x01]),
            access_list: None,
            captured_swaps: Vec::new(),
            hop_count: 2,
        };
        let executor = "0x690b9a9e9aa1c9db991c7721a92d351db4fac990"
            .parse()
            .unwrap();
        // `None` (the unrecoverable-path fall-through) yields an empty set;
        // `Some` yields the derived set — both arms call this one fn.
        let degenerate = join_sim_result(&sim, None, executor);
        assert!(degenerate.path_pools.is_empty());
        let row = join_sim_result(&sim, Some(&path_info), executor);
        assert!(row.path_pools.contains(&PoolKey::new(v4_id)));
        assert!(row
            .path_pools
            .contains(&PoolKey::new("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")));
        assert_eq!(row.path_pools.len(), 2);
        assert_eq!(row.gross_profit, sim.gross_profit);
        assert_eq!(row.net_profit, sim.net_profit);
        assert_eq!(row.gas_used, sim.gas_used);
        assert_eq!(row.priority_fee, sim.priority_fee);
        assert_eq!(row.base_fee_next, sim.base_fee_next);
        assert_eq!(row.access_list, None);
        assert_eq!(row.execute_calldata, sim.execute_calldata);
    }

    /// The standalone payload merge: submit + unprofitable categorization and
    /// the failure rows, one rule for both entry arms.
    #[test]
    fn merge_payload_results_categorizes_and_joins() {
        // Every payload path must resolve — including the failure row's
        // (engine-born rows; the merge's loud-abort arm fires otherwise).
        let res = resolver(&[7, 8, 9]);
        let p = Policy::fresh();
        let _ = p;
        let payloads = [
            PayloadRow {
                path_id: 7,
                gross_profit: alloy::primitives::U256::from(600_u64),
                net_profit: alloy::primitives::U256::from(500_u64),
                gas_used: 300_000,
                priority_fee: 2,
                base_fee_next: 30,
                execute_calldata: alloy::primitives::Bytes::from_static(&[0xab]),
                access_list: None,
                failure: None,
            },
            PayloadRow {
                path_id: 8,
                gross_profit: alloy::primitives::U256::from(600_u64),
                net_profit: alloy::primitives::U256::ZERO,
                gas_used: 300_000,
                priority_fee: 2,
                base_fee_next: 30,
                execute_calldata: alloy::primitives::Bytes::from_static(&[0xab]),
                access_list: None,
                failure: None,
            },
            PayloadRow {
                path_id: 9,
                gross_profit: alloy::primitives::U256::ZERO,
                net_profit: alloy::primitives::U256::ZERO,
                gas_used: 0,
                priority_fee: 0,
                base_fee_next: 0,
                execute_calldata: alloy::primitives::Bytes::new(),
                access_list: None,
                failure: Some(PayloadFailure {
                    bucket: "inline-fail".to_string(),
                    fail_index: None,
                    revert_data: alloy::primitives::Bytes::new(),
                }),
            },
        ];
        let merged = merge_payload_results(&payloads, &res, EXECUTOR).unwrap();
        // The profitable row joined (pool keys derived from the resolved hops).
        assert_eq!(merged.submits.len(), 1);
        assert_eq!(merged.submits[0].path_id, 7);
        assert!(!merged.submits[0].path_pools.is_empty());
        // The zero-profit row counted unprofitable; the failure row surfaced.
        assert_eq!(merged.unprofitable_count, 1);
        assert_eq!(merged.verdicts.len(), 2);
        assert_eq!(merged.verdicts[0], (7, PayloadArm::Submit));
        assert_eq!(merged.verdicts[1], (8, PayloadArm::Unprofitable));
        assert_eq!(merged.failures.len(), 1);
        assert_eq!(merged.failures[0].0, 9);
        // Path infos in first-appearance order over DISTINCT ids (including
        // the failure row's — the render source covers it).
        let ids: Vec<u64> = merged.path_infos.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![7, 8, 9]);
    }

    /// Decision (a), payload arm: a payload row is engine-born, so a resolve
    /// miss evidences batch/registry divergence — the LOUD-ABORT arm (the
    /// raw-row arm above stays the typed skip).
    #[test]
    fn merge_payload_resolve_miss_is_a_loud_abort() {
        let res = resolver(&[]);
        let payloads = [PayloadRow {
            path_id: 11,
            gross_profit: alloy::primitives::U256::from(600_u64),
            net_profit: alloy::primitives::U256::from(500_u64),
            gas_used: 300_000,
            priority_fee: 2,
            base_fee_next: 30,
            execute_calldata: alloy::primitives::Bytes::from_static(&[0xab]),
            access_list: None,
            failure: None,
        }];
        let err = merge_payload_results(&payloads, &res, EXECUTOR).unwrap_err();
        assert_eq!(err.path_id, 11);
        assert!(err.detail.contains("not registered in this engine"));
    }
}
