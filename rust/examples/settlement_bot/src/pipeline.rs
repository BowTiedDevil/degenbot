//! The discovery→registration pipeline — parity-ledger rows 9 + 12 + 13
//!
//! Mirrors `src/degenbot/runner/build_paths.py`'s
//! `PathRegistrationPipeline._registration_unit` (the per-path build/verify/
//! register unit) hand-wired: the D7KMQO policy gate, the W73FVY dup
//! fast-path, and the prepare→arm→fold sequence. The CONTRACT it coordinates
//! is the core's (`degenbot::bot::bot_core::registration_ledger`): the typed
//! [`RegistrationUnitOutcome`](degenbot::bot::bot_core::registration_ledger::RegistrationUnitOutcome)
//! vocabulary, the
//! [`PipelineReport`](degenbot::bot::bot_core::registration_ledger::PipelineReport)
//! counter fold, and the [`RegistrationLedger`](degenbot::bot::bot_core::registration_ledger::RegistrationLedger)
//! memos — this module declares none of them. The at-most-once verify window
//! is the CORE's too (`degenbot::bot::bot_core::VerifyClaims`, entered by
//! `EngineDriver::run_*_registration_lifecycle`); this layer keeps only the
//! verify-once memo and the bounded retry dance over a released window.
//!
//! Two modes:
//! - **offline-dry** ([`run_offline`]): enumeration + direction resolution +
//!   policy + candidate counts only — no RPC, no build, no registration.
//!   This is the CI acceptance path.
//! - **live** (gated on `SMOKE_RPC_URL`; see `main.rs`): per-candidate pool
//!   build through `ConstructionIo`, `BotState` registration, the ADR-022
//!   verify lifecycle under the claim table, then
//!   `EngineDriver::register_and_solve_path`.

use degenbot::pathfinding::{
    resolve_directions as core_resolve_directions, DirectionHop, PoolKind,
};

use crate::discovery::{BuiltGraph, DiscoveryParams};
use crate::policy::{HopView, PathPolicy};
use degenbot::bot::bot_core::registration_ledger::{
    HopSignature, OutcomeLabel, PipelineReport, RegistrationLedger, RegistrationOutcome,
    RegistrationUnitOutcome,
};
use degenbot::bot_core::verification_retry::RetryPolicy;

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
    Skip(RegistrationUnitOutcome),
    /// A counted rejection (path-rejected memo / policy deny).
    Reject(RegistrationUnitOutcome),
    /// A fatal direction-resolution failure (subgraph vs constructed pool
    /// disagreement) — the Python pipeline aborts loudly.
    DirectionFatal(String),
}

/// The reusable registration pipeline (mirrors `PathRegistrationPipeline`).
#[derive(Debug)]
pub struct RegistrationPipeline {
    /// The four memos + typed build-refusal classification.
    pub ledger: RegistrationLedger,
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
            policy,
            retry_policy,
        }
    }

    /// Resolve the per-hop directions and orient the hop views — the driver's
    /// direction-resolution step, over the graph nodes the candidate indexes.
    fn resolve_hops(
        built: &BuiltGraph,
        pool_indices: &[usize],
        input_token_lower: &str,
        weth_lower: &str,
    ) -> Result<(Vec<bool>, Vec<HopView>), String> {
        let direction_hops: Vec<DirectionHop<'_>> = pool_indices
            .iter()
            .map(|idx| {
                let node = &built.nodes[*idx];
                DirectionHop {
                    token0: &node.token0,
                    token1: &node.token1,
                    identity: &node.identity,
                }
            })
            .collect();
        let zfos = core_resolve_directions(&direction_hops, input_token_lower, weth_lower)
            .map_err(|error| error.to_string())?;

        let hops = pool_indices
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
        Ok((zfos, hops))
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
                return PrepareOutcome::Skip(RegistrationUnitOutcome::Skip {
                    label: OutcomeLabel::Vocabulary(RegistrationOutcome::UnknownPoolType),
                    counts_as_skip: true,
                    detail: None,
                });
            };
            if built.nodes[idx].kind != *kind {
                return PrepareOutcome::Skip(RegistrationUnitOutcome::Skip {
                    label: OutcomeLabel::Vocabulary(RegistrationOutcome::UnknownPoolType),
                    counts_as_skip: true,
                    detail: None,
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
                return PrepareOutcome::Skip(RegistrationUnitOutcome::Skip {
                    label: OutcomeLabel::Vocabulary(RegistrationOutcome::V4NoHash),
                    counts_as_skip: true,
                    detail: None,
                });
            }
            let memo_key = RegistrationLedger::pool_memo_key(
                *kind,
                node.address.as_deref(),
                node.pool_hash.as_deref(),
            );
            if let Some(record) = self.ledger.unregistrable_record(memo_key.as_deref()) {
                return PrepareOutcome::Skip(RegistrationUnitOutcome::Skip {
                    label: OutcomeLabel::Vocabulary(record.outcome),
                    counts_as_skip: record.counts_as_skip,
                    detail: None,
                });
            }
        }

        let (zfos, hops) =
            match Self::resolve_hops(built, &pool_indices, input_token_lower, weth_lower) {
                Ok(resolved) => resolved,
                Err(error) => return PrepareOutcome::DirectionFatal(error),
            };

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
            return PrepareOutcome::Reject(RegistrationUnitOutcome::Reject { detail: None });
        }

        // Policy gate: hop bounds, allow/deny, duplicate pools.
        if let Err(rejection) = self.policy.evaluate(&hops) {
            self.ledger.memoize_rejected_path(hop_sig);
            return PrepareOutcome::Reject(RegistrationUnitOutcome::Reject {
                detail: Some(rejection.to_string()),
            });
        }

        // Dup fast-path (W73FVY): answer registered signatures before verify.
        // The dedup outcome carries the unit's V4 hops so the fold's
        // `v4_pool_count` witness matches the Python driver's memo answer.
        if self.ledger.path_registered(&hop_sig) {
            return PrepareOutcome::Skip(RegistrationUnitOutcome::Registered {
                created: false,
                v4_hops,
            });
        }

        PrepareOutcome::Ready(PreparedCandidate {
            pool_indices,
            hops,
            hop_sig,
            zfos,
            v4_hops,
        })
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
                PrepareOutcome::Skip(outcome) => report.absorb(&outcome),
                PrepareOutcome::Reject(outcome) => {
                    report.absorb(&outcome);
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
    // Discovery admits every token as an intermediate hop (mirroring
    // `find_paths_async` with no `allowed_intermediate_tokens`), so the policy
    // gate never sees a token-filter rejection by default; `token_filter_count`
    // stays the Python counter it is (0 unless a `PathPolicy` allow/deny set is
    // configured).
    report
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
    use degenbot::pathfinding::PoolKind;
    use std::path::Path;

    /// The G2 discovery fixture (chain 8453: one V3 pool + one V4 pool).
    fn fixture_path() -> String {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../crates/foundation/degenbot-db/tests/fixtures/parity.db"
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
