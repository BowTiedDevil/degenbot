//! On-demand backrun pair resolution + staged path planning (task B6H2KO).
//!
//! The backrun pipeline receives swap legs from the frame replay, but the
//! boot-time path graph only
//! covers pools the crawler discovered before launch. This module owns the
//! **incremental** side of registration:
//!
//! 1. warm-index check — pairs already in the graph stage nothing,
//! 2. pair discovery via the `PairDiscovery` port (production adapter:
//!    CREATE2-derived V2 pair addresses + existence probe; provider wiring
//!    lands with the driver task NYVL2F),
//! 3. `FoT` gating — suspected fee-on-transfer tokens never stage edges
//!    (same semantics as the dispatcher's `FoT` classifier),
//! 4. per-event budget — at most `max_new_edges_per_event` new pairs stage
//!    per event; overflow is counted, never silently dropped,
//! 5. STAGED application — the planner never mutates the shared graph
//!    (single-writer discipline with the bulk crawler, per the crawl's
//!    single-writer invariants); the dispatcher merges `StagedEdge`s into its
//!    authoritative edge list under its write lock via `apply_staged` and
//!    rebuilds the graph from it.
//!
//! Pool *state* assembly (ticks/reserves) and admission verdicts continue
//! through `pool_builder` + the registration lifecycle + the registration
//! gate; this module only stages graph edges and reports discovery debts.

use std::collections::HashSet;
use std::future::Future;
use std::sync::Mutex;

use alloy::primitives::Address;

pub use degenbot_pathfinding::PoolKind;
use hashbrown::HashMap;
use std::sync::Mutex as StdMutex;

/// A pending pool lookup the planner owes discovery.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PoolQuery {
    /// Uniswap-v2-family pair for the (unordered) token pair. Production
    /// adapter: CREATE2 pair address from factory + init code hash, then one
    /// `get_code` existence probe.
    V2Pair(u64, u64),
    /// V3 pool for the (unordered) pair at a fee tier. The adapter probes
    /// canonical fee tiers in caller-chosen order.
    V3Pool(u64, u64, u32),
    /// V4 pool: token pair evidence + `poolId` on a manager address (the
    /// upstream restore path feeds these directly).
    V4Pool(u64, u64, [u8; 32], Address),
}

/// A discovered pool ready for graph staging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedPool {
    pub pool_id: u64,
    pub pool_kind: PoolKind,
}

/// Async discovery port. Mock in tests; the production adapter lands in the
/// driver-wiring task with the bot's provider (ADR-022 D3 single provider).
pub trait PairDiscovery {
    fn resolve(&self, query: &PoolQuery) -> impl Future<Output = Option<ResolvedPool>> + Send;
}

/// One new graph edge staged for the single-writer apply step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StagedEdge {
    pub token_a: u64,
    pub token_b: u64,
    pub pool_id: u64,
    pub pool_kind: PoolKind,
}

/// Per-stage churn counters (telemetry-facing; small closed sets only).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlannerChurn {
    pub new_pairs: u32,
    pub rejected_fot: u32,
    pub rejected_budget: u32,
    pub discovery_misses: u32,
}

/// The planner's output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StagedPlan {
    pub new_edges: Vec<StagedEdge>,
    /// Queries discovery could not resolve this pass; the dispatcher may
    /// re-owed them on later events (retry policy is the dispatcher's).
    pub owed_queries: Vec<PoolQuery>,
    pub churn: PlannerChurn,
}

pub struct PlannerConfig {
    /// Hard cap on NEW graph edges staged per incoming event.
    pub max_new_edges_per_event: u32,
    /// `FoT` gate: token id => suspected fee-on-transfer (never staged).
    pub token_is_fot: Box<dyn Fn(u64) -> bool + Send + Sync>,
}

impl std::fmt::Debug for PlannerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlannerConfig")
            .field("max_new_edges_per_event", &self.max_new_edges_per_event)
            .finish_non_exhaustive()
    }
}

/// Warm-set driven staged planner. Confirmation inserts into the warm set so
/// later events skip re-discovery.
pub struct OnDemandPathPlanner {
    warm: StdMutex<HashSet<(u64, u64)>>,
    cfg: PlannerConfig,
}

impl OnDemandPathPlanner {
    #[must_use]
    pub fn new(warm: HashSet<(u64, u64)>, cfg: PlannerConfig) -> Self {
        Self {
            warm: StdMutex::new(warm),
            cfg,
        }
    }

    /// Poisson-proof lock (poisoned at panic keeps the consensus set; the
    /// registry discipline treats any panic in this module as orchestrator
    /// fatal elsewhere).
    fn warm(&self) -> std::sync::MutexGuard<'_, HashSet<(u64, u64)>> {
        self.warm
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Stage edges for the event's (unordered) token pairs.
    pub async fn stage_for_legs<D: PairDiscovery>(
        &self,
        legs: &[(u64, u64)],
        discovery: &D,
    ) -> StagedPlan {
        let mut plan = StagedPlan::default();
        let mut seen_this_event: HashSet<(u64, u64)> = HashSet::new();
        for &(t0, t1) in legs {
            let key = normalized(t0, t1);
            if !seen_this_event.insert(key) {
                continue; // mirrored/duplicate leg within the event
            }
            if self.warm().contains(&key) {
                continue; // boot-graph pair: no discovery, no staging
            }
            let query = PoolQuery::V2Pair(key.0, key.1);
            if let Some(resolved) = discovery.resolve(&query).await {
                if (self.cfg.token_is_fot)(key.0) || (self.cfg.token_is_fot)(key.1) {
                    plan.churn.rejected_fot += 1;
                    continue;
                }
                if plan.new_edges.len() >= self.cfg.max_new_edges_per_event as usize {
                    plan.churn.rejected_budget += 1;
                    continue;
                }
                plan.new_edges.push(StagedEdge {
                    token_a: key.0,
                    token_b: key.1,
                    pool_id: resolved.pool_id,
                    pool_kind: resolved.pool_kind,
                });
                plan.churn.new_pairs += 1;
                self.warm().insert(key);
            } else {
                plan.owed_queries.push(query);
                plan.churn.discovery_misses += 1;
            }
        }
        plan
    }
}

fn normalized(t0: u64, t1: u64) -> (u64, u64) {
    if t0 <= t1 {
        (t0, t1)
    } else {
        (t1, t0)
    }
}

/// Merge staged edges into the dispatcher's authoritative (undirected) edge
/// list under its write lock. Dedupe key: (normalized pair, `pool_id`, `kind`) —
/// parallel pools on the same pair (fee tiers) are distinct edges; exact
/// re-staging is a no-op. Returns the number of edges actually added.
pub fn apply_staged(
    authoritative: &mut Vec<(u64, u64, u64, PoolKind)>,
    staged: &[StagedEdge],
) -> u32 {
    let mut added = 0u32;
    for e in staged {
        let key = normalized(e.token_a, e.token_b);
        let candidate = (key.0, key.1, e.pool_id, e.pool_kind);
        if authoritative.contains(&candidate) {
            continue;
        }
        authoritative.push(candidate);
        added += 1;
    }
    added
}

/// Owns the fast token-id space mapping helper crates do not rely on: callers
/// translate Addresses to ids before staging. Retained on the module contract
/// (documented seam) rather than a parallel map.
#[derive(Debug, Default)]
pub struct OnDemandAddressMap {
    map: Mutex<HashMap<Address, u64, hashbrown::DefaultHashBuilder>>,
}

impl OnDemandAddressMap {
    #[must_use]
    pub fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
        }
    }

    /// Intern a token address to a stable id (100 + insertion index, reversed
    /// so ids are stable across map growth in tests).
    pub fn intern(&self, a: Address) -> u64 {
        // Poisoned guard still owns the interned map; interning is monotone.
        let mut map = self
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(&id) = map.get(&a) {
            return id;
        }
        let id = 100 + map.len() as u64;
        map.insert(a, id);
        id
    }
}
