//! The example's registration pipeline — an ADAPTER over Registration
//! admission, plus its offline enumeration arm.
//!
//! The stage order a candidate takes to the engine's registry (memoization →
//! construction → eligibility → Verify lifecycle → registration) is the core's
//! ONE implementation
//! ([`RegistrationAdmission`](degenbot::bot::crawl::RegistrationAdmission)); this
//! module contributes the two things an adapter owns — where candidates come
//! from (the batched graph enumeration) and this driver's observation policy
//! (the pre-registration [`PathPolicy`] gate and the progress cadence). It
//! declares no admission rule of its own: the typed outcome vocabulary, the
//! `PipelineReport` fold, the negative memos, the build flights, the verify
//! claim table and its bounded retry are all the core's.
//!
//! Two arms:
//! - **offline-dry** ([`run_offline`]): enumeration + direction resolution +
//!   the graph-side policy + candidate counts only — no RPC, no build, no
//!   registration, so nothing is admitted at all. This is the CI acceptance
//!   path.
//! - **live** (`crate::live::run_live`, gated on `SMOKE_RPC_URL`): the same
//!   enumeration, then each candidate through the admission. The receipt the
//!   admission answers is folded here through `PipelineReport::absorb` — the
//!   same fold the crawl adapter's receipts fold through.

#![expect(
    clippy::result_large_err,
    reason = "the bounded admission outcome IS the vocabulary the fold consumes; boxing it would only move the allocation to every call site"
)]

use degenbot::pathfinding::{
    resolve_directions as core_resolve_directions, DirectionHop, DirectionToken, PoolKind,
};

use crate::discovery::{BatchedPathFinder, BuiltGraph, DiscoveryParams};
use crate::policy::{HopView, PathPolicy};
use degenbot::bot::bot_core::{
    OutcomeLabel, PipelineReport, RegistrationOutcome, RegistrationUnitOutcome,
};
use degenbot::bot::crawl::DiscoveredHop;

/// One enumerated path resolved to its graph nodes and oriented hop views.
pub struct ResolvedPath {
    /// Indices into [`BuiltGraph::nodes`], in path order.
    pub pool_indices: Vec<usize>,
    /// The oriented hop views (the graph-side policy's input).
    pub hops: Vec<HopView>,
    /// The per-hop directions, parallel to `pool_indices`.
    pub zfos: Vec<bool>,
}

/// Map one enumerated path onto graph nodes, refusing a pool the graph does not
/// hold (or holds under another family) with the admission's own bounded label.
///
/// # Errors
///
/// The bounded `UnknownPoolType` skip outcome.
pub fn node_indices(
    built: &BuiltGraph,
    path: &[(u64, PoolKind)],
) -> Result<Vec<usize>, RegistrationUnitOutcome> {
    let mut pool_indices: Vec<usize> = Vec::with_capacity(path.len());
    for (graph_id, kind) in path {
        let Some(idx) = built.by_graph_id.get(graph_id).copied() else {
            return Err(unknown_pool());
        };
        if built.nodes[idx].kind != *kind {
            return Err(unknown_pool());
        }
        pool_indices.push(idx);
    }
    Ok(pool_indices)
}

fn unknown_pool() -> RegistrationUnitOutcome {
    RegistrationUnitOutcome::Skip {
        label: OutcomeLabel::Vocabulary(RegistrationOutcome::UnknownPoolType),
        counts_as_skip: true,
        detail: None,
    }
}

/// The V4 protocol's PRIVATE native-currency spelling (ADR-097 D3): the
/// graph rows carry the wire view; it is translated to the conceptual native
/// token at this protocol call site and never crosses the resolver raw.
const V4_NATIVE_SENTINEL: &str = "0x0000000000000000000000000000000000000000";

/// The direction resolver's conceptual spelling of one node token: the V4
/// zero currency under the V4 family is the conceptual native token; every
/// other spelling is its own ERC-20.
fn node_direction_token(kind: PoolKind, spelling: &str) -> DirectionToken<'_> {
    if matches!(kind, PoolKind::V4) && spelling == V4_NATIVE_SENTINEL {
        DirectionToken::Native
    } else {
        DirectionToken::Erc20(spelling)
    }
}

/// Resolve the per-hop directions from the graph's own token facts and orient
/// the hop views — the enumeration's direction step.
///
/// # Errors
///
/// The direction resolver's rendering when the chain cannot close.
pub fn resolve_path(
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
                token0: node_direction_token(node.kind, &node.token0),
                token1: node_direction_token(node.kind, &node.token1),
                identity: &node.identity,
            }
        })
        .collect();
    let input = if input_token_lower == V4_NATIVE_SENTINEL {
        DirectionToken::Native
    } else {
        DirectionToken::Erc20(input_token_lower)
    };
    let zfos = core_resolve_directions(&direction_hops, &input, weth_lower)
        .map_err(|error| error.to_string())?
        .zero_for_one;

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

/// The admission's candidate hops for one resolved path — the family identity
/// the admission builds from (the graph's addresses and V4 pool ids).
///
/// # Errors
///
/// The bounded `UnknownPoolType` skip outcome when a node carries no identity
/// for its family.
pub fn admitted_hops(
    built: &BuiltGraph,
    pool_indices: &[usize],
) -> Result<Vec<DiscoveredHop>, RegistrationUnitOutcome> {
    let mut hops = Vec::with_capacity(pool_indices.len());
    for idx in pool_indices {
        let node = &built.nodes[*idx];
        let hop = match node.kind {
            PoolKind::V2 => DiscoveredHop::V2 {
                address: parse_address(node.address.as_deref())?,
            },
            PoolKind::V3 => DiscoveredHop::V3 {
                address: parse_address(node.address.as_deref())?,
            },
            PoolKind::V4 => DiscoveredHop::V4 {
                pool_manager: parse_address(node.manager.as_deref())?,
                pool_hash: node
                    .pool_hash
                    .as_deref()
                    .and_then(|hash| hash.parse::<alloy::primitives::B256>().ok())
                    .map(|hash| hash.0),
            },
            _ => return Err(unknown_pool()),
        };
        hops.push(hop);
    }
    Ok(hops)
}

fn parse_address(
    text: Option<&str>,
) -> Result<alloy::primitives::Address, RegistrationUnitOutcome> {
    text.and_then(|text| text.parse().ok())
        .ok_or_else(unknown_pool)
}

/// The offline-dry arm: walk the batched enumeration, resolve directions, apply
/// the graph-side policy, and count candidates. No RPC, no build, no
/// registration — nothing is admitted.
///
/// This is the CI acceptance path (parity: outcomes comparable to the Python
/// driver's build/skip counters with the build/verify/register turns removed).
pub async fn run_offline(
    built: &BuiltGraph,
    params: &DiscoveryParams,
    policy: &PathPolicy,
    input_token_lower: &str,
    weth_lower: &str,
) -> PipelineReport {
    let mut report = PipelineReport::default();
    let mut finder = BatchedPathFinder::new(&built.graph, params);
    while let Some(batch) = finder.next_batch() {
        for path in &batch {
            match node_indices(built, path) {
                Ok(pool_indices) => {
                    match resolve_path(built, &pool_indices, input_token_lower, weth_lower) {
                        Ok((_zfos, hops)) => match policy.evaluate(&hops) {
                            Ok(()) => report.candidates += 1,
                            Err(rejection) => {
                                report.policy_rejected += 1;
                                report.absorb(&RegistrationUnitOutcome::Reject {
                                    detail: Some(rejection.to_string()),
                                });
                            }
                        },
                        Err(message) => {
                            report.direction_errors += 1;
                            *report
                                .skip_reasons
                                .entry(format!("direction-fatal:{message}"))
                                .or_insert(0) += 1;
                        }
                    }
                }
                Err(outcome) => report.absorb(&outcome),
            }
        }
        tokio::task::yield_now().await;
    }
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
    use alloy::primitives::{address, Address};
    use degenbot::db::discovery_read::DiscoveryPoolRow;
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
    /// malformed") — the intermittent `open fixture db` failure. Each test heals
    /// its own copy instead, so the committed fixture bytes stay historical and
    /// no two opens share a file.
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
        let policy = PathPolicy {
            min_hops: 2,
            max_hops: 3,
            ..PathPolicy::default()
        };
        let report = run_offline(
            &built,
            &params,
            &policy,
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

    // ── the adapter's candidate mapping, pinned against the admission ──────

    const PIN_USDC: Address = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
    const PIN_WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");

    /// One V2 discovery row over `(USDC, WETH)`.
    fn pin_v2_row(id: i64, pool: Address) -> DiscoveryPoolRow {
        use degenbot::db::discovery_read::DiscoveryV2Row;
        use degenbot::db::rows::{Erc20TokenRow, ExchangeRow, LiquidityPoolRow};
        let token = |id: i64, address: Address| Erc20TokenRow {
            id,
            chain: 1,
            address,
            name: None,
            symbol: None,
            decimals: Some(18),
        };
        DiscoveryPoolRow::V2(DiscoveryV2Row {
            pool: LiquidityPoolRow {
                id,
                address: pool,
                chain: 1,
                kind: "uniswap_v2".to_owned(),
                token0_id: 1,
                token1_id: 2,
                exchange_id: 1,
            },
            exchange: ExchangeRow {
                id: 1,
                chain_id: 1,
                name: "uniswap_v2".to_owned(),
                active: true,
                last_update_block: None,
                factory: Address::ZERO,
                deployer: None,
            },
            token0: token(1, PIN_USDC),
            token1: token(2, PIN_WETH),
            fee_token0: 997,
            fee_token1: 997,
            fee_denominator: 1_000,
            stable: None,
        })
    }

    /// A two-pool graph over one token pair: both tokens sit on two edges, so the
    /// degree filter admits them and the graph carries a two-hop cycle.
    fn pin_graph() -> BuiltGraph {
        let rows = vec![
            pin_v2_row(1, Address::from([0x51u8; 20])),
            pin_v2_row(2, Address::from([0x52u8; 20])),
        ];
        build_graph(&rows, &[PoolKind::V2], None)
    }

    /// The candidate path over the fixture graph, in graph order.
    fn pin_path(built: &BuiltGraph) -> Vec<(u64, PoolKind)> {
        built
            .nodes
            .iter()
            .map(|node| (node.graph_id, node.kind))
            .collect()
    }

    /// The adapter's mapping answers the SAME bounded vocabulary the admission
    /// folds: a graph path maps to family identities, an unknown or family-
    /// mismatched pool is the `unknown-pool-type` skip, and the direction
    /// refusal stays the loud arm.
    #[test]
    fn the_adapter_maps_a_path_to_the_admissions_candidate_families() {
        let built = pin_graph();
        let path = pin_path(&built);
        assert_eq!(path.len(), 2, "the fixture graph has two V2 hops");
        let usdc = "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48";
        let weth = "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2";

        let indices = node_indices(&built, &path).expect("the path maps");
        assert_eq!(indices.len(), 2);
        let (zfos, views) = resolve_path(&built, &indices, usdc, weth).expect("the cycle closes");
        assert_eq!(zfos, vec![true, false], "USDC → WETH → USDC");
        assert_eq!(views.len(), 2);

        let hops = admitted_hops(&built, &indices).expect("both hops carry an identity");
        assert_eq!(hops.len(), 2);
        for hop in &hops {
            assert_eq!(hop.kind(), PoolKind::V2);
            assert!(hop.pre_build_identity().is_some());
        }

        // An unknown graph id and a family mismatch are the bounded skip.
        assert!(matches!(
            node_indices(&built, &[(9_999, PoolKind::V2)]),
            Err(RegistrationUnitOutcome::Skip {
                label: OutcomeLabel::Vocabulary(RegistrationOutcome::UnknownPoolType),
                counts_as_skip: true,
                ..
            })
        ));
        assert!(matches!(
            node_indices(&built, &[(path[0].0, PoolKind::V3)]),
            Err(RegistrationUnitOutcome::Skip {
                label: OutcomeLabel::Vocabulary(RegistrationOutcome::UnknownPoolType),
                counts_as_skip: true,
                ..
            })
        ));
    }

    /// A V4 hop carries its `(PoolManager, pool_id)` identity; a hashless one is
    /// still representable, and the ADMISSION answers its bounded `v4-no-hash`
    /// skip (the adapter never invents one).
    #[test]
    fn a_v4_hop_carries_its_manager_and_pool_id() {
        let mut built = pin_graph();
        built.nodes[0].kind = PoolKind::V4;
        built.nodes[0].manager = Some("0x3333333333333333333333333333333333333333".to_owned());
        built.nodes[0].pool_hash = Some(format!("0x{}", "cd".repeat(32)));
        let hops = admitted_hops(&built, &[0]).expect("the V4 identity maps");
        assert_eq!(hops.len(), 1);
        assert_eq!(hops[0].kind(), PoolKind::V4);

        built.nodes[0].pool_hash = None;
        let hashless = admitted_hops(&built, &[0]).expect("a hashless V4 hop is representable");
        assert!(
            hashless[0].pre_build_identity().is_none(),
            "a hashless hop carries no memoizable identity"
        );
    }
}
