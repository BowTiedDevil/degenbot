//! Pool ingress: the single home for V3 pool-state admission into a planning
//! [`Workspace`].
//!
//! Before this module the tree carried four V3 tick-map staging
//! implementations with two precedence semantics: the settlement registration
//! arms (`tick_assembly` / `pool_builder`) staged `Db → Chain`, while the
//! backrun cold-hop ladder and the touched-anchor window merge staged
//! Chain-only with a current-word ± 1 clamp. A wide/full-range depositor
//! (the live feed's dominant anchor family) then entered the frame sandbox
//! with an empty or one-sided map, and every anchored chain rejected
//! `sequence_unavailable` — the pool entered state through a different module
//! than the one whose data the resolver needed.
//!
//! This facade composes the existing internals behind one `Db → Chain`
//! interface: the Db arm is `TickMapDb::fetch_liquidity_map` converted by
//! [`tick_assembly::liquidity_map_to_tick_info`] (with its Tracked intake
//! reconciliation), the Chain arm is `tick_assembly`'s sparse single-word
//! bootstrap. Callers never read tick words.
//!
//! # Layout honesty
//!
//! The ingress does NOT own the fork slot layout. [`TickMapDb::fetch_liquidity_map`]
//! returns raw tick data — `tick → liquidity_net/gross`, the ABI-decoded
//! `ticks(int24)` cells and `tickBitmap` words — not slot math, so it is
//! layout-agnostic and correct for every V3 fork. The layout that produced
//! the staged `slot0`/`liquidity` scalars (read by the caller through the
//! connector edge's [`ClSlotLayout`]) rides into
//! [`ExplicitPoolState::V3::slot_layout`] unchanged; the ingress never drops
//! or defaults it.
//!
//! # Error posture
//!
//! Db and Chain read failures propagate loudly (the `tick_assembly` Decision
//! 8 (A) posture): a transient Db error must not silently degrade to a
//! solver-unsafe sparse state. [`IngressDecline`] carries the refused arm so
//! the per-hop JSONL trace can name the stage.

use std::sync::Arc;

use alloy::primitives::{Address, U256};
use degenbot_db::snapshot::{LiquidityMap, TickMapDb};
use degenbot_pools::tick_fetch::TickBootstrapRpc;
use degenbot_pools::v3_state::{ClSlotLayout, PoolTickCoverage};
use degenbot_pools::TickInfo;
use hashbrown::HashMap;
use parking_lot::Mutex;

use crate::bot_core::planning::{ExplicitPoolState, PlanningPoolParams, Workspace};
use crate::bot_core::tick_assembly::{
    assemble_v3_tick_map, liquidity_map_to_tick_info, TickMapAssemblyError,
};

/// Which arm staged a V3 tick map — the `anchor_ticks` provenance field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickMapSource {
    /// `TickMapDb::fetch_liquidity_map` supplied the complete map (coverage
    /// `Tracked`).
    Db,
    /// The sparse chain bootstrap supplied the current bitmap word (coverage
    /// `Sparse`).
    Chain,
}

impl TickMapSource {
    /// The stable JSONL label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Db => "Db",
            Self::Chain => "Chain",
        }
    }
}

/// A staged V3 tick map with its coverage and provenance.
#[derive(Debug, Clone)]
pub struct V3TickMapSeed {
    /// The per-tick liquidity cells (raw values; layout-agnostic).
    pub ticks: HashMap<i32, TickInfo>,
    /// The coverage tag the workspace registers with (`Tracked` from the Db
    /// arm, `Sparse` from the Chain arm).
    pub coverage: PoolTickCoverage,
    /// Which arm produced the map.
    pub source: TickMapSource,
}

/// Why the ingress could not admit or stage a V3 pool. Stages map 1:1 onto
/// the per-hop JSONL witness labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngressDecline {
    /// The `slot0`/`liquidity` `eth_call` failed.
    Slot0Fetch(String),
    /// A slot0/liquidity value did not fit its wire type.
    Slot0Width,
    /// The Chain-arm tick-map bootstrap failed (transport/revert or a
    /// malformed return).
    TickMapFetch(String),
    /// The Db arm failed (a read error or a self-contradictory Tracked map).
    Db(String),
    /// The workspace refused the fetched state (spec violation,
    /// `AlreadyRegistered`, ...).
    Register(String),
}

impl IngressDecline {
    /// The stable JSONL `stage` label for the refused step.
    #[must_use]
    pub fn stage(&self) -> &'static str {
        match self {
            Self::Slot0Fetch(_) => "admit-v3-slot0-fetch",
            Self::Slot0Width => "admit-v3-slot0-width",
            Self::TickMapFetch(_) => "admit-v3-tick-map-fetch",
            Self::Db(_) => "admit-v3-db",
            Self::Register(_) => "admit-v3-register",
        }
    }

    /// The step's error detail (empty for width refusals, whose identity is
    /// the stage itself).
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::Slot0Fetch(d) | Self::TickMapFetch(d) | Self::Db(d) | Self::Register(d) => {
                d.clone()
            }
            Self::Slot0Width => "slot0/liquidity value out of type range".into(),
        }
    }
}

impl From<TickMapAssemblyError> for IngressDecline {
    fn from(e: TickMapAssemblyError) -> Self {
        match e {
            TickMapAssemblyError::Db(inner) => Self::Db(inner.to_string()),
            TickMapAssemblyError::Chain(inner) => Self::TickMapFetch(inner.to_string()),
            other @ TickMapAssemblyError::InconsistentTickMap { .. } => Self::Db(other.to_string()),
        }
    }
}

/// A V3 pool's admission payload. `slot_layout` is mandatory — the fork the
/// caller READ the `slot0`/`liquidity` scalars through; the ingress passes it
/// to [`ExplicitPoolState::V3`] verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngressV3Params {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,
    pub tick_spacing: i32,
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
    pub slot_layout: ClSlotLayout,
}

/// The per-block memoized Db view: one frozen set of `LiquidityMap` reads for
/// the block currently being staged (the `BotStateDb` storage-memo shape,
/// bounded by the pools the frame touches).
struct MapMemo {
    block: u64,
    maps: HashMap<Address, Option<LiquidityMap>>,
}

impl MapMemo {
    /// A memo with no block yet staged (every first read re-fills).
    fn empty() -> Self {
        Self {
            block: u64::MAX,
            maps: HashMap::new(),
        }
    }
}

/// The V3 pool ingress: `Db → Chain` tick-map precedence behind one interface,
/// with a per-block memo of the Db reads.
pub struct PoolIngress {
    db: Option<Arc<dyn TickMapDb>>,
    chain: Option<Arc<dyn TickBootstrapRpc>>,
    memo: Mutex<MapMemo>,
}

impl PoolIngress {
    /// Build an ingress over the Db and Chain arms. Either may be absent: no
    /// Db falls through to the Chain arm; no Chain yields an empty `Sparse`
    /// map on a Db miss (never a fabricated word).
    #[must_use]
    pub fn new(db: Option<Arc<dyn TickMapDb>>, chain: Option<Arc<dyn TickBootstrapRpc>>) -> Self {
        Self {
            db,
            chain,
            memo: Mutex::new(MapMemo::empty()),
        }
    }

    /// Attach the Chain-arm RPC after construction (the driver resolves the
    /// provider after the runtime is built).
    pub fn set_chain(&mut self, chain: Arc<dyn TickBootstrapRpc>) {
        self.chain = Some(chain);
    }

    /// Whether a Db handle is wired (the anchor trace reports the arm).
    #[must_use]
    pub fn has_db(&self) -> bool {
        self.db.is_some()
    }

    /// Stage a V3 tick map with `Db → Chain` precedence.
    ///
    /// The Db arm is a per-block memoized `fetch_liquidity_map` conversion; a
    /// pool present in the Db with an empty map is a legitimately-empty
    /// `Tracked` pool (never degraded to `Sparse`). A Db miss falls to the
    /// Chain arm (`tick_assembly`'s sparse single-word bootstrap); a Chain
    /// miss yields an empty `Sparse` map.
    ///
    /// # Errors
    ///
    /// [`IngressDecline::Db`] on a Db read failure or a self-contradictory
    /// Tracked map; [`IngressDecline::TickMapFetch`] on a Chain-arm failure.
    pub fn v3_tick_map(
        &self,
        address: Address,
        tick: i32,
        tick_spacing: i32,
        block: u64,
    ) -> Result<V3TickMapSeed, IngressDecline> {
        if let Some(db) = self.db.as_ref() {
            if let Some(map) = self.memoized_map(address, block, db.as_ref())? {
                let seed = match liquidity_map_to_tick_info(map, tick_spacing)? {
                    Some((ticks, coverage)) => V3TickMapSeed {
                        ticks,
                        coverage,
                        source: TickMapSource::Db,
                    },
                    None => V3TickMapSeed {
                        ticks: HashMap::new(),
                        coverage: PoolTickCoverage::Tracked,
                        source: TickMapSource::Db,
                    },
                };
                return Ok(seed);
            }
        }
        // Chain arm: `db=None` short-circuits `assemble_v3_tick_map`'s Db arm
        // so the precedence helper's Chain path runs unchanged.
        let chain_hit = assemble_v3_tick_map(
            None,
            address,
            tick,
            tick_spacing,
            block,
            self.chain.as_deref(),
        )?;
        Ok(match chain_hit {
            Some((ticks, coverage)) => V3TickMapSeed {
                ticks,
                coverage,
                source: TickMapSource::Chain,
            },
            None => V3TickMapSeed {
                ticks: HashMap::new(),
                coverage: PoolTickCoverage::Sparse,
                source: TickMapSource::Chain,
            },
        })
    }

    /// Register a V3 pool into `ws` with the Db-first staged map. The caller
    /// supplies the slot0/liquidity scalars it READ (through `slot_layout`);
    /// the ingress owns only the tick map.
    ///
    /// # Errors
    ///
    /// [`IngressDecline`] naming the refused staging or registration step.
    pub fn admit_v3(
        &self,
        ws: &mut Workspace,
        params: IngressV3Params,
        head: u64,
    ) -> Result<u64, IngressDecline> {
        let seed = self.v3_tick_map(params.address, params.tick, params.tick_spacing, head)?;
        ws.register_with_state(
            PlanningPoolParams {
                address: params.address,
                token0: params.token0,
                token1: params.token1,
            },
            ExplicitPoolState::V3 {
                sqrt_price_x96: params.sqrt_price_x96,
                liquidity: params.liquidity,
                tick: params.tick,
                fee: params.fee,
                tick_spacing: params.tick_spacing,
                tick_data: seed.ticks,
                coverage: seed.coverage,
                slot_layout: params.slot_layout,
            },
            head,
        )
        .map_err(|e| IngressDecline::Register(format!("{e:?}")))
    }

    /// The Db read behind the per-block memo. A miss is `None` (the caller
    /// falls through to the Chain arm); an error propagates.
    fn memoized_map(
        &self,
        address: Address,
        block: u64,
        db: &dyn TickMapDb,
    ) -> Result<Option<LiquidityMap>, IngressDecline> {
        let mut memo = self.memo.lock();
        if memo.block != block {
            memo.block = block;
            memo.maps.clear();
        }
        if let Some(hit) = memo.maps.get(&address) {
            return Ok(hit.clone());
        }
        let fetched = db
            .fetch_liquidity_map(address)
            .map_err(|e| IngressDecline::Db(e.to_string()))?;
        memo.maps.insert(address, fetched.clone());
        Ok(fetched)
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use alloy::primitives::{aliases::U128, B256};
    use degenbot_db::error::DbError;
    use degenbot_db::snapshot::{BitmapAtWord, LiquidityAtTick};
    use degenbot_db::ExchangeFamily;
    use degenbot_pools::tick_fetch::{BootstrapTickError, BootstrapTickWord};

    const CHAIN: i64 = 1;
    const POOL: Address = Address::new([0x11; 20]);

    /// A `TickMapDb` fake with a per-call fetch counter (memo assertions).
    struct FakeDb {
        maps: HashMap<Address, LiquidityMap>,
        fetches: Arc<AtomicUsize>,
    }

    impl TickMapDb for FakeDb {
        fn fetch_liquidity_map(
            &self,
            pool_address: Address,
        ) -> Result<Option<LiquidityMap>, DbError> {
            self.fetches.fetch_add(1, Ordering::SeqCst);
            Ok(self.maps.get(&pool_address).cloned())
        }

        fn fetch_liquidity_map_v4(
            &self,
            _pool_manager: Address,
            _pool_id_hash: B256,
        ) -> Result<Option<LiquidityMap>, DbError> {
            Ok(None)
        }

        fn fetch_newest_update_block(
            &self,
            _chain: i64,
            _family: ExchangeFamily,
        ) -> Result<Option<i64>, DbError> {
            Ok(None)
        }

        fn fetch_liquidity_update_block(
            &self,
            _pool_address: Address,
        ) -> Result<Option<i64>, DbError> {
            Ok(None)
        }

        fn fetch_liquidity_update_block_v4(
            &self,
            _pool_manager: Address,
            _pool_id_hash: B256,
        ) -> Result<Option<i64>, DbError> {
            Ok(None)
        }
    }

    /// A Chain-arm fake returning one initialized tick per call.
    #[derive(Debug)]
    struct FakeChain {
        tick: i32,
        gross: u128,
        net: i128,
    }

    impl TickBootstrapRpc for FakeChain {
        fn bootstrap_v3_tick_word(
            &self,
            _pool_address: &str,
            _tick: i32,
            _tick_spacing: i32,
            block: u64,
        ) -> Result<Option<BootstrapTickWord>, BootstrapTickError> {
            let mut ticks = HashMap::new();
            ticks.insert(
                self.tick,
                TickInfo {
                    liquidity_gross: U128::from(self.gross),
                    liquidity_net: self.net,
                    block,
                },
            );
            Ok(Some(BootstrapTickWord { word: 0, ticks }))
        }

        fn bootstrap_v4_tick_word(
            &self,
            _state_view: &str,
            _pool_id: &[u8; 32],
            _tick: i32,
            _tick_spacing: i32,
            _block: u64,
        ) -> Result<Option<BootstrapTickWord>, BootstrapTickError> {
            Ok(None)
        }
    }

    /// A consistent map: one initialized tick `120` in word 0 at spacing 60.
    fn map_with_tick(tick: i32, spacing: i32) -> LiquidityMap {
        let compressed = tick.div_euclid(spacing);
        let word = i64::from(compressed >> 8);
        let bit = u32::try_from(compressed.rem_euclid(256)).unwrap();
        let mut bitmap = U256::ZERO;
        bitmap.set_bit(bit as usize, true);
        let mut tick_bitmap = HashMap::new();
        tick_bitmap.insert(word, BitmapAtWord { bitmap });
        let mut tick_data = HashMap::new();
        tick_data.insert(
            tick,
            LiquidityAtTick {
                liquidity_gross: U256::from(1_000u64),
                liquidity_net: 500,
            },
        );
        LiquidityMap {
            tick_bitmap,
            tick_data,
        }
    }

    fn ingress_with(db: Option<Arc<dyn TickMapDb>>, fetches: &Arc<AtomicUsize>) -> PoolIngress {
        let _ = fetches;
        PoolIngress::new(db, None)
    }

    #[test]
    fn db_arm_wins_and_stages_the_complete_map() {
        let fetches = Arc::new(AtomicUsize::new(0));
        let mut maps = HashMap::new();
        maps.insert(POOL, map_with_tick(120, 60));
        let db = Arc::new(FakeDb {
            maps,
            fetches: Arc::clone(&fetches),
        });
        let ingress = ingress_with(Some(db), &fetches);
        let seed = ingress.v3_tick_map(POOL, 0, 60, 100).expect("stages");
        assert_eq!(seed.source, TickMapSource::Db);
        assert_eq!(seed.coverage, PoolTickCoverage::Tracked);
        assert!(seed.ticks.contains_key(&120));
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn db_reads_memoize_per_block() {
        let fetches = Arc::new(AtomicUsize::new(0));
        let mut maps = HashMap::new();
        maps.insert(POOL, map_with_tick(120, 60));
        let db = Arc::new(FakeDb {
            maps,
            fetches: Arc::clone(&fetches),
        });
        let ingress = ingress_with(Some(db), &fetches);
        ingress.v3_tick_map(POOL, 0, 60, 100).unwrap();
        ingress.v3_tick_map(POOL, 0, 60, 100).unwrap();
        assert_eq!(
            fetches.load(Ordering::SeqCst),
            1,
            "the second read in the same block is memoized"
        );
        ingress.v3_tick_map(POOL, 0, 60, 101).unwrap();
        assert_eq!(fetches.load(Ordering::SeqCst), 2, "a new block re-reads");
    }

    #[test]
    fn db_miss_without_chain_is_empty_sparse() {
        let fetches = Arc::new(AtomicUsize::new(0));
        let db = Arc::new(FakeDb {
            maps: HashMap::new(),
            fetches: Arc::clone(&fetches),
        });
        let ingress = ingress_with(Some(db), &fetches);
        let seed = ingress.v3_tick_map(POOL, 0, 60, 100).expect("stages");
        assert_eq!(seed.source, TickMapSource::Chain);
        assert_eq!(seed.coverage, PoolTickCoverage::Sparse);
        assert!(seed.ticks.is_empty(), "no word is fabricated on a miss");
    }

    #[test]
    fn chain_arm_stages_sparse_when_db_misses() {
        let fetches = Arc::new(AtomicUsize::new(0));
        let db = Arc::new(FakeDb {
            maps: HashMap::new(),
            fetches: Arc::clone(&fetches),
        });
        let chain = Arc::new(FakeChain {
            tick: 120,
            gross: 2_000,
            net: -700,
        });
        let ingress = PoolIngress::new(Some(db), Some(chain));
        let seed = ingress.v3_tick_map(POOL, 0, 60, 100).expect("stages");
        assert_eq!(seed.source, TickMapSource::Chain);
        assert_eq!(seed.coverage, PoolTickCoverage::Sparse);
        assert_eq!(
            seed.ticks.get(&120).unwrap().liquidity_gross,
            U128::from(2_000u64)
        );
    }

    #[test]
    fn decline_stages_are_stable_labels() {
        assert_eq!(
            IngressDecline::Slot0Fetch("timeout".into()).stage(),
            "admit-v3-slot0-fetch"
        );
        assert_eq!(IngressDecline::Slot0Width.stage(), "admit-v3-slot0-width");
        assert_eq!(
            IngressDecline::TickMapFetch("revert".into()).stage(),
            "admit-v3-tick-map-fetch"
        );
        assert_eq!(IngressDecline::Db("locked".into()).stage(), "admit-v3-db");
        assert_eq!(
            IngressDecline::Register("AlreadyRegistered".into()).stage(),
            "admit-v3-register"
        );
        let _ = CHAIN;
    }
}
