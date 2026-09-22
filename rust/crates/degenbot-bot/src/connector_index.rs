//! DB-backed V2 connector index for the backrun frame solver (epic DFYDYI,
//! task B3): one startup load of the unified `pools` table's V2 edges, an
//! adjacency map by token id, and the two-hop candidate expansion the solver
//! needs: "other pools trading TOKEN against WETH".
//!
//! This is the Rust-native answer to the discovery prototype's adjacency
//! join (610k pools -> sub-ms connector lookups) — the DB read happens ONCE
//! at boot, never per frame.
//!
//! Truncation ranks by LIVE depth, not DB row order: the first fan for a
//! `(token, quote)` pair runs one Multicall3 batch of depth probes (V2
//! quote-side reserve, V3 in-range liquidity) and memoizes the descending
//! order, so every later frame for that pair walks the cached order with
//! zero reads.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use alloy::primitives::{address, Address, Bytes, B256, U256};
use async_trait::async_trait;
use degenbot_db::connection::DegenbotDb;
use degenbot_db::error::DbError;
use degenbot_pathfinding::PoolKind;
use degenbot_rpc::multicall3::{multicall3_batch, MulticallResult};
use degenbot_rpc::provider::AlloyProvider;
use parking_lot::RwLock;

/// `keccak256("getReserves()")[..4]` — the V2 pair depth probe (asserted
/// against alloy keccak in the tests).
pub const GET_RESERVES_SELECTOR: [u8; 4] = [0x09, 0x02, 0xf1, 0xac];

/// `keccak256("liquidity()")[..4]` — the V3 in-range depth probe (same test
/// assertion as the reserves selector).
pub const LIQUIDITY_SELECTOR: [u8; 4] = [0x1a, 0x68, 0x65, 0x02];

/// One V2 edge of the connector index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2Edge {
    pub pool_id: u64,
    pub token0_id: u64,
    pub token1_id: u64,
    pub address: Address,
}

/// One V4 managed-pool edge of the connector index — the identity the backrun
/// V4 lane needs to admit a post-state and compose a hop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V4Edge {
    /// The V4 `pool_hash` (`keccak(abi.encode(pool_key))`).
    pub pool_hash: B256,
    /// The `PoolManager` singleton address (`pool_managers.address`).
    pub manager: Address,
    /// `currency0` (V4's sorted-lower currency) token address.
    pub token0: Address,
    /// `currency1` token address.
    pub token1: Address,
    /// The direction-0 fee (`fee_currency0`) in pips of 1e6.
    ///
    /// Settlement reads this SAME column into `V4PoolKey.fee`
    /// (`bot_core::pool_builder::builder::resolve_v4_identity`), and the
    /// pool-updater writes its single `Initialize` fee into both columns
    /// (`upsert_v4_pools_on_conn`), so every row that pipeline creates has
    /// `fee_currency0 == fee_currency1`. [`Self::fee_currency1`] is retained
    /// separately so a direction-dependent pair is never silently collapsed.
    pub fee: u32,
    /// The direction-1 fee (`fee_currency1`) in pips of 1e6; see [`Self::fee`].
    pub fee_currency1: u32,
    /// The V4 tick spacing.
    pub tick_spacing: i32,
    /// The pool's hook contract address. Always [`Address::ZERO`]: hooked
    /// pools are excluded at load (backrun replay does not model hook
    /// intervention).
    pub hooks: Address,
    /// The `managed_pools.id` (V4's polymorphic DB primary key).
    pub db_pool_id: u64,
}

/// Outcome counts of one [`V2ConnectorIndex::load_v4`] scan. Every fetched row
/// lands in `loaded`, `excluded_hooked`, or `skipped`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct V4RosterLoad {
    /// Rows the chain-scoped scan returned.
    pub total: usize,
    /// Rows pushed into the roster.
    pub loaded: usize,
    /// Hooked rows excluded (backrun replay does not model hook intervention).
    pub excluded_hooked: usize,
    /// Rows dropped because a column did not fit its target type.
    pub skipped: usize,
}

/// One candidate pool handed to the depth ranker: identity + the orientation
/// a V2 score needs (which side of the pair the quote token sits on).
#[derive(Debug, Clone, Copy)]
pub struct RankCandidate {
    pub pool_id: u64,
    pub address: Address,
    pub kind: PoolKind,
    /// V2 candidates: `quote_id` is this pair's token0 (score = reserve0).
    pub quote_is_token0: bool,
}

/// Live depth scoring for connector candidates (higher = deeper). The index
/// consumes these scores once per `(token, quote)` pair and memoizes the
/// descending order; the driver attaches [`OnChainLiquidityRanker`] at
/// startup so truncation keeps the deepest pools, not the oldest rows.
#[async_trait]
pub trait ConnectorLiquidityRanker: Send + Sync {
    /// Depth per candidate; unprobed/failed pools score 0 (the stable sort
    /// keeps DB row order as the tiebreak).
    async fn rank(&self, candidates: &[RankCandidate]) -> Vec<(u64, u128)>;
}

/// Multicall3 depth probe: V2 pairs score their quote-side `getReserves`
/// reserve; V3 pools score their in-range `liquidity()`. Failed or short
/// sub-calls score 0 (sort last) — one dead pool never zeroes the fan.
pub struct OnChainLiquidityRanker {
    provider: Arc<AlloyProvider>,
}

impl fmt::Debug for OnChainLiquidityRanker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OnChainLiquidityRanker").finish()
    }
}

impl OnChainLiquidityRanker {
    #[must_use]
    pub fn new(provider: Arc<AlloyProvider>) -> Self {
        Self { provider }
    }

    /// V2 depth: the quote-side reserve (reserves return in PAIR token
    /// order). A reverted/short probe scores 0.
    #[must_use]
    pub fn v2_score(result: &MulticallResult, quote_is_token0: bool) -> u128 {
        if !result.success || result.return_data.len() < 64 {
            return 0;
        }
        let r0 = U256::from_be_slice(&result.return_data[..32]);
        let r1 = U256::from_be_slice(&result.return_data[32..64]);
        let quote = if quote_is_token0 { r0 } else { r1 };
        u128::try_from(quote).unwrap_or(u128::MAX)
    }

    /// V3 depth: the in-range `liquidity()` (uint128). A reverted probe
    /// scores 0.
    #[must_use]
    pub fn v3_score(result: &MulticallResult) -> u128 {
        if !result.success || result.return_data.len() < 32 {
            return 0;
        }
        let liq = U256::from_be_slice(&result.return_data[..32]);
        u128::try_from(liq).unwrap_or(u128::MAX)
    }
}

#[async_trait]
impl ConnectorLiquidityRanker for OnChainLiquidityRanker {
    async fn rank(&self, candidates: &[RankCandidate]) -> Vec<(u64, u128)> {
        let calls: Vec<(Address, Bytes)> = candidates
            .iter()
            .map(|c| {
                let selector: &[u8; 4] = match c.kind {
                    PoolKind::V2 => &GET_RESERVES_SELECTOR,
                    // Non-exhaustive upstream: V4 (and any future kind) has
                    // no depth probe here — never emitted by this index.
                    _ => &LIQUIDITY_SELECTOR,
                };
                (c.address, Bytes::copy_from_slice(selector))
            })
            .collect();
        let Ok(results) = multicall3_batch(&self.provider, &calls, None).await else {
            // Whole-batch transport failure: every pool scores 0 and the
            // ranking degenerates to row order for this pair (re-ranked on
            // the next first-touch — the memo was never written).
            return candidates.iter().map(|c| (c.pool_id, 0)).collect();
        };
        candidates
            .iter()
            .zip(results)
            .map(|(c, result)| {
                let score = match c.kind {
                    PoolKind::V2 => Self::v2_score(&result, c.quote_is_token0),
                    _ => Self::v3_score(&result),
                };
                (c.pool_id, score)
            })
            .collect()
    }
}

/// The fields the shared emit/candidate helpers need off either edge flavor.
trait EdgeIds: Copy {
    fn pool_id(self) -> u64;
    fn token0_id(self) -> u64;
    fn token1_id(self) -> u64;
    fn address(self) -> Address;
}

impl EdgeIds for V2Edge {
    fn pool_id(self) -> u64 {
        self.pool_id
    }
    fn token0_id(self) -> u64 {
        self.token0_id
    }
    fn token1_id(self) -> u64 {
        self.token1_id
    }
    fn address(self) -> Address {
        self.address
    }
}

impl EdgeIds for V3Edge {
    fn pool_id(self) -> u64 {
        self.pool_id
    }
    fn token0_id(self) -> u64 {
        self.token0_id
    }
    fn token1_id(self) -> u64 {
        self.token1_id
    }
    fn address(self) -> Address {
        self.address
    }
}

/// Every `(token_id, quote_id)` connector among `idxs` (row order) with the
/// descriptors the depth ranker probes; non-(token, quote) edges drop out.
fn rank_candidates<E: EdgeIds>(
    edges: &[E],
    idxs: &[usize],
    token_id: u64,
    quote_id: u64,
    kind: PoolKind,
) -> Vec<(usize, RankCandidate)> {
    idxs.iter()
        .filter_map(|&i| {
            let e = edges[i];
            let quote_is_token0 = if e.token0_id() == token_id && e.token1_id() == quote_id {
                false
            } else if e.token1_id() == token_id && e.token0_id() == quote_id {
                true
            } else {
                return None;
            };
            Some((
                i,
                RankCandidate {
                    pool_id: e.pool_id(),
                    address: e.address(),
                    kind,
                    quote_is_token0,
                },
            ))
        })
        .collect()
}

/// Emit `(edge, token is token0)` pairs along a precomputed edge order,
/// dropping `exclude_pool` (per-call) and truncating at `limit`.
fn emit_connectors<'a, E: EdgeIds>(
    edges: &'a [E],
    order: &[usize],
    token_id: u64,
    quote_id: u64,
    exclude_pool: u64,
    limit: usize,
) -> Vec<(&'a E, bool)> {
    let mut out = Vec::new();
    for &i in order {
        let e = &edges[i];
        if e.pool_id() == exclude_pool {
            continue;
        }
        if e.token0_id() == token_id && e.token1_id() == quote_id {
            out.push((e, true));
        } else if e.token1_id() == token_id && e.token0_id() == quote_id {
            out.push((e, false));
        }
        if out.len() >= limit {
            break;
        }
    }
    out
}

/// Memoized descending-depth edge order per connector pair key.
type RankedEdgeOrder = HashMap<(u64, u64), Arc<Vec<usize>>>;

/// Startup-loaded V2 adjacency (token id -> edges).
#[derive(Default)]
pub struct V2ConnectorIndex {
    edges: Vec<V2Edge>,
    by_pool: HashMap<u64, usize>,
    by_address: HashMap<Address, usize>,
    by_token: HashMap<u64, Vec<usize>>,
    /// V3 edges + their adjacency (the CL connector lane, B2-CL).
    v3_edges: Vec<V3Edge>,
    v3_by_pool: HashMap<u64, usize>,
    v3_by_address: HashMap<Address, usize>,
    v3_by_token: HashMap<u64, Vec<usize>>,
    /// V4 managed-pool roster + its `pool_hash`, token-pair, and manager
    /// adjacency (the backrun V4 connector lane).
    v4_edges: Vec<V4Edge>,
    v4_by_pool_hash: HashMap<B256, usize>,
    v4_by_pair: HashMap<(Address, Address), Vec<usize>>,
    v4_by_manager: HashMap<Address, Vec<usize>>,
    /// Outcome counts of the most recent [`Self::load_v4`] scan.
    v4_last_load: V4RosterLoad,
    /// Chain-scoped pool families this index does NOT type, keyed by the
    /// address a frame touches: a unified `pools` row's address is the pool
    /// contract, a `managed_pools` row's is the pool MANAGER
    /// (`pool_managers.address`). The descriptor seam consults this so an
    /// unsupported family observes loudly instead of dropping into an
    /// unexplained no-candidate.
    unsupported_by_address: HashMap<Address, String>,
    ranker: Option<Arc<dyn ConnectorLiquidityRanker>>,
    /// Memoized descending-depth edge order per `(token_id, quote_id)` — the
    /// memo is what keeps the per-frame fan cost unchanged (first touch
    /// ranks once; every later frame reads the cached order). A concurrent
    /// first-touch stampede may rank twice; last write wins (benign — the
    /// steady state is identical).
    ranked_v2: RwLock<RankedEdgeOrder>,
    ranked_v3: RwLock<RankedEdgeOrder>,
}

impl fmt::Debug for V2ConnectorIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("V2ConnectorIndex")
            .field("edges", &self.edges.len())
            .field("v3_edges", &self.v3_edges.len())
            .field("ranker", &self.ranker.is_some())
            .field("ranked_v2_pairs", &self.ranked_v2.read().len())
            .field("ranked_v3_pairs", &self.ranked_v3.read().len())
            .field("by_pool", &self.by_pool.len())
            .field("by_address", &self.by_address.len())
            .field("by_token", &self.by_token.len())
            .field("v3_by_pool", &self.v3_by_pool.len())
            .field("v3_by_address", &self.v3_by_address.len())
            .field("v3_by_token", &self.v3_by_token.len())
            .field("v4_edges", &self.v4_edges.len())
            .field("v4_by_pool_hash", &self.v4_by_pool_hash.len())
            .field("v4_by_pair", &self.v4_by_pair.len())
            .field("v4_by_manager", &self.v4_by_manager.len())
            .field("v4_last_load", &self.v4_last_load)
            .field("unsupported_by_address", &self.unsupported_by_address.len())
            .finish()
    }
}

impl V2ConnectorIndex {
    /// Load the V2 edges for `chain_id` from the DB (one scan, no per-frame
    /// queries).
    ///
    /// # Errors
    ///
    /// DB failures propagate (`DbError`). A `kind` outside the V2 family is
    /// skipped here — the unsupported-family roster captures it — so an
    /// unrecognized row cannot disable the lane at boot.
    pub fn load(db: &DegenbotDb, chain_id: i64) -> Result<Self, DbError> {
        let rows = db.fetch_connector_pool_rows(chain_id)?;
        let mut data = degenbot_db::pathfinding::PathGraphData::default();
        for row in rows {
            if !degenbot_db::schema::table::is_v2_kind(&row.kind) {
                continue;
            }
            // The DB emits `0x`-prefixed checksum addresses; `parse` accepts
            // those and the lowercase fixture form alike.
            let Ok(address) = row.address.parse::<Address>() else {
                continue;
            };
            data.edges
                .push((row.token0_id, row.token1_id, row.pool_id, PoolKind::V2));
            data.v2v3_addresses.insert(row.pool_id, address);
        }
        Self::from_graph_data(&data)
    }

    /// Build the V2 index from an already-fetched graph snapshot (the body of
    /// [`Self::load`], split so the V2-only admission runs without a DB).
    ///
    /// # Errors
    ///
    /// [`DbError::UnknownPoolKind`] when the snapshot carries a non-V2 edge:
    /// [`Self::load`] only ever pushes V2 edges, so a violation means the edge
    /// source and this loader disagree — refused, never dropped.
    fn from_graph_data(data: &degenbot_db::pathfinding::PathGraphData) -> Result<Self, DbError> {
        let mut index = Self::default();
        index.edges.reserve(data.edges.len());
        // PathEdge is (token0_id, token1_id, pool_id, kind) -- the pool id
        // rides THIRD (see fetch_path_graph_edges' push order).
        for (t0, t1, pool_id, kind) in &data.edges {
            if *kind != PoolKind::V2 {
                return Err(DbError::UnknownPoolKind {
                    kind: format!("{kind:?}"),
                    pool_id: i64::try_from(*pool_id).unwrap_or(i64::MAX),
                });
            }
            let Some(&address) = data.v2v3_addresses.get(pool_id) else {
                continue;
            };
            index.push_edge(V2Edge {
                pool_id: *pool_id,
                token0_id: *t0,
                token1_id: *t1,
                address,
            });
        }
        Ok(index)
    }

    /// In-memory edge insertion (the loader's body; pub so external
    /// pipelines/tests can assemble a fixture index without a DB).
    pub fn push_edge(&mut self, edge: V2Edge) {
        let idx = self.edges.len();
        self.by_pool.insert(edge.pool_id, idx);
        self.by_address.insert(edge.address, idx);
        self.by_token.entry(edge.token0_id).or_default().push(idx);
        self.by_token.entry(edge.token1_id).or_default().push(idx);
        self.edges.push(edge);
    }

    /// In-memory V3 edge insertion (the loader's body; pub for fixture
    /// assembly — see [`Self::push_edge`]).
    pub fn push_v3_edge(&mut self, edge: V3Edge) {
        let idx = self.v3_edges.len();
        self.v3_by_pool.insert(edge.pool_id, idx);
        self.v3_by_address.insert(edge.address, idx);
        self.v3_by_token
            .entry(edge.token0_id)
            .or_default()
            .push(idx);
        self.v3_by_token
            .entry(edge.token1_id)
            .or_default()
            .push(idx);
        self.v3_edges.push(edge);
    }

    /// In-memory V4 edge insertion (the loader's body; pub for fixture
    /// assembly — see [`Self::push_edge`]).
    pub fn push_v4_edge(&mut self, edge: V4Edge) {
        let idx = self.v4_edges.len();
        self.v4_by_pool_hash.insert(edge.pool_hash, idx);
        // V4 enforces currency0 < currency1, but a neighborhood walk may query
        // either orientation, so the pair key is canonicalized symmetric.
        let pair = if edge.token0 <= edge.token1 {
            (edge.token0, edge.token1)
        } else {
            (edge.token1, edge.token0)
        };
        self.v4_by_pair.entry(pair).or_default().push(idx);
        self.v4_by_manager
            .entry(edge.manager)
            .or_default()
            .push(idx);
        self.v4_edges.push(edge);
    }

    /// Attach the live depth ranker (call once at startup, before frames
    /// flow). Without one, truncation degrades to DB row order — the
    /// offline fallback, never the hot path.
    pub fn set_ranker(&mut self, ranker: Arc<dyn ConnectorLiquidityRanker>) {
        self.ranker = Some(ranker);
    }

    /// One ranked edge order per `(token, quote)` pair: memo hit answers with
    /// zero reads; a miss ranks ONCE (all candidates, unbounded — the memo
    /// then serves any per-frame cap) and stores the descending-depth order.
    async fn ranked_order(
        ranker: Option<&Arc<dyn ConnectorLiquidityRanker>>,
        memo: &RwLock<RankedEdgeOrder>,
        key: (u64, u64),
        candidates: &[(usize, RankCandidate)],
    ) -> Arc<Vec<usize>> {
        if let Some(order) = memo.read().get(&key).cloned() {
            return order;
        }
        let order = Arc::new(match ranker {
            None => candidates.iter().map(|(i, _)| *i).collect(),
            Some(ranker) => {
                let descs: Vec<RankCandidate> = candidates.iter().map(|(_, c)| *c).collect();
                let by_pool: HashMap<u64, u128> = ranker.rank(&descs).await.into_iter().collect();
                let mut scored: Vec<(usize, u128)> = candidates
                    .iter()
                    .map(|(i, c)| (*i, by_pool.get(&c.pool_id).copied().unwrap_or(0)))
                    .collect();
                // Descending depth; the stable sort keeps DB row order as
                // the tiebreak (and the no-ranker ordering degenerate).
                scored.sort_by_key(|(_, depth)| std::cmp::Reverse(*depth));
                scored.into_iter().map(|(i, _)| i).collect()
            }
        });
        memo.write().insert(key, Arc::clone(&order));
        order
    }

    /// The V2 edge degree for a `(token, quote)` pair: how many indexed
    /// edges actually connect the two. The per-frame quote selector reads
    /// this to keep only quotes the touched token really trades against.
    #[must_use]
    pub fn edge_degree(&self, token_id: u64, quote_id: u64) -> usize {
        let Some(idxs) = self.by_token.get(&token_id) else {
            return 0;
        };
        rank_candidates(&self.edges, idxs, token_id, quote_id, PoolKind::V2).len()
    }

    /// The V3 edge degree — the V3 half of the per-frame quote selector.
    #[must_use]
    pub fn v3_edge_degree(&self, token_id: u64, quote_id: u64) -> usize {
        let Some(idxs) = self.v3_by_token.get(&token_id) else {
            return 0;
        };
        rank_candidates(&self.v3_edges, idxs, token_id, quote_id, PoolKind::V3).len()
    }

    /// The edge of a known pool id.
    #[must_use]
    pub fn pool_edge(&self, pool_id: u64) -> Option<&V2Edge> {
        self.by_pool.get(&pool_id).map(|&i| &self.edges[i])
    }

    /// The edge of a pool by on-chain address (the feed frames name pools by
    /// address; the index id map resolves them).
    #[must_use]
    pub fn edge_by_address(&self, address: Address) -> Option<&V2Edge> {
        self.by_address.get(&address).map(|&i| &self.edges[i])
    }

    /// Connector candidates for a two-hop WETH-denominated backrun: pools
    /// trading `token_id` against `quote_id` (usually WETH), excluding
    /// `exclude_pool` (the affected pool). Returns `(pool edge, token is
    /// token0)` so the caller can orient hops without re-deriving, bounded
    /// by `limit` in DESCENDING LIVE DEPTH (first touch runs one Multicall3
    /// batch, further frames walk the memo).
    pub async fn connectors(
        &self,
        token_id: u64,
        quote_id: u64,
        exclude_pool: u64,
        limit: usize,
    ) -> Vec<(&V2Edge, bool)> {
        let Some(idxs) = self.by_token.get(&token_id) else {
            return Vec::new();
        };
        let candidates = rank_candidates(&self.edges, idxs, token_id, quote_id, PoolKind::V2);
        if candidates.is_empty() {
            return Vec::new();
        }
        let order = Self::ranked_order(
            self.ranker.as_ref(),
            &self.ranked_v2,
            (token_id, quote_id),
            &candidates,
        )
        .await;
        emit_connectors(&self.edges, &order, token_id, quote_id, exclude_pool, limit)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.edges.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    /// The loaded V3 edge count (the B2-CL lane surface).
    #[must_use]
    pub fn v3_len(&self) -> usize {
        self.v3_edges.len()
    }
}

// ───────────────────────── V3 edges (DFYDYI B2-CL) ─────────────────────────

/// One V3 edge: identity + the seed facts admission needs (fee in the 1e6
/// convention + tick spacing) without a second scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V3Edge {
    pub pool_id: u64,
    pub token0_id: u64,
    pub token1_id: u64,
    pub address: Address,
    /// The pool's fee, `bips` of 1e6 (3000 = 0.3%) -- the composer's unit.
    pub fee: u32,
    pub tick_spacing: i32,
}

impl V2ConnectorIndex {
    /// Load the V3 edges (all v3-kind pools joined to their family table for
    /// fee + tick spacing). One scan; call once at startup after `load`.
    ///
    /// # Errors
    ///
    /// DB failures propagate (`DbError`).
    pub fn load_v3(&mut self, db: &DegenbotDb, chain_id: i64) -> Result<(), DbError> {
        // Every supported V3 variant. The registry's `*-v3` pool types each
        // get their own variant table with an IDENTICAL column shape
        // (`fee_token0`/`fee_denominator` on one 1e6 denominator, so the fee
        // reads directly); a missing table for an unsupported variant is a
        // schema state this build never creates — refused via `?`, never
        // skipped.
        const V3_VARIANT_TABLES: [&str; 4] = [
            "uniswap_v3_pools",
            "pancakeswap_v3_pools",
            "sushiswap_v3_pools",
            "aerodrome_v3_pools",
        ];
        // The DB emits `0x`-prefixed checksum addresses; alloy parses those.
        // (rows::decode::decode_address is pub(crate) to degenbot-db.)
        let conn = db.lock();
        for table in V3_VARIANT_TABLES {
            let sql = format!(
                "SELECT p.id, p.token0_id, p.token1_id, p.address, v.fee_token0, v.tick_spacing \
                 FROM pools p JOIN {table} v ON v.pool_id = p.id WHERE p.chain = ?1"
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut rows = stmt.query([chain_id])?;
            while let Some(row) = rows.next()? {
                let id: i64 = row.get(0)?;
                let t0: i64 = row.get(1)?;
                let t1: i64 = row.get(2)?;
                let address: String = row.get(3)?;
                let fee: i64 = row.get(4)?;
                let tick_spacing: i64 = row.get(5)?;
                let (Ok(pool_id), Ok(token0_id), Ok(token1_id)) =
                    (u64::try_from(id), u64::try_from(t0), u64::try_from(t1))
                else {
                    continue;
                };
                let (Ok(fee), Ok(tick_spacing)) = (u32::try_from(fee), i32::try_from(tick_spacing))
                else {
                    continue;
                };
                // The DB emits `0x`-prefixed checksum addresses; alloy parses
                // those directly (rows::decode::decode_address is pub(crate)).
                let address: Address = match address.parse() {
                    Ok(a) => a,
                    Err(_) => continue,
                };
                self.push_v3_edge(V3Edge {
                    pool_id,
                    token0_id,
                    token1_id,
                    address,
                    fee,
                    tick_spacing,
                });
            }
        }
        Ok(())
    }

    /// V3 connector candidates trading `token_id` against `quote_id`, same
    /// contract as [`Self::connectors`]: `(edge, token is token0)`, ranked
    /// by in-range liquidity.
    pub async fn v3_connectors(
        &self,
        token_id: u64,
        quote_id: u64,
        exclude_pool: u64,
        limit: usize,
    ) -> Vec<(&V3Edge, bool)> {
        let Some(idxs) = self.v3_by_token.get(&token_id) else {
            return Vec::new();
        };
        let candidates = rank_candidates(&self.v3_edges, idxs, token_id, quote_id, PoolKind::V3);
        if candidates.is_empty() {
            return Vec::new();
        }
        let order = Self::ranked_order(
            self.ranker.as_ref(),
            &self.ranked_v3,
            (token_id, quote_id),
            &candidates,
        )
        .await;
        emit_connectors(
            &self.v3_edges,
            &order,
            token_id,
            quote_id,
            exclude_pool,
            limit,
        )
    }

    /// A V3 pool's edge by on-chain address.
    #[must_use]
    pub fn v3_edge_by_address(&self, address: Address) -> Option<&V3Edge> {
        self.v3_by_address.get(&address).map(|&i| &self.v3_edges[i])
    }

    /// A V3 pool's edge by pool id (the walker-cycle → index-edge join).
    #[must_use]
    pub fn v3_pool_edge(&self, pool_id: u64) -> Option<&V3Edge> {
        self.v3_by_pool.get(&pool_id).map(|&i| &self.v3_edges[i])
    }

    /// The flat `(token0_id, token1_id, pool_id, PoolKind)` edge list the
    /// discovery walker builds its `PathGraph` from — the SAME loaded edge
    /// set every fan reads; `PoolKind` carries the pool-table family.
    #[must_use]
    pub fn path_edges(&self) -> Vec<(u64, u64, u64, PoolKind)> {
        self.edges
            .iter()
            .map(|e| (e.token0_id, e.token1_id, e.pool_id, PoolKind::V2))
            .chain(
                self.v3_edges
                    .iter()
                    .map(|e| (e.token0_id, e.token1_id, e.pool_id, PoolKind::V3)),
            )
            .collect()
    }
}

// ───────────────────────── V4 edges (backrun roster) ─────────────────────────

impl V2ConnectorIndex {
    /// Load the chain-scoped V4 managed-pool roster (one scan; call once at
    /// startup after [`Self::load_v3`]).
    ///
    /// Hooked pools are excluded: backrun replay does not model hook
    /// intervention, so their post-states cannot be replayed. One INFO line
    /// reports the totals (loaded rows, excluded hooked rows).
    ///
    /// # Errors
    ///
    /// DB failures propagate (`DbError`).
    pub fn load_v4(&mut self, db: &DegenbotDb, chain_id: i64) -> Result<(), DbError> {
        let rows = db.fetch_v4_discovery_rows(chain_id)?;
        let total = rows.len();
        let mut loaded = 0usize;
        let mut excluded_hooked = 0usize;
        let mut skipped = 0usize;
        for row in rows {
            if row.hooks != Address::ZERO {
                excluded_hooked += 1;
                continue;
            }
            let converted = (|| -> Result<_, &'static str> {
                Ok((
                    u64::try_from(row.managed_pool_id).map_err(|_| "managed_pool_id")?,
                    u32::try_from(row.fee_currency0).map_err(|_| "fee_currency0")?,
                    u32::try_from(row.fee_currency1).map_err(|_| "fee_currency1")?,
                    i32::try_from(row.tick_spacing).map_err(|_| "tick_spacing")?,
                ))
            })();
            let (db_pool_id, fee, fee_currency1, tick_spacing) = match converted {
                Ok(values) => values,
                Err(field) => {
                    skipped += 1;
                    tracing::warn!(
                        chain_id,
                        pool_hash = %row.pool_hash,
                        field,
                        "v4 connector roster row skipped: column out of range for target type"
                    );
                    continue;
                }
            };
            self.push_v4_edge(V4Edge {
                pool_hash: row.pool_hash,
                manager: row.manager.address,
                token0: row.token0.address,
                token1: row.token1.address,
                fee,
                fee_currency1,
                tick_spacing,
                hooks: row.hooks,
                db_pool_id,
            });
            loaded += 1;
        }
        self.v4_last_load = V4RosterLoad {
            total,
            loaded,
            excluded_hooked,
            skipped,
        };
        tracing::info!(
            chain_id,
            loaded,
            total,
            excluded_hooked,
            skipped,
            "v4 connector roster loaded"
        );
        Ok(())
    }

    /// The V4 edge of a known `pool_hash`.
    #[must_use]
    pub fn v4_edge_by_pool_hash(&self, pool_hash: B256) -> Option<&V4Edge> {
        self.v4_by_pool_hash
            .get(&pool_hash)
            .map(|&i| &self.v4_edges[i])
    }

    /// Every V4 edge touching the unordered token pair `(token_a, token_b)` —
    /// the neighborhood walk. Orientation is recovered from the edge's
    /// `token0` / `token1`.
    #[must_use]
    pub fn v4_edges_for_pair(&self, token_a: Address, token_b: Address) -> Vec<&V4Edge> {
        let pair = if token_a <= token_b {
            (token_a, token_b)
        } else {
            (token_b, token_a)
        };
        self.v4_by_pair
            .get(&pair)
            .map(|idxs| idxs.iter().map(|&i| &self.v4_edges[i]).collect())
            .unwrap_or_default()
    }

    /// Every V4 edge owned by `manager` — the per-manager descriptor set.
    #[must_use]
    pub fn v4_edges_for_manager(&self, manager: Address) -> Vec<&V4Edge> {
        self.v4_by_manager
            .get(&manager)
            .map(|idxs| idxs.iter().map(|&i| &self.v4_edges[i]).collect())
            .unwrap_or_default()
    }

    /// The loaded V4 edge count.
    #[must_use]
    pub fn v4_len(&self) -> usize {
        self.v4_edges.len()
    }

    /// Outcome counts of the most recent [`Self::load_v4`] scan.
    #[must_use]
    pub fn v4_last_load(&self) -> V4RosterLoad {
        self.v4_last_load
    }

    /// Load the chain-scoped roster of pool families this index does NOT
    /// type (one scan; call once at startup after [`Self::load_v4`]).
    ///
    /// Keys are the address a frame touches: a unified `pools` row's address
    /// is the pool contract, a `managed_pools` row's is the pool MANAGER. An
    /// INFO line reports the total with a per-kind breakdown so a future
    /// family present in the DB is legible at boot.
    ///
    /// # Errors
    ///
    /// DB failures propagate (`DbError`).
    pub fn load_unsupported(&mut self, db: &DegenbotDb, chain_id: i64) -> Result<(), DbError> {
        let rows = db.fetch_unsupported_pool_addresses(chain_id)?;
        let mut by_address = HashMap::new();
        let mut by_kind: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for row in rows {
            *by_kind.entry(row.kind.clone()).or_default() += 1;
            by_address.insert(row.address, row.kind);
        }
        let total = by_address.len();
        let kinds = by_kind.len();
        self.unsupported_by_address = by_address;
        tracing::info!(
            chain_id,
            total,
            kinds,
            ?by_kind,
            "unsupported-family roster loaded"
        );
        Ok(())
    }

    /// The `kind` of an address whose pool family this index does not type
    /// (`None` for a supported or unrecorded address).
    #[must_use]
    pub fn unsupported_kind(&self, address: Address) -> Option<&str> {
        self.unsupported_by_address
            .get(&address)
            .map(String::as_str)
    }

    /// The loaded unsupported-family count.
    #[must_use]
    pub fn unsupported_len(&self) -> usize {
        self.unsupported_by_address.len()
    }
}

// ───────────────────────── startup evidence (LIVE) ─────────────────────────

/// The canonical mainnet WETH address for the ranking's evidence probe.
const EVIDENCE_QUOTE: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
/// The canonical mainnet USDC address for the ranking's evidence probe.
const EVIDENCE_TOKEN: Address = address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
/// The canonical (deepest) mainnet USDC/WETH Uniswap V2 pair — with a live
/// ranker attached, it must top the `(USDC, WETH)` connector ranking.
const EVIDENCE_DEEP_PAIR: Address = address!("b4e16d0168e52d35cacd2c6185b44281ec28c9dc");

/// Startup evidence (a LIVE probe — not a fixture, not a test): with a
/// ranker attached, the top of the USDC/WETH connector ranking must be the
/// canonical deep pair. The driver logs the `Err` loudly in evidence
/// mode, so a mis-ranked or row-ordered index announces itself instead of
/// silently truncating by age.
///
/// # Errors
///
/// `Err(String)` names the miss: the DB id join, an index lacking the deep
/// pair, or the observed ranking top.
pub async fn deep_pair_ranking_evidence(
    index: &V2ConnectorIndex,
    db: &DegenbotDb,
) -> Result<(), String> {
    let ids = db
        .fetch_token_ids_by_address(1, &[EVIDENCE_QUOTE, EVIDENCE_TOKEN])
        .map_err(|e| format!("rank evidence: USDC/WETH id join failed: {e}"))?;
    let (Some(&quote_id), Some(&token_id)) = (ids.get(&EVIDENCE_QUOTE), ids.get(&EVIDENCE_TOKEN))
    else {
        return Err("rank evidence: USDC/WETH token ids missing from the DB".into());
    };
    let deep = index.edge_by_address(EVIDENCE_DEEP_PAIR).ok_or_else(|| {
        "rank evidence: canonical USDC/WETH pool absent from the index".to_string()
    })?;
    let ranked = index.connectors(token_id, quote_id, 0, index.len()).await;
    match ranked.first() {
        Some((top, _)) if top.pool_id == deep.pool_id => Ok(()),
        Some((top, _)) => Err(format!(
            "rank evidence: deep pair {} did NOT top the ranking (observed top: {})",
            deep.pool_id, top.pool_id
        )),
        None => Err("rank evidence: no USDC/WETH connectors ranked".into()),
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn edge(pool_id: u64, t0: u64, t1: u64) -> V2Edge {
        V2Edge {
            pool_id,
            token0_id: t0,
            token1_id: t1,
            address: Address::new([u8::try_from(pool_id).unwrap_or(0); 20]),
        }
    }

    #[tokio::test]
    async fn adjacency_and_orientation() {
        let mut ix = V2ConnectorIndex::default();
        ix.push_edge(edge(1, 20, 10));
        ix.push_edge(edge(2, 10, 20));
        ix.push_edge(edge(3, 20, 30));

        // Connectors for TOK=20 against quote=10: pools 1 (tok is t0) and
        // 2 (tok is t1); pool 3 doesn't touch 10.
        let c = ix.connectors(20, 10, 0, 10).await;
        assert_eq!(c.len(), 2, "both V2 edges trading (20,10) surface");
        assert_eq!(c[0].0.pool_id, 1);
        assert!(c[0].1, "pool 1 has TOK as token0");
        assert_eq!(c[1].0.pool_id, 2);
        assert!(!c[1].1, "pool 2 has TOK as token1");

        // Exclusion drops the affected pool.
        let c = ix.connectors(20, 10, 2, 10).await;
        assert_eq!(c.len(), 1, "only pool 1 after excluding 2");

        // Limit bounds the fan-out.
        let c = ix.connectors(20, 10, 0, 1).await;
        assert_eq!(c.len(), 1);

        assert!(ix.pool_edge(3).is_some());
        assert!(ix.pool_edge(99).is_none());
    }

    #[tokio::test]
    async fn edge_degree_counts_only_pair_edges() {
        let mut ix = V2ConnectorIndex::default();
        // (20, 10) edges: 1, 2; (20, 30): 3; V3 (20, 10): 12.
        ix.push_edge(edge(1, 20, 10));
        ix.push_edge(edge(2, 10, 20));
        ix.push_edge(edge(3, 20, 30));
        ix.push_v3_edge(V3Edge {
            pool_id: 12,
            token0_id: 20,
            token1_id: 10,
            address: Address::new([12; 20]),
            fee: 500,
            tick_spacing: 10,
        });

        assert_eq!(ix.edge_degree(20, 10), 2);
        assert_eq!(ix.edge_degree(20, 30), 1);
        assert_eq!(ix.edge_degree(30, 10), 0);
        assert_eq!(ix.v3_edge_degree(20, 10), 1);
        assert_eq!(ix.v3_edge_degree(20, 30), 0);
    }

    /// A `pools` row under a family the index cannot type must not disable the
    /// V2 load: the recognized row still indexes, and the unknown one is left
    /// to `load_unsupported` rather than refused mid-scan.
    #[test]
    fn load_tolerates_unsupported_pool_kind() {
        const TOK: Address = Address::new([0x31; 20]);
        const OTHER: Address = Address::new([0x32; 20]);
        const PAIR: Address = Address::new([0xB1; 20]);
        const LFJ: Address = Address::new([0xB2; 20]);
        let (db, _state) =
            degenbot_db::connection::DegenbotDb::open_in_memory_for_writes().unwrap();
        let tok_id = u64::try_from(
            db.get_or_create_erc20_token(1, &TOK.to_checksum(None), None, None, None)
                .unwrap(),
        )
        .unwrap();
        let other_id = u64::try_from(
            db.get_or_create_erc20_token(1, &OTHER.to_checksum(None), None, None, None)
                .unwrap(),
        )
        .unwrap();
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO exchanges (id, chain_id, name, active, last_update_block, factory) \
                 VALUES (1, 1, 'test', 0, NULL, '0x0000000000000000000000000000000000000001')",
                [],
            )
            .unwrap();
            for (id, addr, kind) in [(1_i64, PAIR, "uniswap_v2"), (2_i64, LFJ, "lfj_binned")] {
                conn.execute(
                    "INSERT INTO pools (id, address, chain, kind, token0_id, token1_id, exchange_id) \
                     VALUES (?1, ?2, 1, ?3, ?4, ?5, 1)",
                    rusqlite::params![
                        id,
                        addr.to_checksum(None),
                        kind,
                        tok_id.cast_signed(),
                        other_id.cast_signed()
                    ],
                )
                .unwrap();
            }
        }

        let mut ix =
            V2ConnectorIndex::load(&db, 1).expect("an unsupported kind must not refuse the load");
        assert!(
            ix.edge_by_address(PAIR).is_some(),
            "the recognized V2 pair still indexes"
        );
        ix.load_unsupported(&db, 1).unwrap();
        assert_eq!(ix.unsupported_kind(LFJ), Some("lfj_binned"));
        assert!(
            degenbot_db::schema::table::is_lfj_kind("lfj_binned"),
            "the D8 roster kind is a DECLARED graph kind, not unclassifiable"
        );
    }

    /// Every supported V3 variant table feeds `load_v3`, chain-filtered.
    /// Regression (tx 0x3dcfe class): only `uniswap_v3_pools` was loaded, so
    /// Pancake/Sushi V3 pools were invisible descriptors — a frame swapping
    /// through one extracted as `Unsupported` and dropped before admission,
    /// producing zero path registrations with no signal anywhere.
    #[test]
    fn load_v3_admits_every_v3_variant_table_for_the_chain() {
        const TOK: Address = Address::new([0x11; 20]);
        const OTHER: Address = Address::new([0x22; 20]);
        let (db, _state) =
            degenbot_db::connection::DegenbotDb::open_in_memory_for_writes().unwrap();

        let tok_id = db
            .get_or_create_erc20_token(1, &TOK.to_checksum(None), None, None, None)
            .unwrap();
        let tok_id = u64::try_from(tok_id).unwrap();
        let _other_id = db
            .get_or_create_erc20_token(1, &OTHER.to_checksum(None), None, None, None)
            .unwrap();

        let conn = db.lock();
        conn.execute(
            "INSERT INTO exchanges (id, chain_id, name, active, last_update_block, factory) \
             VALUES (1, 1, 'test', 0, NULL, '0x0000000000000000000000000000000000000001')",
            [],
        )
        .unwrap();

        // One pool per V3 variant; Aerodrome is base-only in the registry.
        let variants: [(&str, &str, i64); 4] = [
            ("uniswap_v3", "uniswap_v3_pools", 1),
            ("pancakeswap_v3", "pancakeswap_v3_pools", 1),
            ("sushiswap_v3", "sushiswap_v3_pools", 1),
            // Aerodrome seeded on 8453: a chain-1 index must NOT carry it.
            ("aerodrome_v3", "aerodrome_v3_pools", 8453),
        ];
        for (i, (kind, variant_table, chain)) in variants.iter().enumerate() {
            let id = 50 + i64::try_from(i).unwrap();
            let addr = format!("{:?}", Address::new([u8::try_from(i).unwrap() + 0xB0; 20]));
            conn.execute(
                "INSERT INTO pools (id, address, chain, kind, token0_id, token1_id, exchange_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)",
                rusqlite::params![
                    id,
                    addr,
                    chain,
                    kind,
                    tok_id.cast_signed(),
                    (tok_id + 1).cast_signed()
                ],
            )
            .unwrap();
            conn.execute(
                &format!(
                    "INSERT INTO {variant_table} (pool_id, tick_spacing, liquidity_update_block, \
                     liquidity_update_log_index, fee_token0, fee_token1, fee_denominator) \
                     VALUES (?1, ?2, NULL, NULL, 500, 500, 1000000)"
                ),
                rusqlite::params![id, 10],
            )
            .unwrap();
        }
        drop(conn);

        let mut ix = V2ConnectorIndex::load(&db, 1).unwrap();
        ix.load_v3(&db, 1).unwrap();
        for (i, (kind, _, _)) in variants.iter().take(3).enumerate() {
            let addr = Address::new([u8::try_from(i).unwrap() + 0xB0; 20]);
            let edge = ix.v3_edge_by_address(addr);
            assert!(
                edge.is_some(),
                "{kind}: a pool in the DB must surface as an indexed V3 edge"
            );
            assert_eq!(edge.unwrap().fee, 500, "{kind}: fee from fee_token0");
        }
        let aerodrome = Address::new([0xB3; 20]);
        assert!(
            ix.v3_edge_by_address(aerodrome).is_none(),
            "a chain-1 index must not carry the 8453 Aerodrome pool"
        );

        let mut ix_base = V2ConnectorIndex::load(&db, 8453).unwrap();
        ix_base.load_v3(&db, 8453).unwrap();
        assert!(
            ix_base.v3_edge_by_address(aerodrome).is_some(),
            "the 8453 index carries the Aerodrome pool"
        );
    }

    /// A non-V2 edge in a V2-only snapshot is refused, not silently filtered.
    #[test]
    fn non_v2_edge_from_a_v2_query_is_refused() {
        let mut data = degenbot_db::pathfinding::PathGraphData::default();
        data.edges.push((10, 20, 1, PoolKind::V3));
        data.v2v3_addresses.insert(1, Address::ZERO);
        assert!(
            matches!(
                V2ConnectorIndex::from_graph_data(&data),
                Err(DbError::UnknownPoolKind { ref kind, pool_id: 1 }) if kind == "V3"
            ),
            "a non-V2 edge must be refused with UnknownPoolKind(V3)"
        );
    }

    /// Seed a chain-scoped V4 roster: two tokens, managers on chains 1 and
    /// 8453, and four managed pools (clean, hooked, other-chain, overflowing
    /// fee).
    fn seed_v4_roster(db: &degenbot_db::connection::DegenbotDb) {
        let t0 = Address::new([0x11; 20]).to_checksum(None);
        let t1 = Address::new([0x22; 20]).to_checksum(None);
        let manager = Address::new([0xaa; 20]).to_checksum(None);
        let manager2 = Address::new([0xbb; 20]).to_checksum(None);
        let clean_hash = format!("{:#x}", B256::from([0x11; 32]));
        let hooked_hash = format!("{:#x}", B256::from([0x22; 32]));
        let other_chain_hash = format!("{:#x}", B256::from([0x33; 32]));
        let overflow_fee_hash = format!("{:#x}", B256::from([0x44; 32]));
        let zero_hooks = Address::ZERO.to_checksum(None);
        let hooked_hooks = Address::new([0x01; 20]).to_checksum(None);
        {
            let conn = db.lock();
            conn.execute_batch(&format!(
                "PRAGMA foreign_keys=OFF;
                 INSERT INTO erc20_tokens (id, chain, address, name, symbol, decimals) VALUES
                   (1, 1, '{t0}', 'T0', 'T0', 18),
                   (2, 1, '{t1}', 'T1', 'T1', 6);
                 INSERT INTO exchanges (id, chain_id, name, active, factory) VALUES
                   (1, 1, 'uniswap_v4', 1, '0x0000000000000000000000000000000000000001');
                 INSERT INTO pool_managers (id, address, chain, kind, state_view, exchange_id) VALUES
                   (1, '{manager}', 1, 'uniswap_v4', NULL, 1),
                   (2, '{manager2}', 8453, 'uniswap_v4', NULL, 1);
                 INSERT INTO managed_pools (id, kind, manager_id) VALUES
                   (10, 'uniswap_v4', 1),
                   (11, 'uniswap_v4', 1),
                   (12, 'uniswap_v4', 2),
                   (13, 'uniswap_v4', 1);
                 INSERT INTO uniswap_v4_pools (managed_pool_id, pool_hash, hooks, currency0_id,
                   currency1_id, fee_currency0, fee_currency1, fee_denominator, tick_spacing) VALUES
                   (10, '{clean_hash}', '{zero_hooks}', 1, 2, 500, 500, 1000000, 10),
                   (11, '{hooked_hash}', '{hooked_hooks}', 1, 2, 3000, 3000, 1000000, 60),
                   (12, '{other_chain_hash}', '{zero_hooks}', 1, 2, 100, 100, 1000000, 1),
                   (13, '{overflow_fee_hash}', '{zero_hooks}', 1, 2, 5000000000, 500, 1000000, 10);"
            ))
            .unwrap();
        }
    }

    /// The V4 roster loader decodes a chain-scoped graph, drops hooked pools,
    /// and indexes by `pool_hash`, token pair, and manager.
    #[test]
    fn load_v4_roster_decodes_indexes_and_excludes_hooked() {
        let (db, _state) =
            degenbot_db::connection::DegenbotDb::open_in_memory_for_writes().unwrap();
        seed_v4_roster(&db);

        let mut ix = V2ConnectorIndex::default();
        ix.load_v4(&db, 1).unwrap();

        // Pool 10 loads; pool 11 (hooked) and pool 12 (chain 8453) do not.
        assert_eq!(ix.v4_len(), 1);
        let edge = ix
            .v4_edge_by_pool_hash(B256::from([0x11; 32]))
            .expect("clean chain-1 pool is indexed");
        assert_eq!(edge.db_pool_id, 10);
        assert_eq!(edge.manager, Address::new([0xaa; 20]));
        assert_eq!(edge.token0, Address::new([0x11; 20]));
        assert_eq!(edge.token1, Address::new([0x22; 20]));
        assert_eq!((edge.fee, edge.fee_currency1), (500, 500));
        assert_eq!(edge.tick_spacing, 10);
        assert_eq!(edge.hooks, Address::ZERO);

        assert!(
            ix.v4_edge_by_pool_hash(B256::from([0x22; 32])).is_none(),
            "hooked pool excluded at load"
        );
        assert!(
            ix.v4_edge_by_pool_hash(B256::from([0x33; 32])).is_none(),
            "other-chain pool excluded"
        );

        // Pair adjacency is orientation-independent.
        assert_eq!(
            ix.v4_edges_for_pair(Address::new([0x11; 20]), Address::new([0x22; 20]))
                .len(),
            1
        );
        assert_eq!(
            ix.v4_edges_for_pair(Address::new([0x22; 20]), Address::new([0x11; 20]))
                .len(),
            1
        );
        assert_eq!(
            ix.v4_edges_for_pair(Address::new([0x99; 20]), Address::new([0x22; 20]))
                .len(),
            0
        );

        // Manager grouping feeds the per-manager descriptor set.
        assert_eq!(ix.v4_edges_for_manager(Address::new([0xaa; 20])).len(), 1);
        assert_eq!(ix.v4_edges_for_manager(Address::new([0xbb; 20])).len(), 0);

        // The 8453 roster loads its own pool.
        let mut ix_base = V2ConnectorIndex::default();
        ix_base.load_v4(&db, 8453).unwrap();
        assert_eq!(ix_base.v4_len(), 1);
        assert_eq!(
            ix_base
                .v4_edge_by_pool_hash(B256::from([0x33; 32]))
                .expect("8453 pool is indexed on its chain")
                .db_pool_id,
            12
        );

        // A fee column exceeding u32 is skipped, not silently dropped: the
        // row is absent from the roster while the counters account for it.
        assert!(
            ix.v4_edge_by_pool_hash(B256::from([0x44; 32])).is_none(),
            "overflowing-fee row is not indexed"
        );
        let load = ix.v4_last_load();
        assert_eq!(
            load.total, 3,
            "chain-1 scan saw clean, hooked, and bad rows"
        );
        assert_eq!(load.loaded, 1);
        assert_eq!(load.excluded_hooked, 1);
        assert_eq!(load.skipped, 1, "the bad row is counted, not lost");
        assert_eq!(
            ix_base.v4_last_load(),
            V4RosterLoad {
                total: 1,
                loaded: 1,
                excluded_hooked: 0,
                skipped: 0,
            }
        );
    }
}

#[cfg(test)]
mod ranking_tests {
    use super::*;
    use alloy::primitives::keccak256;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Synthetic depth ranker: scores come from a fixed table (an independent
    /// source of truth, deliberately unrelated to row order); `reads` counts
    /// rank requests so the memoization test can assert zero second reads.
    struct TableRanker {
        scores: HashMap<u64, u128>,
        reads: AtomicUsize,
    }

    #[async_trait]
    impl ConnectorLiquidityRanker for TableRanker {
        async fn rank(&self, candidates: &[RankCandidate]) -> Vec<(u64, u128)> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            candidates
                .iter()
                .map(|c| (c.pool_id, self.scores.get(&c.pool_id).copied().unwrap_or(0)))
                .collect()
        }
    }

    fn table_ranker(scores: &[(u64, u128)]) -> Arc<TableRanker> {
        Arc::new(TableRanker {
            scores: scores.iter().copied().collect(),
            reads: AtomicUsize::new(0),
        })
    }

    fn edge(pool_id: u64, t0: u64, t1: u64) -> V2Edge {
        V2Edge {
            pool_id,
            token0_id: t0,
            token1_id: t1,
            address: Address::new([u8::try_from(pool_id).unwrap_or(0); 20]),
        }
    }

    fn v3_edge(pool_id: u64, t0: u64, t1: u64) -> V3Edge {
        V3Edge {
            pool_id,
            token0_id: t0,
            token1_id: t1,
            address: Address::new([u8::try_from(pool_id).unwrap_or(0); 20]),
            fee: 500,
            tick_spacing: 10,
        }
    }

    #[tokio::test]
    async fn ranked_top_k_beats_row_order() {
        // Insertion age (rows 1, 2, 3) is the INVERSE of the depth table:
        // with the backrun connectors=2 the truncation must keep the two deepest
        // pools, not the two oldest.
        let mut ix = V2ConnectorIndex::default();
        ix.push_edge(edge(1, 20, 10));
        ix.push_edge(edge(2, 10, 20));
        ix.push_edge(edge(3, 20, 10));
        ix.set_ranker(table_ranker(&[(1, 10), (2, 300), (3, 100)]));

        let top = ix.connectors(20, 10, 0, 2).await;
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].0.pool_id, 2, "deepest pool first");
        assert_eq!(top[1].0.pool_id, 3, "second-deepest next");

        // Exclusion stays per-call: drop the deep pool and the surviving
        // ranking is depth-ordered, not reshuffled.
        let top = ix.connectors(20, 10, 2, 2).await;
        assert_eq!(top[0].0.pool_id, 3);
        assert_eq!(top[1].0.pool_id, 1);
    }

    #[tokio::test]
    async fn ranked_top_k_beats_row_order_v3() {
        // The B2-CL lane truncates by the same depth order (in-range
        // liquidity scores), row order inverted again.
        let mut ix = V2ConnectorIndex::default();
        ix.push_v3_edge(v3_edge(11, 20, 10));
        ix.push_v3_edge(v3_edge(12, 10, 20));
        ix.push_v3_edge(v3_edge(13, 20, 10));
        ix.set_ranker(table_ranker(&[(11, 1_000), (12, 5), (13, 50)]));

        let top = ix.v3_connectors(20, 10, 0, 2).await;
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].0.pool_id, 11, "deepest V3 pool first");
        assert_eq!(top[1].0.pool_id, 13);
    }

    #[tokio::test]
    async fn ranking_memoizes_per_pair() {
        // The second frame for the same (token, quote) pair must perform ZERO
        // rank reads: the memoized order is what keeps per-frame cost
        // unchanged.
        let mut ix = V2ConnectorIndex::default();
        ix.push_edge(edge(1, 20, 10));
        let ranker = table_ranker(&[(1, 7)]);
        ix.set_ranker(Arc::clone(&ranker) as Arc<dyn ConnectorLiquidityRanker>);

        let first = ix.connectors(20, 10, 0, 8).await;
        let second = ix.connectors(20, 10, 0, 8).await;
        assert_eq!(
            ranker.reads.load(Ordering::SeqCst),
            1,
            "only the first touch ranks; the memo answers the second"
        );
        assert_eq!(first[0].0.pool_id, second[0].0.pool_id);
    }

    #[tokio::test]
    async fn no_ranker_falls_back_to_row_order() {
        // Offline construction (no live chain) keeps the pre-ranking
        // row-order truncation instead of failing.
        let mut ix = V2ConnectorIndex::default();
        ix.push_edge(edge(1, 20, 10));
        ix.push_edge(edge(2, 20, 10));
        let c = ix.connectors(20, 10, 0, 8).await;
        assert_eq!(c[0].0.pool_id, 1);
        assert_eq!(c[1].0.pool_id, 2);
    }

    #[test]
    fn depth_probe_selectors_and_decoding() {
        // Independent truth: alloy keccak of the canonical signatures. A
        // wrong selector would silently rank every pool at 0 (all probes
        // "fail").
        assert_eq!(&keccak256("getReserves()")[..4], &GET_RESERVES_SELECTOR);
        assert_eq!(&keccak256("liquidity()")[..4], &LIQUIDITY_SELECTOR);

        // A successful V2 probe scores the QUOTE-side reserve (reserves
        // return in PAIR token order).
        let mut ret = Vec::new();
        ret.extend_from_slice(&U256::from(111_u128).to_be_bytes::<32>());
        ret.extend_from_slice(&U256::from(999_u128).to_be_bytes::<32>());
        let ok = MulticallResult {
            success: true,
            return_data: Bytes::from(ret),
        };
        assert_eq!(OnChainLiquidityRanker::v2_score(&ok, true), 111);
        assert_eq!(OnChainLiquidityRanker::v2_score(&ok, false), 999);

        // V3 probes score in-range liquidity alone.
        let mut liq_ret = Vec::new();
        liq_ret.extend_from_slice(&U256::from(42_u128).to_be_bytes::<32>());
        let ok_liq = MulticallResult {
            success: true,
            return_data: Bytes::from(liq_ret),
        };
        assert_eq!(OnChainLiquidityRanker::v3_score(&ok_liq), 42);

        // A reverted probe scores 0 (sorts last) instead of aborting the
        // batch.
        let failed = MulticallResult {
            success: false,
            return_data: Bytes::new(),
        };
        assert_eq!(OnChainLiquidityRanker::v2_score(&failed, false), 0);
        assert_eq!(OnChainLiquidityRanker::v3_score(&failed), 0);
    }
}
