//! The discovery→registration pipeline — parity-ledger rows 9 + 12 + 13
//! (ergo `XFEJUG`).
//!
//! Mirrors `src/degenbot/runner/build_paths.py`:
//! `PathRegistrationPipeline._registration_unit` (the per-path build/verify/
//! register unit), `resolve_directions`, the D7KMQO policy gate, the W73FVY
//! dup fast-path, and the `_absorb_outcome` counter fold; plus the
//! [`RegistrationLedger`](crate::ledger::RegistrationLedger) memos and the
//! [`VerifyClaims`](crate::claims::VerifyClaims) at-most-once verify window.
//!
//! Two modes:
//! - **offline-dry** ([`run_offline`]): enumeration + direction resolution +
//!   policy + candidate counts only — no RPC, no build, no registration.
//!   This is the CI acceptance path.
//! - **live** (gated on `SMOKE_RPC_URL`; see `main.rs`): per-candidate pool
//!   build through `ConstructionIo`, `BotState` registration, the ADR-022
//!   verify lifecycle under the claim table, then
//!   `EngineDriver::register_and_solve_path`.

use std::collections::BTreeMap;

use degenbot::pathfinding::PoolKind;

use crate::claims::{VerificationError, VerifyClaims, VerifyErrorKind};
use crate::discovery::{BuiltGraph, DiscoveryParams, PoolNode, NATIVE_CURRENCY};
use crate::ledger::{HopSignature, RegistrationLedger, RegistrationOutcome};
use crate::policy::{HopView, PathPolicy};
use crate::retry::RetryPolicy;

/// The registration unit outcome (mirrors `RegistrationUnitOutcome`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CandidateOutcome {
    /// A benign build/direction skip with its bounded tag.
    Skip {
        /// The bounded tag.
        outcome: RegistrationOutcome,
        /// Whether the skip adds to `skip_count`.
        counts_as_skip: bool,
    },
    /// A counted engine rejection.
    Reject {
        /// The bounded tag.
        outcome: RegistrationOutcome,
        /// Detail text.
        detail: Option<String>,
    },
    /// The benign registered-path-cap stop.
    Cap,
    /// A transient register failure.
    RegisterFailed {
        /// Detail text.
        detail: String,
    },
    /// A completed registration (`created == false` = engine dedup).
    Registered {
        /// Whether a NEW path id was created.
        created: bool,
    },
}

/// A path that passed every driver gate and is ready for the registration
/// stage.
#[derive(Clone, Debug)]
pub struct PreparedCandidate {
    /// Node indices (into [`BuiltGraph::nodes`]) in path order.
    pub pool_indices: Vec<usize>,
    /// The oriented hop views.
    pub hops: Vec<HopView>,
    /// The hop signature (engine pool ids once live; graph ids offline).
    pub hop_sig: HopSignature,
    /// The per-hop directions (parallel to `pool_indices`).
    pub zfos: Vec<bool>,
    /// The number of V4 hops (the `v4_pool_count` witness).
    pub v4_hops: usize,
}

/// The result of the pre-registration preparation stage.
#[derive(Clone, Debug)]
pub enum PrepareOutcome {
    /// Ready for build/verify/register.
    Ready(PreparedCandidate),
    /// A benign skip (unregistrable memo, unknown type, no hash, direction
    /// mismatch).
    Skip(CandidateOutcome),
    /// A counted rejection (path-rejected memo / policy deny).
    Reject(CandidateOutcome),
    /// A fatal direction-resolution failure (subgraph vs constructed pool
    /// disagreement) — the Python pipeline aborts loudly.
    DirectionFatal(String),
}

/// The summary counters (mirrors the `PathRegistrationPipeline` summary).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PipelineReport {
    /// New paths registered.
    pub path_count: usize,
    /// Total skips.
    pub skip_count: usize,
    /// Benign post-cap skips.
    pub cap_skip_count: usize,
    /// Token-filtered paths.
    pub token_filter_count: usize,
    /// Counted engine rejections.
    pub engine_reject_count: usize,
    /// Engine-dedup duplicates.
    pub dup_count: usize,
    /// Transient register failures.
    pub register_fail_count: usize,
    /// V4 hops that reached the registration stage.
    pub v4_pool_count: usize,
    /// V4 hook rejections.
    pub v4_hook_rejected: usize,
    /// V4 dynamic-fee rejections.
    pub v4_dynamic_fee_rejected: usize,
    /// Other counted exceptions.
    pub other_exc_count: usize,
    /// Paths that passed the policy gate (the offline candidate witness).
    pub candidates: usize,
    /// Paths that failed direction resolution.
    pub direction_errors: usize,
    /// Paths rejected by the driver policy.
    pub policy_rejected: usize,
    /// Whether the benign registered-path cap was hit.
    pub capped: bool,
    /// Reason-tagged skip breakdown.
    pub skip_reasons: BTreeMap<String, usize>,
}

/// The reusable registration pipeline (mirrors `PathRegistrationPipeline`).
#[derive(Debug)]
pub struct RegistrationPipeline {
    /// The four memos + typed build-refusal classification.
    pub ledger: RegistrationLedger,
    /// The at-most-once verify-claim table.
    pub verify_claims: VerifyClaims,
    /// The driver path-composition policy (row 13).
    pub policy: PathPolicy,
    /// The transient-verify retry policy.
    pub retry_policy: RetryPolicy,
}

impl RegistrationPipeline {
    /// Build a pipeline with the driver defaults.
    #[must_use]
    pub fn new(policy: PathPolicy, retry_policy: RetryPolicy) -> Self {
        Self {
            ledger: RegistrationLedger::default(),
            verify_claims: VerifyClaims::new(),
            policy,
            retry_policy,
        }
    }

    /// Resolve per-hop directions so the cycle closes (mirrors
    /// `resolve_directions`).
    ///
    /// V4 native currency (`address(0)`) is treated as WETH for matching.
    /// Returns `Err(message)` when a mid-path token mismatch or a
    /// non-closing cycle is found (the fatal invariant).
    ///
    /// # Errors
    ///
    /// Returns the Python `DirectionResolutionError` message on mismatch.
    pub fn resolve_directions(
        nodes: &[PoolNode],
        pool_indices: &[usize],
        input_token_lower: &str,
        weth_lower: &str,
    ) -> Result<Vec<bool>, String> {
        let start = input_token_lower;
        let mut addr = start.to_string();
        let mut zfos: Vec<bool> = Vec::with_capacity(pool_indices.len());
        let len = pool_indices.len();
        for (i, idx) in pool_indices.iter().enumerate() {
            let node = &nodes[*idx];
            let mut token0 = normalize_native(node, &node.token0, weth_lower);
            let mut token1 = normalize_native(node, &node.token1, weth_lower);
            if node.kind == PoolKind::V4 {
                if token0 == NATIVE_CURRENCY {
                    token0 = weth_lower.to_string();
                }
                if token1 == NATIVE_CURRENCY {
                    token1 = weth_lower.to_string();
                }
            }
            let zfo = if token0 == addr {
                true
            } else if token1 == addr {
                false
            } else {
                return Err(format!(
                    "hop {i}/{len}: pool {} has token0={token0} token1={token1}; expected either to carry the tracked input token {addr} (path starts at {start})",
                    node.identity,
                ));
            };
            addr = if zfo { token1 } else { token0 };
            zfos.push(zfo);
        }
        if addr != start {
            return Err(format!(
                "cycle does not close: final output {addr} != input {start}"
            ));
        }
        Ok(zfos)
    }

    /// Run the pre-registration stages (mirrors `_registration_unit` up to the
    /// build/verify/register turns).
    #[must_use]
    pub fn prepare_candidate(
        &mut self,
        built: &BuiltGraph,
        path: &[(u64, PoolKind)],
        input_token_lower: &str,
        weth_lower: &str,
    ) -> PrepareOutcome {
        let mut pool_indices: Vec<usize> = Vec::with_capacity(path.len());
        let mut kinds: Vec<PoolKind> = Vec::with_capacity(path.len());
        for (graph_id, kind) in path {
            let Some(idx) = built.by_graph_id.get(graph_id).copied() else {
                return PrepareOutcome::Skip(CandidateOutcome::Skip {
                    outcome: RegistrationOutcome::UnknownPoolType,
                    counts_as_skip: true,
                });
            };
            if built.nodes[idx].kind != *kind {
                return PrepareOutcome::Skip(CandidateOutcome::Skip {
                    outcome: RegistrationOutcome::UnknownPoolType,
                    counts_as_skip: true,
                });
            }
            pool_indices.push(idx);
            kinds.push(*kind);
        }

        // Unregistrable-pool memo: a stable-refused hop answers before any
        // build/verify (mirrors the ledger ask).
        for (idx, kind) in pool_indices.iter().zip(kinds.iter()) {
            let node = &built.nodes[*idx];
            if node.kind == PoolKind::V4 && node.pool_hash.is_none() {
                return PrepareOutcome::Skip(CandidateOutcome::Skip {
                    outcome: RegistrationOutcome::V4NoHash,
                    counts_as_skip: true,
                });
            }
            let memo_key = RegistrationLedger::pool_memo_key(
                *kind,
                node.address.as_deref(),
                node.pool_hash.as_deref(),
            );
            if let Some(record) = self.ledger.unregistrable_record(memo_key.as_deref()) {
                return PrepareOutcome::Skip(CandidateOutcome::Skip {
                    outcome: record.outcome,
                    counts_as_skip: record.counts_as_skip,
                });
            }
        }

        let zfos = match Self::resolve_directions(
            &built.nodes,
            &pool_indices,
            input_token_lower,
            weth_lower,
        ) {
            Ok(zfos) => zfos,
            Err(message) => return PrepareOutcome::DirectionFatal(message),
        };

        let hops: Vec<HopView> = pool_indices
            .iter()
            .zip(zfos.iter())
            .map(|(idx, zfo)| {
                let node = &built.nodes[*idx];
                let (token_in, token_out) = if *zfo {
                    (node.token0.clone(), node.token1.clone())
                } else {
                    (node.token1.clone(), node.token0.clone())
                };
                HopView {
                    pool_identity: node.identity.clone(),
                    pool_kind: node.kind,
                    token_in,
                    token_out,
                }
            })
            .collect();

        // Offline stand-in for the engine hop id: the graph id. The live path
        // replaces this with the BotState pool id before registering.
        let hop_sig: HopSignature = path
            .iter()
            .zip(zfos.iter())
            .map(|((graph_id, _), zfo)| (*graph_id, *zfo))
            .collect();
        let v4_hops = kinds.iter().filter(|k| **k == PoolKind::V4).count();

        // Deterministic reject memo (D7KMQO gate deny).
        if self.ledger.path_rejected(&hop_sig) {
            return PrepareOutcome::Reject(CandidateOutcome::Reject {
                outcome: RegistrationOutcome::PathRejected,
                detail: None,
            });
        }

        // Policy gate: hop bounds, allow/deny, duplicate pools.
        if let Err(rejection) = self.policy.evaluate(&hops) {
            self.ledger.memoize_rejected_path(hop_sig);
            return PrepareOutcome::Reject(CandidateOutcome::Reject {
                outcome: RegistrationOutcome::PathRejected,
                detail: Some(rejection.to_string()),
            });
        }

        // Dup fast-path (W73FVY): answer registered signatures before verify.
        if self.ledger.path_registered(&hop_sig) {
            return PrepareOutcome::Skip(CandidateOutcome::Registered { created: false });
        }

        PrepareOutcome::Ready(PreparedCandidate {
            pool_indices,
            hops,
            hop_sig,
            zfos,
            v4_hops,
        })
    }

    /// Fold one unit outcome into the report (mirrors `_absorb_outcome`).
    pub fn absorb(report: &mut PipelineReport, outcome: &CandidateOutcome, v4_hops: usize) {
        match outcome {
            CandidateOutcome::Skip {
                outcome,
                counts_as_skip,
            } => {
                if *counts_as_skip {
                    report.skip_count += 1;
                }
                match outcome {
                    RegistrationOutcome::V4HookRejected => report.v4_hook_rejected += 1,
                    RegistrationOutcome::V4DynamicFeeRejected => {
                        report.v4_dynamic_fee_rejected += 1;
                    }
                    _ => {}
                }
                *report
                    .skip_reasons
                    .entry(outcome.as_str().to_string())
                    .or_insert(0) += 1;
            }
            CandidateOutcome::Reject { outcome, detail } => {
                report.engine_reject_count += 1;
                report.other_exc_count += 1;
                let tag = detail.as_deref().map_or_else(
                    || outcome.as_str().to_string(),
                    |d| format!("{}:{d}", outcome.as_str()),
                );
                *report.skip_reasons.entry(tag).or_insert(0) += 1;
            }
            CandidateOutcome::RegisterFailed { detail } => {
                report.register_fail_count += 1;
                *report
                    .skip_reasons
                    .entry(RegistrationOutcome::RegisterFailed.as_str().to_string())
                    .or_insert(0) += 1;
                // RSP-14: the single choke point every
                // CandidateOutcome::RegisterFailed funnels through — the
                // verify folds in live::verify_one return before the
                // register_and_solve_path arm, so sampling at the producer
                // sites would miss them. Env-gated + cardinality-bounded.
                crate::live::emit_register_failure_sample(detail);
            }
            CandidateOutcome::Cap => {
                report.skip_count += 1;
                report.cap_skip_count += 1;
                report.capped = true;
                *report
                    .skip_reasons
                    .entry(RegistrationOutcome::PathCap.as_str().to_string())
                    .or_insert(0) += 1;
            }
            CandidateOutcome::Registered { created } => {
                report.v4_pool_count += v4_hops;
                if *created {
                    report.path_count += 1;
                } else {
                    report.dup_count += 1;
                    *report
                        .skip_reasons
                        .entry(RegistrationOutcome::Dup.as_str().to_string())
                        .or_insert(0) += 1;
                }
            }
        }
    }
}

fn normalize_native(node: &PoolNode, token: &str, weth_lower: &str) -> String {
    if node.kind == PoolKind::V4 && token == NATIVE_CURRENCY {
        weth_lower.to_string()
    } else {
        token.to_string()
    }
}

/// The offline-dry pipeline: walk the batched enumeration, apply directions +
/// policy, and count candidates. No RPC, no build, no registration.
///
/// This is the CI acceptance path (parity: outcomes comparable to the Python
/// driver's build/skip counters with the build/verify/register turns removed).
pub async fn run_offline(
    pipeline: &mut RegistrationPipeline,
    built: &BuiltGraph,
    params: &DiscoveryParams,
    input_token_lower: &str,
    weth_lower: &str,
) -> PipelineReport {
    let mut report = PipelineReport::default();
    let mut finder = crate::discovery::BatchedPathFinder::new(&built.graph, params);
    while let Some(batch) = finder.next_batch() {
        for path in &batch {
            let outcome = pipeline.prepare_candidate(built, path, input_token_lower, weth_lower);
            match outcome {
                PrepareOutcome::Ready(candidate) => {
                    report.candidates += 1;
                    report.v4_pool_count += candidate.v4_hops;
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
    // The driver applies the token allowlist at the GRAPH filter (mirroring
    // `find_paths_async`'s `allowed_intermediate_tokens`), so the policy gate
    // never sees a token-filter rejection by default; `token_filter_count`
    // stays the Python counter it is (0 unless a `PathPolicy` allow/deny set is
    // configured).
    report
}

/// Run a verification lifecycle under the at-most-once claim table with the
/// bounded retry dance (mirrors `_SeatVerifyClaims.run_exclusive`): only
/// [`VerifyErrorKind::Rpc`] failures are retried.
///
/// `lifecycle` is invoked with the 1-indexed attempt number; only
/// [`VerifyErrorKind::Rpc`] failures are retried.
///
/// # Errors
///
/// Returns the last failure after exhausting attempts, or a fatal immediately.
pub async fn run_verification_with_claims<F, Fut>(
    pipeline: &RegistrationPipeline,
    claim_key: &str,
    lifecycle: F,
) -> Result<(), VerificationError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), VerificationError>>,
{
    pipeline
        .verify_claims
        .run_exclusive(claim_key, lifecycle)
        .await
}

/// Convenience: a `VerificationError` constructor for a mismatch (fatal).
#[must_use]
pub fn mismatch_error(message: impl Into<String>) -> VerificationError {
    VerificationError::new(VerifyErrorKind::Mismatch, message)
}

/// Convenience: a `VerificationError` constructor for a transient RPC failure.
#[must_use]
pub fn rpc_error(message: impl Into<String>) -> VerificationError {
    VerificationError::new(VerifyErrorKind::Rpc, message)
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests assert on known-valid fixture contents"
)]
mod tests {
    use super::*;
    use crate::discovery::{build_graph, DiscoveryParams, V4_POOL_ID_OFFSET};
    use crate::policy::PathPolicy;
    use crate::retry::RetryPolicy;
    use degenbot::pathfinding::PoolKind;
    use std::path::Path;

    /// The G2 discovery fixture (chain 8453: one V3 pool + one V4 pool).
    fn fixture_path() -> String {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../crates/degenbot-db/tests/fixtures/parity.db"
        )
        .to_string()
    }

    /// A private copy of the chain-8453 parity fixture for one test.
    ///
    /// The committed fixture is Alembic-head-stamped, so the first
    /// `DegenbotDb::open` heals it in place: the embedded DDL is written to a
    /// `.heal-tmp` sidecar, the file is atomically swapped, and a `.bak`
    /// backup is left behind. The two fixture tests run on parallel test
    /// threads; opening the shared repo file from both raced that heal
    /// (`SQLITE_IOERR` / "index ... already exists" / "database disk image is
    /// malformed") — the intermittent `open fixture db` failure at
    /// pipeline.rs:545. Each test heals its own copy instead, so the committed
    /// fixture bytes stay historical and no two opens share a file.
    struct FixtureCopy {
        path: std::path::PathBuf,
    }

    impl FixtureCopy {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "settlement-pipeline-fixture-{}-{}.db",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::copy(Path::new(&fixture_path()), &path).expect("copy parity fixture");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for FixtureCopy {
        fn drop(&mut self) {
            for suffix in ["", "-shm", "-wal", ".bak", ".bak-shm", ".bak-wal"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
            }
        }
    }

    #[test]
    fn graph_build_reads_the_parity_fixture_with_v4_offset() {
        let fixture = FixtureCopy::new();
        let (db, _schema) =
            degenbot::db::DegenbotDb::open(fixture.path()).expect("open fixture db");
        let rows = db.fetch_discovery_rows(8453).expect("fetch discovery rows");
        assert_eq!(rows.len(), 2, "fixture has one V3 + one V4 pool");

        let built = build_graph(&rows, &[PoolKind::V2, PoolKind::V3, PoolKind::V4], None);
        assert_eq!(built.nodes.len(), 2);
        assert_eq!(built.token_id_by_lower.len(), 3);

        let v4 = built
            .nodes
            .iter()
            .find(|node| node.kind == PoolKind::V4)
            .expect("V4 node present");
        assert_eq!(v4.raw_id, 1);
        assert_eq!(v4.graph_id, 1 + V4_POOL_ID_OFFSET);
        assert!(v4.pool_hash.is_some());
        assert_eq!(v4.identity, v4.pool_hash.clone().unwrap());

        // Degree filter: only token 1 is incident to >= 2 edges, so the two
        // edges are filtered out and the graph is empty (exact parity with
        // `build_path_graph`'s degree=2 candidate-token intersection).
        assert!(built.candidate_tokens.contains(&1));
        assert_eq!(built.candidate_tokens.len(), 1);
        assert_eq!(built.graph.node_count(), 0);
    }

    #[tokio::test]
    async fn offline_pipeline_over_the_parity_fixture_is_empty_but_clean() {
        let fixture = FixtureCopy::new();
        let (db, _schema) =
            degenbot::db::DegenbotDb::open(fixture.path()).expect("open fixture db");
        let rows = db.fetch_discovery_rows(8453).expect("fetch discovery rows");
        let built = build_graph(&rows, &[PoolKind::V3, PoolKind::V4], None);
        let params = DiscoveryParams {
            start_tokens: vec![1],
            end_tokens: vec![1],
            min_depth: 2,
            max_depth: Some(3),
            pool_type_per_depth: None,
            batch_size: 1000,
        };
        let mut pipeline = RegistrationPipeline::new(
            PathPolicy {
                min_hops: 2,
                max_hops: 3,
                ..PathPolicy::default()
            },
            RetryPolicy::verification_default(),
        );
        let report = run_offline(
            &mut pipeline,
            &built,
            &params,
            "0x236aa50979d5f3de3bd1eeb40e81137f22ab794b",
            "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
        )
        .await;
        // A single pool pair with no closing cycle: enumeration is empty and
        // no stage panics.
        assert_eq!(report.candidates, 0);
        assert_eq!(report.path_count, 0);
        assert_eq!(report.direction_errors, 0);
    }
}
