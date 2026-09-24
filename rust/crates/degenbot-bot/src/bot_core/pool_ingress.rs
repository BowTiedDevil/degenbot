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

use std::collections::HashSet;
use std::sync::Arc;

use alloy::primitives::{Address, U256};
use degenbot_db::snapshot::{LiquidityMap, TickMapDb};
use degenbot_pools::tick_fetch::TickBootstrapRpc;
use degenbot_pools::v3_state::{ClSlotLayout, PoolTickCoverage};
use degenbot_pools::TickInfo;
use degenbot_rpc::provider::AlloyProvider;
use hashbrown::HashMap;
use parking_lot::Mutex;

use crate::bot_core::liquidity_verifier::verify_v3_liquidity_map;
use crate::bot_core::planning::{ExplicitPoolState, PlanningPoolParams, Workspace};
use crate::bot_core::tick_assembly::{
    assemble_v3_tick_map, liquidity_map_to_tick_info, TickMapAssemblyError,
};

// The seed's provenance tag + value ride at the planning boundary; re-export
// here so ingress consumers reach them through the provisioning module. Db and
// Chain provenance constructors stay crate-private (minted only by this
// module); `TickMapSeed::journal` is the public exact-replay constructor.
pub use crate::bot_core::planning::{TickMapSeed, TickMapSource};

/// The chain-sample verification policy for ingress V3 admission.
///
/// Sampling is DISTINCT from integrity: [`VerifyLevel::Off`] disables the
/// chain sample only — the unconditional Tracked self-contradiction abort and
/// the two-stamp liquidity clock still run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerifyLevel {
    /// Chain-sample-verify every admission (new-lane development stance).
    Strict,
    /// Verify the first admission per pool per process, then memoize the
    /// result (the pump's verified-once discipline inherited by the per-frame
    /// stance). Production default.
    #[default]
    Bootstrap,
    /// No chain-sample verification; the operator has declared confidence.
    /// Integrity checks are unaffected.
    Off,
}

impl VerifyLevel {
    /// The stable label (`strict`/`bootstrap`/`off`).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Bootstrap => "bootstrap",
            Self::Off => "off",
        }
    }
}

/// A chain-sample verifier for a staged V3 tick map: the ingress composes it
/// behind [`VerifyLevel`] without knowing the RPC shape. The concrete
/// [`AlloySampleVerifier`] delegates to the pump's own snapshot-block
/// machinery (`liquidity_verifier`), not a reimplementation.
#[async_trait::async_trait]
pub trait TickMapSampleVerifier: Send + Sync {
    /// Verify `ticks` against on-chain state at `block`.
    ///
    /// # Errors
    ///
    /// A genuine mismatch or a transport failure, as a loud string.
    async fn verify_v3(
        &self,
        address: Address,
        ticks: &HashMap<i32, TickInfo>,
        block: u64,
    ) -> Result<(), String>;
}

/// The `AlloyProvider`-backed sample verifier: delegates to the snapshot-block
/// [`verify_v3_liquidity_map`] so the ingress and the settlement pump share one
/// verification implementation.
pub struct AlloySampleVerifier {
    provider: Arc<AlloyProvider>,
}

impl AlloySampleVerifier {
    #[must_use]
    pub fn new(provider: Arc<AlloyProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait::async_trait]
impl TickMapSampleVerifier for AlloySampleVerifier {
    async fn verify_v3(
        &self,
        address: Address,
        ticks: &HashMap<i32, TickInfo>,
        block: u64,
    ) -> Result<(), String> {
        verify_v3_liquidity_map(self.provider.as_ref(), address, ticks, block, "ingress")
            .await
            .map_err(|e| e.to_string())
    }
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
    /// The chain-sample verification refused the staged map (mismatch or
    /// transport failure). A loud decline, never a silent fallback.
    Verify(String),
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
            Self::Verify(_) => "admit-v3-verify",
        }
    }

    /// The step's error detail (empty for width refusals, whose identity is
    /// the stage itself).
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::Slot0Fetch(d)
            | Self::TickMapFetch(d)
            | Self::Db(d)
            | Self::Register(d)
            | Self::Verify(d) => d.clone(),
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
    /// The chain-sample policy (default [`VerifyLevel::Bootstrap`]).
    verify_level: VerifyLevel,
    /// Pools whose staged map has already been sample-verified. Process-scoped
    /// by design: the per-frame workspace is re-sized every frame, but the
    /// verification result for a pool is a process-lifetime fact.
    verified: Mutex<HashSet<Address>>,
    /// The chain-sample verifier. Absent means the Chain arm is unconfigured
    /// and sampling is skipped (an RPC-less Db-only ingress cannot verify).
    verifier: Option<Arc<dyn TickMapSampleVerifier>>,
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
            verify_level: VerifyLevel::default(),
            verified: Mutex::new(HashSet::new()),
            verifier: None,
        }
    }

    /// Attach the Chain-arm RPC after construction (the driver resolves the
    /// provider after the runtime is built).
    pub fn set_chain(&mut self, chain: Arc<dyn TickBootstrapRpc>) {
        self.chain = Some(chain);
    }

    /// Set the chain-sample verification policy. Setting [`VerifyLevel::Off`]
    /// emits one loud journal entry: the operator has declared confidence, and
    /// the unconditional integrity checks still run.
    pub fn set_verify_level(&mut self, level: VerifyLevel) {
        if level == VerifyLevel::Off {
            tracing::warn!(
                "pool ingress verify_level=off: chain-sample verification of staged V3 tick maps is disabled by operator declaration; self-contradictory Tracked maps and the two-stamp clock still abort"
            );
        }
        self.verify_level = level;
    }

    /// The active chain-sample policy.
    #[must_use]
    pub fn verify_level(&self) -> VerifyLevel {
        self.verify_level
    }

    /// Attach the chain-sample verifier (the driver wires the provider-backed
    /// adapter after the runtime is built).
    pub fn set_verifier(&mut self, verifier: Arc<dyn TickMapSampleVerifier>) {
        self.verifier = Some(verifier);
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
    ) -> Result<TickMapSeed, IngressDecline> {
        if let Some(db) = self.db.as_ref() {
            if let Some(map) = self.memoized_map(address, block, db.as_ref())? {
                let seed = match liquidity_map_to_tick_info(map, tick_spacing)? {
                    Some((ticks, coverage)) => TickMapSeed::db(ticks, coverage, block),
                    None => TickMapSeed::db(HashMap::new(), PoolTickCoverage::Tracked, block),
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
            Some((ticks, coverage)) => TickMapSeed::chain(ticks, coverage, block),
            None => TickMapSeed::chain(HashMap::new(), PoolTickCoverage::Sparse, block),
        })
    }

    /// Stage, chain-sample-verify (per [`VerifyLevel`]), then register a V3
    /// pool. The verification composes the pump's snapshot-block machinery
    /// through [`TickMapSampleVerifier`]; a mismatch or transport failure is a
    /// loud [`IngressDecline::Verify`], never a silent fallback.
    ///
    /// # Errors
    ///
    /// [`IngressDecline`] naming the refused staging, verification, or
    /// registration step.
    pub async fn admit_v3_verified(
        &self,
        ws: &mut Workspace,
        params: IngressV3Params,
        head: u64,
    ) -> Result<u64, IngressDecline> {
        let seed = self.v3_tick_map(params.address, params.tick, params.tick_spacing, head)?;
        self.verify_staged_v3(params.address, &seed).await?;
        Self::register_v3(ws, params, seed, head)
    }

    /// Register a staged V3 seed into `ws`, naming the typed rejection on
    /// refusal.
    fn register_v3(
        ws: &mut Workspace,
        params: IngressV3Params,
        seed: TickMapSeed,
        head: u64,
    ) -> Result<u64, IngressDecline> {
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
                seed,
                slot_layout: params.slot_layout,
            },
            head,
        )
        .map_err(|e| IngressDecline::Register(format!("{e:?}")))
    }

    /// Apply the [`VerifyLevel`] policy to a freshly staged seed. Integrity
    /// already ran inside staging (the Tracked self-contradiction abort is
    /// upstream of this method and unconditional); this is the chain SAMPLE
    /// only.
    async fn verify_staged_v3(
        &self,
        address: Address,
        seed: &TickMapSeed,
    ) -> Result<(), IngressDecline> {
        if self.verify_level == VerifyLevel::Off {
            return Ok(());
        }
        // No verifier means no chain arm is wired: there is nothing to sample
        // against, so the Db-only ingress proceeds.
        let Some(verifier) = self.verifier.as_ref() else {
            return Ok(());
        };
        if self.verify_level == VerifyLevel::Bootstrap && self.verified.lock().contains(&address) {
            return Ok(());
        }
        verifier
            .verify_v3(address, &seed.ticks, seed.seed_block)
            .await
            .map_err(IngressDecline::Verify)?;
        self.verified.lock().insert(address);
        Ok(())
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
        assert_eq!(
            IngressDecline::Verify("mismatch".into()).stage(),
            "admit-v3-verify"
        );
        let _ = CHAIN;
    }

    /// A verifier fake: counts calls and can fail on demand.
    struct FakeVerifier {
        calls: Arc<AtomicUsize>,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl TickMapSampleVerifier for FakeVerifier {
        async fn verify_v3(
            &self,
            _address: Address,
            _ticks: &HashMap<i32, TickInfo>,
            _block: u64,
        ) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err("on-chain mismatch".into())
            } else {
                Ok(())
            }
        }
    }

    fn params(address: Address) -> IngressV3Params {
        IngressV3Params {
            address,
            token0: Address::new([0x02; 20]),
            token1: Address::new([0x03; 20]),
            fee: 3_000,
            tick_spacing: 60,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            slot_layout: ClSlotLayout::UniswapV3,
        }
    }

    /// A self-contradictory Tracked map: a bitmap bit set at compressed tick 2
    /// (tick 120) with no gross-positive row there (the row sits at tick 180).
    fn contradictory_map() -> LiquidityMap {
        let mut bitmap = U256::ZERO;
        bitmap.set_bit(2, true);
        let mut tick_bitmap = HashMap::new();
        tick_bitmap.insert(0i64, BitmapAtWord { bitmap });
        let mut tick_data = HashMap::new();
        tick_data.insert(
            180,
            LiquidityAtTick {
                liquidity_gross: U256::from(1u64),
                liquidity_net: 0,
            },
        );
        LiquidityMap {
            tick_bitmap,
            tick_data,
        }
    }

    fn empty_db() -> (Arc<FakeDb>, Arc<AtomicUsize>) {
        let fetches = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(FakeDb {
                maps: HashMap::new(),
                fetches: Arc::clone(&fetches),
            }),
            fetches,
        )
    }

    #[test]
    fn verify_level_defaults_to_bootstrap() {
        let ingress = PoolIngress::new(None, None);
        assert_eq!(ingress.verify_level(), VerifyLevel::Bootstrap);
        assert_eq!(VerifyLevel::default(), VerifyLevel::Bootstrap);
        assert_eq!(VerifyLevel::Strict.label(), "strict");
        assert_eq!(VerifyLevel::Bootstrap.label(), "bootstrap");
        assert_eq!(VerifyLevel::Off.label(), "off");
    }

    #[tokio::test]
    async fn bootstrap_verifies_the_first_admission_and_memoizes() {
        let (db, _) = empty_db();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut ingress = PoolIngress::new(Some(db), None);
        ingress.set_verify_level(VerifyLevel::Bootstrap);
        ingress.set_verifier(Arc::new(FakeVerifier {
            calls: Arc::clone(&calls),
            fail: false,
        }));
        let mut ws = Workspace::new();
        ingress
            .admit_v3_verified(&mut ws, params(POOL), 100)
            .await
            .expect("admits");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "first admission verified");
        let _ = ingress.admit_v3_verified(&mut ws, params(POOL), 101).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the per-pool memo skips the second admission's sample"
        );
    }

    #[tokio::test]
    async fn strict_verifies_every_admission() {
        let (db, _) = empty_db();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut ingress = PoolIngress::new(Some(db), None);
        ingress.set_verify_level(VerifyLevel::Strict);
        ingress.set_verifier(Arc::new(FakeVerifier {
            calls: Arc::clone(&calls),
            fail: false,
        }));
        let mut ws = a_new_workspace();
        let _ = ingress.admit_v3_verified(&mut ws, params(POOL), 100).await;
        let _ = ingress.admit_v3_verified(&mut ws, params(POOL), 101).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2, "Strict never memoizes");
    }

    #[tokio::test]
    async fn off_skips_sampling() {
        let (db, _) = empty_db();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut ingress = PoolIngress::new(Some(db), None);
        ingress.set_verify_level(VerifyLevel::Off);
        ingress.set_verifier(Arc::new(FakeVerifier {
            calls: Arc::clone(&calls),
            fail: true,
        }));
        let mut ws = a_new_workspace();
        ingress
            .admit_v3_verified(&mut ws, params(POOL), 100)
            .await
            .expect("Off admits without sampling even when the verifier would fail");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "Off never calls the verifier"
        );
    }

    #[tokio::test]
    async fn off_never_proceeds_past_a_self_contradictory_tracked_map() {
        let fetches = Arc::new(AtomicUsize::new(0));
        let mut maps = HashMap::new();
        maps.insert(POOL, contradictory_map());
        let db = Arc::new(FakeDb {
            maps,
            fetches: Arc::clone(&fetches),
        });
        let mut ingress = PoolIngress::new(Some(db), None);
        ingress.set_verify_level(VerifyLevel::Off);
        let mut ws = a_new_workspace();
        let err = ingress
            .admit_v3_verified(&mut ws, params(POOL), 100)
            .await
            .expect_err("integrity abort is unconditional");
        assert_eq!(err.stage(), "admit-v3-db");
    }

    #[tokio::test]
    async fn a_failed_sample_is_a_loud_verify_decline() {
        let (db, _) = empty_db();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut ingress = PoolIngress::new(Some(db), None);
        ingress.set_verify_level(VerifyLevel::Strict);
        ingress.set_verifier(Arc::new(FakeVerifier {
            calls: Arc::clone(&calls),
            fail: true,
        }));
        let mut ws = a_new_workspace();
        let err = ingress
            .admit_v3_verified(&mut ws, params(POOL), 100)
            .await
            .expect_err("a mismatch must decline, not fall back");
        assert_eq!(err.stage(), "admit-v3-verify");
        assert!(err.detail().contains("mismatch"));
    }

    fn a_new_workspace() -> Workspace {
        Workspace::new()
    }
}
