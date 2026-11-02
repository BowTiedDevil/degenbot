//! Native discovery: the DB graph reads, the traversal plan, and the DFS sweep
//! that hands the crawl its candidates.
//!
//! The Python driver's `pathfinding/_pathfinding.py` is the composition this
//! module replaces, and it stays two things here:
//!
//! 1. **the plan** — [`DiscoverySweep::prepare`]: resolve the boundary token
//!    ids, build the candidate-token graph (the degree-2 rule plus the
//!    candidate ∩ edge filter `_prepare_graph` applies inline), and assemble
//!    the traversal plan (`prepare_traversal_plan`);
//! 2. **the sweep** — [`DiscoverySweep::next_path`]: a lazy DFS walk over that
//!    plan that yields ONE candidate at a time.
//!
//! The sweep is a unit SOURCE; the controller owns the loop, the window and the
//! fold. Discovery is a pure function of the database's pool-row structure
//! (never of prices or pool state), which is why the crawl may key its
//! sweep-completion latch on [`Edition`].
//!
//! # What moved off the driver
//!
//! - `_prepare_graph` — candidate-token fetch, the edge filter, the typed graph;
//! - `_resolve_boundary_token_ids` — address → token-id resolution, with the
//!   missing-token refusal that is a named error here instead of a
//!   `DegenbotValueError`;
//! - `_prepare_traversals` — the Cartesian plan and the per-depth filter;
//! - the `find_paths_async` batch loop — one [`DiscoverySweep`] step per
//!   candidate, with the per-path `PathStep` construction replaced by the
//!   typed [`DiscoveredHop`] the unit builds from.
//!
//! # What did not move
//!
//! No pool/state work. The graph's address and hash lookups are the edge's own
//! identity, so a discovered candidate is decidable before any RPC: the family
//! is the edge's [`PoolKind`] (the R1 decision, pinned by the unit's fixture),
//! and the address/manager/hash are the lookups the same read produced.
//!
//! # Cancellation
//!
//! The Python sweep's cooperative cancel flag exists because one
//! `PathBatchIterator` poll grinds a whole batch of paths on a tokio worker, so
//! an abandoned sweep has to be told to stop. Here a pull is ONE DFS step, so
//! dropping the sweep (or the finder it holds) is the cancel edge: there is no
//! mid-grind to interrupt, and no zombie thread to release.

use std::path::Path;

use alloy::primitives::Address;
use degenbot_core::hex_utils::decode_hex;
use degenbot_db::{DbError, DegenbotDb, PathGraphData};
use degenbot_pathfinding::{
    prepare_traversal_plan, OwnedPathFinder, PathGraph, PoolKind, SearchSpec, TraversalSpec,
};
use hashbrown::{HashMap, HashSet};

use crate::arb_engine::crawl::unit::DiscoveredHop;

/// The structural fingerprint of the discovery subgraph — a
/// [`degenbot_db::GraphEdition`].
///
/// Re-exported under the crawl's own name because it is the crawl's latch
/// payload: two probes that agree mean the candidate set cannot have changed
/// (the row count and max id of each pool family are the whole input), so a
/// completed sweep needs no re-run.
pub use degenbot_db::GraphEdition as Edition;

/// The pool-family degree a token needs before it can anchor a cycle.
///
/// Mirrors the Python callers' `_get_tokens_with_min_degree(degree=2)`: a
/// two-hop cycle touches a token through two pools, so a token that appears in
/// fewer than two pools can never close one.
const CANDIDATE_TOKEN_DEGREE: u32 = 2;

// ── the discovery port ──────────────────────────────────────────────────────

/// The database reads one discovery pass performs.
///
/// A port rather than four free functions over a [`DegenbotDb`]: the sweep is
/// then drivable from a scripted source (a pure-Rust consumer with its own
/// row store, or a test pinning the sweep's own behavior without a database
/// file), and the production source — [`DbDiscoverySource`] — is four thin
/// delegations.
pub trait DiscoverySource: Send + Sync {
    /// The structural fingerprint the sweep-completion latch keys on.
    ///
    /// # Errors
    ///
    /// [`CrawlDiscoveryError::Db`] when the probe cannot read the database.
    fn edition(&self) -> Result<Edition, CrawlDiscoveryError>;

    /// Resolve boundary token addresses to token ids.
    ///
    /// Absent addresses are simply missing from the returned map; the sweep
    /// names the missing one ([`CrawlDiscoveryError::UnknownBoundaryToken`]).
    ///
    /// # Errors
    ///
    /// [`CrawlDiscoveryError::Db`] when the read fails.
    fn resolve_token_ids(
        &self,
        addresses: &[Address],
    ) -> Result<HashMap<Address, u64>, CrawlDiscoveryError>;

    /// The tokens appearing in at least [`CANDIDATE_TOKEN_DEGREE`] pools of the
    /// requested families.
    ///
    /// # Errors
    ///
    /// [`CrawlDiscoveryError::Db`] when the read fails.
    fn candidate_tokens(&self, kinds: &[PoolKind]) -> Result<HashSet<u64>, CrawlDiscoveryError>;

    /// The flat edge list plus the address/hash lookups for the requested
    /// families.
    ///
    /// # Errors
    ///
    /// [`CrawlDiscoveryError::Db`] when the read fails, or when a pool row's
    /// family is outside the graph vocabulary.
    fn graph_data(&self, kinds: &[PoolKind]) -> Result<PathGraphData, CrawlDiscoveryError>;
}

/// The production discovery source: the file-backed [`DegenbotDb`] reads the
/// Python driver called through the FFI, called in-process.
pub struct DbDiscoverySource {
    db: DegenbotDb,
    chain_id: i64,
}

impl DbDiscoverySource {
    /// Open the discovery handle for `path` on `chain_id`.
    ///
    /// # Errors
    ///
    /// [`CrawlDiscoveryError::Db`] when the database cannot be opened (the
    /// caller's configure step fails loud rather than discovering into an empty
    /// graph).
    pub fn open(path: &Path, chain_id: i64) -> Result<Self, CrawlDiscoveryError> {
        let (db, _schema) = DegenbotDb::open(path)?;
        Ok(Self { db, chain_id })
    }

    /// Wrap an already-open handle (a caller that owns the connection).
    #[must_use]
    pub const fn from_database(db: DegenbotDb, chain_id: i64) -> Self {
        Self { db, chain_id }
    }

    /// The chain this source reads.
    #[must_use]
    pub const fn chain_id(&self) -> i64 {
        self.chain_id
    }
}

impl DiscoverySource for DbDiscoverySource {
    fn edition(&self) -> Result<Edition, CrawlDiscoveryError> {
        Ok(self.db.fetch_graph_edition(self.chain_id)?)
    }

    fn resolve_token_ids(
        &self,
        addresses: &[Address],
    ) -> Result<HashMap<Address, u64>, CrawlDiscoveryError> {
        Ok(self
            .db
            .fetch_token_ids_by_address(self.chain_id, addresses)?)
    }

    fn candidate_tokens(&self, kinds: &[PoolKind]) -> Result<HashSet<u64>, CrawlDiscoveryError> {
        Ok(self
            .db
            .fetch_tokens_with_min_degree(self.chain_id, CANDIDATE_TOKEN_DEGREE, kinds)?)
    }

    fn graph_data(&self, kinds: &[PoolKind]) -> Result<PathGraphData, CrawlDiscoveryError> {
        Ok(self.db.fetch_path_graph_edges(self.chain_id, kinds)?)
    }
}

// ── the errors ──────────────────────────────────────────────────────────────

/// Why a discovery pass could not produce its next candidate.
///
/// Every arm is loud: discovery failing is not a skip, it is the crawl's input
/// being unreadable.
#[derive(Debug, thiserror::Error)]
pub enum CrawlDiscoveryError {
    /// A database read failed.
    #[error("discovery read failed: {0}")]
    Db(#[from] DbError),
    /// A configured boundary token has no row in the chain's token table — the
    /// Python driver's `DegenbotValueError` ("was not found in the database"),
    /// named here so the shell can raise its own exception class.
    #[error("the {boundary} token {address} is absent from the chain {chain_id} token table")]
    UnknownBoundaryToken {
        /// Which boundary set the token came from (`Start` / `End`).
        boundary: &'static str,
        /// The unresolvable address.
        address: Address,
        /// The chain the resolution ran on.
        chain_id: i64,
    },
    /// The graph yielded an edge whose pool id the same read carries no lookup
    /// for. A graph/lookup pair built in one read cannot disagree, so this is a
    /// read that changed underneath the pass (a rewrite between the two
    /// queries) — refused rather than guessed.
    #[error("the discovery graph names pool {pool_id} ({kind:?}) but carries no identity for it")]
    UnknownHop {
        /// The graph-level pool id (`V4` ids carry the offset).
        pool_id: u64,
        /// The family the edge asserted.
        kind: PoolKind,
    },
    /// A graph family this crawl cannot build a hop for.
    #[error("the discovery graph names a pool family this crawl cannot build ({kind:?})")]
    UnsupportedHopKind {
        /// The family the edge asserted.
        kind: PoolKind,
    },
}

// ── the values ──────────────────────────────────────────────────────────────

/// The discovery side of the crawl's configuration — what one sweep reads.
#[derive(Clone, Debug)]
pub struct DiscoveryConfig {
    /// The chain the reads are scoped to.
    pub chain_id: i64,
    /// The pool families to include.
    pub pool_kinds: Vec<PoolKind>,
    /// Optional per-depth family permutation filter.
    pub pool_type_per_depth: Option<Vec<Option<Vec<PoolKind>>>>,
    /// Path-start tokens (WETH and the native sentinel for V4).
    pub start_tokens: Vec<Address>,
    /// Path-end tokens.
    pub end_tokens: Vec<Address>,
    /// The minimum hop count, before any per-depth filter floor.
    pub min_depth: usize,
    /// The maximum hop count (`None` = unbounded).
    pub max_depth: Option<usize>,
}

/// One discovered candidate: the hops the discovery edge carried, in hop order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredPath {
    /// The candidate's hops, ready for the unit's build stage.
    pub hops: Vec<DiscoveredHop>,
}

// ── the sweep ───────────────────────────────────────────────────────────────

/// The per-family identity lookups the edge list's pool ids resolve through.
struct HopLookups {
    /// `V2`/`V3` `pool_id` → pool address.
    v2v3: HashMap<u64, Address>,
    /// `V4` graph id (offset `managed_pools.id`) → `PoolManager` + pool hash.
    v4: HashMap<u64, V4Lookup>,
}

/// A `V4` hop's identity as the edge read carried it.
struct V4Lookup {
    /// The `PoolManager` hosting the pool.
    pool_manager: Address,
    /// The pool id hash, `None` when the row's `0x`-hex column does not decode
    /// to 32 bytes (the unit answers a hashless hop with its own skip).
    pool_hash: Option<[u8; 32]>,
}

impl HopLookups {
    /// Split a graph read's lookups into the two identity shapes.
    fn new(data: PathGraphData) -> Self {
        let v4 = data
            .v4_lookups
            .into_iter()
            .map(|(pool_id, (pool_manager, hash))| {
                (
                    pool_id,
                    V4Lookup {
                        pool_manager,
                        pool_hash: parse_pool_hash(&hash),
                    },
                )
            })
            .collect();
        Self {
            v2v3: data.v2v3_addresses,
            v4,
        }
    }

    /// Resolve one edge to the hop the unit builds from.
    fn hop(&self, edge: (u64, PoolKind)) -> Result<DiscoveredHop, CrawlDiscoveryError> {
        let (pool_id, kind) = edge;
        match kind {
            PoolKind::V2 | PoolKind::V3 => {
                let address = *self
                    .v2v3
                    .get(&pool_id)
                    .ok_or(CrawlDiscoveryError::UnknownHop { pool_id, kind })?;
                Ok(if kind == PoolKind::V2 {
                    DiscoveredHop::V2 { address }
                } else {
                    DiscoveredHop::V3 { address }
                })
            }
            PoolKind::V4 => {
                let lookup = self
                    .v4
                    .get(&pool_id)
                    .ok_or(CrawlDiscoveryError::UnknownHop { pool_id, kind })?;
                Ok(DiscoveredHop::V4 {
                    pool_manager: lookup.pool_manager,
                    pool_hash: lookup.pool_hash,
                })
            }
            _ => Err(CrawlDiscoveryError::UnsupportedHopKind { kind }),
        }
    }
}

/// Decode a `V4` pool-hash column (`0x`-prefixed hex) into the `[u8; 32]` the
/// unit's identity carries.
fn parse_pool_hash(hex: &str) -> Option<[u8; 32]> {
    let bytes = decode_hex(hex).ok()?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

/// One discovery pass over a prepared plan.
///
/// The graph is built once by [`Self::prepare`]; each traversal in the plan gets
/// its own lazy finder (`OwnedPathFinder`), created when the previous one
/// exhausts and dropped when the sweep is. Cloning the graph per traversal is
/// what the Python path already paid — the FFI seam built a fresh `PathGraph`
/// from the same edge list for every traversal (`find_paths_async_rust` is
/// called with `traversal.graph.edges` per plan entry), so this is not a new
/// cost.
pub struct DiscoverySweep {
    lookups: HopLookups,
    graph: PathGraph,
    plan: std::vec::IntoIter<TraversalSpec>,
    finder: Option<OwnedPathFinder>,
    max_depth: Option<usize>,
    filter: Option<Vec<Option<Vec<PoolKind>>>>,
    exhausted: bool,
}

impl DiscoverySweep {
    /// Resolve the boundary ids, build the candidate graph, and assemble the
    /// plan — `_prepare_traversals`, in-process.
    ///
    /// # Errors
    ///
    /// [`CrawlDiscoveryError`] — a failed read, an unresolvable boundary token,
    /// or a pool row outside the graph vocabulary.
    pub fn prepare(
        source: &dyn DiscoverySource,
        config: &DiscoveryConfig,
    ) -> Result<Self, CrawlDiscoveryError> {
        // The candidate-token rule first: the degree filter means the edge
        // filter below is a subset test, exactly `_prepare_graph`'s
        // `_get_tokens_with_min_degree` + `candidate_tokens` intersection.
        let candidates = source.candidate_tokens(&config.pool_kinds)?;
        let data = source.graph_data(&config.pool_kinds)?;
        let mut edges = Vec::with_capacity(data.edges.len());
        for edge in &data.edges {
            if candidates.contains(&edge.0) && candidates.contains(&edge.1) {
                edges.push(*edge);
            }
        }
        let lookups = HopLookups::new(data);

        let starts = resolve_boundary(source, &config.start_tokens, "Start", config.chain_id)?;
        let ends = resolve_boundary(source, &config.end_tokens, "End", config.chain_id)?;
        let filter_len = config.pool_type_per_depth.as_ref().map(Vec::len);
        let plan = prepare_traversal_plan(&starts, &ends, config.min_depth, filter_len);

        Ok(Self {
            lookups,
            graph: PathGraph::from_edges(edges),
            plan: plan.into_iter(),
            finder: None,
            max_depth: config.max_depth,
            filter: config.pool_type_per_depth.clone(),
            exhausted: false,
        })
    }

    /// How many traversals the plan holds (the search breadth of this sweep).
    #[must_use]
    pub fn traversal_count(&self) -> usize {
        self.plan.len() + usize::from(self.finder.is_some())
    }

    /// Whether the sweep has no candidate left.
    ///
    /// A sweep that was abandoned early reports `false` — "exhausted" is the
    /// natural-completion fact, and only natural completion may latch.
    #[must_use]
    pub const fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    /// The next candidate, or `None` once every traversal is exhausted.
    ///
    /// # Errors
    ///
    /// [`CrawlDiscoveryError::UnknownHop`] / [`CrawlDiscoveryError::UnsupportedHopKind`]
    /// when a yielded edge cannot be resolved to a hop identity.
    pub fn next_path(&mut self) -> Result<Option<DiscoveredPath>, CrawlDiscoveryError> {
        if self.exhausted {
            return Ok(None);
        }
        loop {
            let next = self.finder.as_mut().and_then(OwnedPathFinder::next_path);
            if let Some(path) = next {
                return Ok(Some(DiscoveredPath {
                    hops: path
                        .iter()
                        .map(|edge| self.lookups.hop(*edge))
                        .collect::<Result<Vec<_>, _>>()?,
                }));
            }
            if self.finder.take().is_some() {
                continue;
            }
            let Some(spec) = self.plan.next() else {
                self.exhausted = true;
                return Ok(None);
            };
            self.finder = Some(OwnedPathFinder::new(
                self.graph.clone(),
                SearchSpec::new(
                    spec.start_token_id,
                    spec.end_token_id,
                    spec.min_depth,
                    self.max_depth,
                    spec.include_reverse,
                    self.filter.clone(),
                ),
            ));
        }
    }
}

/// Resolve one boundary token set, preserving the configured order and naming
/// the first token the database does not know.
fn resolve_boundary(
    source: &dyn DiscoverySource,
    tokens: &[Address],
    boundary: &'static str,
    chain_id: i64,
) -> Result<Vec<u64>, CrawlDiscoveryError> {
    // First-seen order, deduplicated: the plan's own dedup preserves order, so
    // the configured order is the sweep order.
    let mut unique: Vec<Address> = Vec::with_capacity(tokens.len());
    for token in tokens {
        if !unique.contains(token) {
            unique.push(*token);
        }
    }
    let resolved = source.resolve_token_ids(&unique)?;
    let mut ids: Vec<u64> = Vec::with_capacity(unique.len());
    for token in unique {
        match resolved.get(&token) {
            Some(id) => ids.push(*id),
            None => {
                return Err(CrawlDiscoveryError::UnknownBoundaryToken {
                    boundary,
                    address: token,
                    chain_id,
                })
            }
        }
    }
    Ok(ids)
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    // ── the fixture source ──────────────────────────────────────────────────

    /// A scripted [`DiscoverySource`]: one graph, one edition, one candidate set.
    struct Fixture {
        data: PathGraphData,
        candidates: HashSet<u64>,
        tokens: HashMap<Address, u64>,
    }

    impl DiscoverySource for Fixture {
        fn edition(&self) -> Result<Edition, CrawlDiscoveryError> {
            Ok(Edition::default())
        }

        fn resolve_token_ids(
            &self,
            addresses: &[Address],
        ) -> Result<HashMap<Address, u64>, CrawlDiscoveryError> {
            Ok(addresses
                .iter()
                .filter_map(|address| self.tokens.get(address).map(|id| (*address, *id)))
                .collect())
        }

        fn candidate_tokens(
            &self,
            _kinds: &[PoolKind],
        ) -> Result<HashSet<u64>, CrawlDiscoveryError> {
            Ok(self.candidates.clone())
        }

        fn graph_data(&self, _kinds: &[PoolKind]) -> Result<PathGraphData, CrawlDiscoveryError> {
            Ok(self.data.clone())
        }
    }

    /// The graph-id offset a `V4` pool id carries (the database read's own).
    const V4_OFFSET: u64 = 1 << 32;

    const TOKEN_A: u64 = 1;
    const TOKEN_B: u64 = 2;
    const TOKEN_C: u64 = 3;

    fn addr(byte: u8) -> Address {
        Address::from([byte; 20])
    }

    fn config(start: Vec<Address>, end: Vec<Address>) -> DiscoveryConfig {
        DiscoveryConfig {
            chain_id: 1,
            pool_kinds: vec![PoolKind::V4],
            pool_type_per_depth: None,
            start_tokens: start,
            end_tokens: end,
            min_depth: 2,
            max_depth: Some(2),
        }
    }

    fn tokens() -> HashMap<Address, u64> {
        HashMap::from_iter([
            (addr(0xA1), TOKEN_A),
            (addr(0xA2), TOKEN_B),
            (addr(0xA3), TOKEN_C),
        ])
    }

    /// Two parallel V4 pools between A and B: `m·(m-1) = 2` two-hop cycles.
    fn two_pool_graph() -> PathGraphData {
        let mut data = PathGraphData::default();
        for slot in 1..=2u64 {
            let graph_id = V4_OFFSET + slot;
            data.edges.push((TOKEN_A, TOKEN_B, graph_id, PoolKind::V4));
            data.v4_lookups
                .insert(graph_id, (addr(0xBB), "0xzz".to_owned()));
        }
        data
    }

    fn sweep_of(
        data: PathGraphData,
        candidates: [u64; 2],
        config: &DiscoveryConfig,
    ) -> DiscoverySweep {
        let source = Fixture {
            data,
            candidates: HashSet::from_iter(candidates),
            tokens: tokens(),
        };
        DiscoverySweep::prepare(&source, config).unwrap()
    }

    // ── the plan ────────────────────────────────────────────────────────────

    #[test]
    fn the_candidate_rule_drops_pools_the_degree_rule_rejects() {
        // A 4-pool graph, but only {A, B} are candidates: the C edges must be
        // filtered before the graph is built (`_prepare_graph`'s intersection).
        let mut data = two_pool_graph();
        data.edges
            .push((TOKEN_B, TOKEN_C, V4_OFFSET + 3, PoolKind::V4));
        data.v4_lookups
            .insert(V4_OFFSET + 3, (addr(0xBB), "0xzz".to_owned()));
        data.edges
            .push((TOKEN_C, TOKEN_A, V4_OFFSET + 4, PoolKind::V4));
        data.v4_lookups
            .insert(V4_OFFSET + 4, (addr(0xBB), "0xzz".to_owned()));

        let mut sweep = sweep_of(
            data,
            [TOKEN_A, TOKEN_B],
            &config(vec![addr(0xA1)], vec![addr(0xA1)]),
        );
        let mut paths = 0;
        while let Some(path) = sweep.next_path().unwrap() {
            paths += 1;
            assert_eq!(path.hops.len(), 2, "a two-hop cycle");
            for hop in &path.hops {
                let DiscoveredHop::V4 { pool_hash, .. } = hop else {
                    panic!("a V4-only graph yielded another family");
                };
                assert!(
                    pool_hash.is_none(),
                    "the fixture's hash columns do not decode"
                );
            }
        }
        assert_eq!(paths, 2, "the two A-B pools, and no path through C");
        assert!(sweep.is_exhausted());
    }

    #[test]
    fn a_missing_boundary_token_is_named() {
        let source = Fixture {
            data: two_pool_graph(),
            candidates: HashSet::from_iter([TOKEN_A, TOKEN_B]),
            tokens: tokens(),
        };
        let unknown = addr(0xEF);
        let refused = DiscoverySweep::prepare(&source, &config(vec![unknown], vec![unknown]));
        let Err(error) = refused else {
            panic!("an unknown start token must refuse the pass");
        };
        match error {
            CrawlDiscoveryError::UnknownBoundaryToken {
                boundary,
                address,
                chain_id,
            } => {
                assert_eq!(boundary, "Start");
                assert_eq!(address, unknown);
                assert_eq!(chain_id, 1);
            }
            other => panic!("expected the named boundary refusal, got {other:?}"),
        }
    }

    #[test]
    fn the_plan_consolidates_the_reverse_direction() {
        // Both tokens are starts AND ends, so `prepare_traversal_plan` merges one
        // direction into the other's reverse traversal: the same cycle is
        // reached from both ends without a duplicated plan entry.
        let mut sweep = sweep_of(
            two_pool_graph(),
            [TOKEN_A, TOKEN_B],
            &config(vec![addr(0xA1), addr(0xA2)], vec![addr(0xA1), addr(0xA2)]),
        );
        let mut paths = Vec::new();
        while let Some(path) = sweep.next_path().unwrap() {
            paths.push(path.hops);
        }
        assert_eq!(
            paths.len(),
            4,
            "(A,A) and (B,B) each yield two pool assignments, and (A,B) yields the \
             forward walk plus its reverse"
        );
    }

    // ── the hops ────────────────────────────────────────────────────────────

    #[test]
    fn v4_hops_carry_the_manager_and_parse_the_pool_hash() {
        let mut data = PathGraphData::default();
        let valid = format!("0x{}", alloy::hex::encode([0xcdu8; 32]));
        data.edges
            .push((TOKEN_A, TOKEN_B, V4_OFFSET + 1, PoolKind::V4));
        data.edges
            .push((TOKEN_A, TOKEN_B, V4_OFFSET + 2, PoolKind::V4));
        data.v4_lookups.insert(V4_OFFSET + 1, (addr(0xBB), valid));
        data.v4_lookups
            .insert(V4_OFFSET + 2, (addr(0xCC), "0xnothex".to_owned()));

        let mut sweep = sweep_of(
            data,
            [TOKEN_A, TOKEN_B],
            &config(vec![addr(0xA1)], vec![addr(0xA1)]),
        );
        let path = sweep.next_path().unwrap().expect("a cycle");
        assert_eq!(path.hops.len(), 2);
        let hashes: Vec<Option<[u8; 32]>> = path
            .hops
            .iter()
            .map(|hop| match hop {
                DiscoveredHop::V4 {
                    pool_manager,
                    pool_hash,
                } => {
                    assert!(
                        *pool_manager == addr(0xBB) || *pool_manager == addr(0xCC),
                        "the hop's manager is the edge's lookup"
                    );
                    *pool_hash
                }
                _ => panic!("a V4-only graph yielded another family"),
            })
            .collect();
        assert!(hashes.contains(&Some([0xcd; 32])));
        assert!(hashes.contains(&None), "a malformed hash column is no hash");
    }

    #[test]
    fn a_v2_hop_is_the_address_the_edge_carried() {
        let mut data = PathGraphData::default();
        for (pool_id, byte) in [(11u64, 0xE1u8), (12, 0xE2)] {
            data.edges.push((TOKEN_A, TOKEN_B, pool_id, PoolKind::V2));
            data.v2v3_addresses.insert(pool_id, addr(byte));
        }
        let mut config = config(vec![addr(0xA1)], vec![addr(0xA1)]);
        config.pool_kinds = vec![PoolKind::V2];
        let mut sweep = sweep_of(data, [TOKEN_A, TOKEN_B], &config);
        let path = sweep.next_path().unwrap().expect("a cycle");
        for hop in &path.hops {
            let DiscoveredHop::V2 { address } = hop else {
                panic!("a V2-only graph yielded another family");
            };
            assert!(
                *address == addr(0xE1) || *address == addr(0xE2),
                "the hop's address is the edge's own lookup"
            );
        }
    }

    #[test]
    fn an_edge_without_a_lookup_is_refused() {
        let mut data = two_pool_graph();
        data.v2v3_addresses.clear();
        data.v4_lookups.clear();
        let mut sweep = sweep_of(
            data,
            [TOKEN_A, TOKEN_B],
            &config(vec![addr(0xA1)], vec![addr(0xA1)]),
        );
        match sweep.next_path() {
            Err(CrawlDiscoveryError::UnknownHop { kind, .. }) => {
                assert_eq!(kind, PoolKind::V4);
            }
            other => panic!("expected the unknown-hop refusal, got {other:?}"),
        }
    }

    // ── the sweep ───────────────────────────────────────────────────────────

    #[test]
    fn an_exhausted_sweep_stays_exhausted() {
        let mut sweep = sweep_of(
            two_pool_graph(),
            [TOKEN_A, TOKEN_B],
            &config(vec![addr(0xA1)], vec![addr(0xA1)]),
        );
        assert!(sweep.next_path().unwrap().is_some());
        assert!(sweep.next_path().unwrap().is_some());
        assert!(sweep.next_path().unwrap().is_none());
        assert!(
            sweep.is_exhausted(),
            "a drained sweep reports natural completion"
        );
        assert!(sweep.next_path().unwrap().is_none(), "and stays drained");
    }

    #[test]
    fn an_unbounded_depth_searches_the_whole_graph() {
        let mut config = config(vec![addr(0xA1)], vec![addr(0xA1)]);
        config.max_depth = None;
        let mut sweep = sweep_of(two_pool_graph(), [TOKEN_A, TOKEN_B], &config);
        let mut count = 0;
        while sweep.next_path().unwrap().is_some() {
            count += 1;
        }
        assert_eq!(count, 2);
    }
}
