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

use alloy::primitives::{Address, B256, I256, U128, U256};
use degenbot_db::snapshot::{
    BitmapAtWord as DbBitmapAtWord, LiquidityAtTick as DbLiquidityAtTick, LiquidityMap, TickMapDb,
};
use degenbot_db::LiquidityUpdateEvent;
use degenbot_math::cl::liquidity_mapping::{
    apply_liquidity_mapping_update, BitmapAtWord as ApplyBitmapAtWord,
    LiquidityAtTick as ApplyLiquidityAtTick,
};
pub use degenbot_pool_updater::fetch::{AlloyLiquidityLogSource, LiquidityLogSource};
use degenbot_pools::tick_fetch::TickBootstrapRpc;
use degenbot_pools::v3_state::ClSlotLayout;
use degenbot_pools::TickInfo;
use degenbot_rpc::liquidity_verifier::{
    verify_liquidity_map, LiquidityMap as RpcLiquidityMap, LiquidityMapTarget,
};
use degenbot_rpc::provider::AlloyProvider;
use hashbrown::HashMap;
use parking_lot::Mutex;

use crate::bot_core::planning::{ExplicitPoolState, PlanningPoolParams, Workspace};
use crate::bot_core::pool_builder::builder::derive_hook_flags;
use crate::bot_core::tick_assembly::{chain_arm, resolve_tick_map_arm, TickMapAssemblyError};

// The seed's provenance tag + value ride at the planning boundary; re-export
// here so ingress consumers reach them through the provisioning module. Db and
// Chain provenance constructors stay crate-private (minted only by this
// module); the Journal constructor is crate-private for capability tests.
pub use crate::bot_core::planning::{TickMapPoolIdentity, TickMapSeed, TickMapSource};

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

/// The ingress's backfill witness sink: the strategy layer's per-hop JSONL
/// capture, behind a trait so the ingress stays free of any capture
/// implementation.
pub trait IngressWitness: Send + Sync {
    /// A Db-staged map was advanced from `from_block` to `to_block` by
    /// `events` decoded liquidity events.
    fn db_backfill(
        &self,
        identity: TickMapPoolIdentity,
        from_block: u64,
        to_block: u64,
        events: usize,
    );
}

/// A Db staging arm: the snapshot handle plus the transport that closes its
/// lag to head.
///
/// The pairing is mandatory. A Db-staged map whose ledger sits behind `head`
/// is ALWAYS advanced by the pool's decoded Mint/Burn events before
/// admission; there is no window cap and no transport-less deferral, so a Db
/// arm that cannot backfill is unrepresentable rather than a silent fall to
/// the sparse Chain arm.
pub struct DbArm {
    db: Arc<dyn TickMapDb>,
    backfill_source: Arc<dyn LiquidityLogSource>,
}

impl DbArm {
    /// Pair a Db snapshot handle with the transport that backfills its lag.
    #[must_use]
    pub fn new(db: Arc<dyn TickMapDb>, backfill_source: Arc<dyn LiquidityLogSource>) -> Self {
        Self {
            db,
            backfill_source,
        }
    }
}

/// The contract identity sampled for one complete tick map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickMapSampleTarget {
    /// A V3 pool contract.
    V3(Address),
    /// A V4 pool's full-map storage owner and pool id.
    V4 { manager: Address, pool_id: B256 },
}

/// A chain-sample verifier for a staged CL tick map. The concrete
/// [`AlloySampleVerifier`] delegates both families to the shared full-map
/// verifier; this seam exists only for ingress policy and test doubles.
#[async_trait::async_trait]
pub trait TickMapSampleVerifier: Send + Sync {
    /// Verify one complete map against its contract target at `block`.
    ///
    /// # Errors
    ///
    /// A genuine mismatch or a transport failure, as a loud string.
    async fn verify(
        &self,
        target: TickMapSampleTarget,
        map: &RpcLiquidityMap,
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
    async fn verify(
        &self,
        target: TickMapSampleTarget,
        map: &RpcLiquidityMap,
        block: u64,
    ) -> Result<(), String> {
        let target = match target {
            TickMapSampleTarget::V3(address) => LiquidityMapTarget::V3(address),
            TickMapSampleTarget::V4 { manager, pool_id } => LiquidityMapTarget::V4 {
                pool_manager: manager,
                pool_id,
            },
        };
        let facts = verify_liquidity_map(self.provider.as_ref(), target, map, Some(block))
            .await
            .map_err(|e| e.to_string())?;
        if facts.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "liquidity map verification returned {} divergences",
                facts.len()
            ))
        }
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

/// A V4 pool's admission payload. `state_view` is bootstrap-only; complete
/// map verification always targets `manager` through the shared verifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngressV4Params {
    pub manager: Address,
    pub state_view: Option<Address>,
    pub pool_id: B256,
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,
    pub tick_spacing: i32,
    pub hooks: Address,
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
}

/// A Db-staged map plus the backfill window that brought it to head.
#[derive(Clone)]
struct StagedDbMap {
    map: LiquidityMap,
    /// The Db map's own liquidity-clock stamp, before any backfill.
    source_block: Option<u64>,
    /// `None` when the map already sat at head (window 0).
    backfill: Option<BackfillStamp>,
}

/// The backfill window applied to a staged map.
#[derive(Clone, Copy)]
struct BackfillStamp {
    from_block: u64,
    to_block: u64,
    events: usize,
}

/// The per-block memoized Db view: one frozen set of `LiquidityMap` reads plus
/// the backfill windows that brought them to head (the `BotStateDb`
/// storage-memo shape, bounded by the pools the frame touches).
struct MapMemo {
    block: u64,
    maps: HashMap<TickMapPoolIdentity, Option<StagedDbMap>>,
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
    db: Option<DbArm>,
    chain: Option<Arc<dyn TickBootstrapRpc>>,
    memo: Mutex<MapMemo>,
    /// The chain-sample policy (default [`VerifyLevel::Bootstrap`]).
    verify_level: VerifyLevel,
    /// Pool identity → the freshness block whose staged map was sampled.
    /// Bootstrap memoization is process-scoped but never suppresses a later
    /// admission whose seed block requires a fresh sample.
    verified: Mutex<HashMap<TickMapPoolIdentity, u64>>,
    /// The chain-sample verifier. Absent means the Chain arm is unconfigured
    /// and sampling is skipped (an RPC-less Db-only ingress cannot verify).
    verifier: Option<Arc<dyn TickMapSampleVerifier>>,
    /// The backfill witness sink (the strategy's JSONL capture).
    witness: Option<Arc<dyn IngressWitness>>,
}

impl PoolIngress {
    /// Build an ingress over the Db and Chain arms. Either may be absent: no
    /// Db falls through to the Chain arm; no Chain yields an empty `Sparse`
    /// map on a Db miss (never a fabricated word). A [`DbArm`] carries its own
    /// backfill transport, so the Db and its window-closing source cannot
    /// drift apart.
    #[must_use]
    pub fn new(db: Option<DbArm>, chain: Option<Arc<dyn TickBootstrapRpc>>) -> Self {
        Self {
            db,
            chain,
            memo: Mutex::new(MapMemo::empty()),
            verify_level: VerifyLevel::default(),
            verified: Mutex::new(HashMap::new()),
            verifier: None,
            witness: None,
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

    /// Attach the backfill witness sink.
    pub fn set_witness(&mut self, witness: Arc<dyn IngressWitness>) {
        self.witness = Some(witness);
    }

    /// Whether a Db handle is wired (the anchor trace reports the arm).
    #[must_use]
    pub fn has_db(&self) -> bool {
        self.db.is_some()
    }

    /// Stage a V3 tick map with `Db → Chain` precedence.
    ///
    /// The Db arm routes through the shared
    /// [`resolve_tick_map_arm`](crate::bot_core::tick_assembly::resolve_tick_map_arm)
    /// over the per-block memoized `fetch_liquidity_map`, advanced to `block`
    /// by the pool's decoded Mint/Burn events when the ledger sits behind head.
    /// A pool present in the Db with an empty map is a legitimately-empty
    /// `Tracked` pool (never degraded to `Sparse`). A Db miss falls to the
    /// sync Chain arm; a Chain miss mints an empty `Sparse` map through
    /// [`chain_arm`](crate::bot_core::tick_assembly::chain_arm).
    ///
    /// # Errors
    ///
    /// [`IngressDecline::Db`] on a Db read failure or a self-contradictory
    /// Tracked map; [`IngressDecline::TickMapFetch`] on a Chain-arm failure.
    fn stage_v3_tick_map(
        &self,
        address: Address,
        tick: i32,
        tick_spacing: i32,
        block: u64,
    ) -> Result<TickMapSeed, IngressDecline> {
        // The Db arm is staged + backfilled to head under the per-block memo;
        // a Db miss or a corrupt map returns `None`, so the shared precedence
        // falls to the Chain arm below.
        let identity = TickMapPoolIdentity::V3(address);
        let staged = self.staged_db_arm(identity, tick_spacing, block)?;
        let source_block = staged.as_ref().and_then(|staged| staged.source_block);
        let db_arm = resolve_tick_map_arm::<IngressDecline, _>(
            &format!("{address}"),
            identity,
            tick_spacing,
            block,
            || Ok(staged.map(|staged| staged.map)),
        )?;
        if let Some(mut seed) = db_arm {
            seed.source_block = source_block;
            return Ok(seed);
        }
        // Chain arm: the Db missed (or no Db handle is wired).
        let chain_hit = match self.chain.as_deref() {
            Some(chain) => chain
                .bootstrap_v3_tick_word(&address.to_checksum(None), tick, tick_spacing, block)
                .map_err(TickMapAssemblyError::Chain)?
                .map(|word| (word.ticks, HashMap::from([(word.word, word.bitmap)]))),
            None => None,
        };
        let (ticks, bitmaps) = chain_hit.unwrap_or((HashMap::new(), HashMap::new()));
        Ok(chain_arm(Some(ticks), bitmaps).into_seed(block, identity))
    }

    /// Stage a V4 `(PoolManager, PoolId)` map with `Db → Chain` precedence.
    #[cfg(test)]
    fn stage_v4_tick_map(
        &self,
        manager: Address,
        pool_id: B256,
        tick: i32,
        tick_spacing: i32,
        block: u64,
    ) -> Result<TickMapSeed, IngressDecline> {
        self.stage_v4_tick_map_with_state_view(manager, pool_id, None, tick, tick_spacing, block)
    }

    fn stage_v4_tick_map_with_state_view(
        &self,
        manager: Address,
        pool_id: B256,
        state_view: Option<Address>,
        tick: i32,
        tick_spacing: i32,
        block: u64,
    ) -> Result<TickMapSeed, IngressDecline> {
        let identity = TickMapPoolIdentity::V4 { manager, pool_id };
        let staged = self.staged_db_arm(identity, tick_spacing, block)?;
        let source_block = staged.as_ref().and_then(|staged| staged.source_block);
        let db_arm = resolve_tick_map_arm::<IngressDecline, _>(
            &alloy::hex::encode_prefixed(pool_id),
            identity,
            tick_spacing,
            block,
            || Ok(staged.map(|staged| staged.map)),
        )?;
        if let Some(mut seed) = db_arm {
            seed.source_block = source_block;
            return Ok(seed);
        }
        let chain_hit = match (self.chain.as_deref(), state_view) {
            (Some(chain), Some(state_view)) => chain
                .bootstrap_v4_tick_word(
                    &state_view.to_checksum(None),
                    &pool_id.0,
                    tick,
                    tick_spacing,
                    block,
                )
                .map_err(TickMapAssemblyError::Chain)?
                .map(|word| (word.ticks, HashMap::from([(word.word, word.bitmap)]))),
            _ => None,
        };
        let (ticks, bitmaps) = chain_hit.unwrap_or((HashMap::new(), HashMap::new()));
        Ok(chain_arm(Some(ticks), bitmaps).into_seed(block, identity))
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
        let seed =
            self.stage_v3_tick_map(params.address, params.tick, params.tick_spacing, head)?;
        self.verify_staged(
            TickMapPoolIdentity::V3(params.address),
            params.tick_spacing,
            &seed,
        )
        .await?;
        Self::register_v3(ws, params, seed, head)
    }

    /// Merge replay-provided touched rows into the Db/Chain seed, project those
    /// post-state rows onto the seed's bitmap, verify the final map under the
    /// active policy, and only then register it. Existing bitmap words and bits
    /// are preserved, including words previously checked empty.
    ///
    /// # Errors
    ///
    /// Returns the same typed [`IngressDecline`] stages as cold-hop admission.
    pub async fn admit_v3_replay(
        &self,
        ws: &mut Workspace,
        params: IngressV3Params,
        overlay: HashMap<i32, TickInfo>,
        head: u64,
    ) -> Result<u64, IngressDecline> {
        let mut seed =
            self.stage_v3_tick_map(params.address, params.tick, params.tick_spacing, head)?;
        merge_replay_overlay(&mut seed, overlay, params.tick_spacing, "V3")?;
        self.verify_staged(
            TickMapPoolIdentity::V3(params.address),
            params.tick_spacing,
            &seed,
        )
        .await?;
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
    async fn verify_staged(
        &self,
        identity: TickMapPoolIdentity,
        tick_spacing: i32,
        seed: &TickMapSeed,
    ) -> Result<(), IngressDecline> {
        if self.verify_level == VerifyLevel::Off {
            return Ok(());
        }
        let Some(verifier) = self.verifier.as_ref() else {
            return Ok(());
        };
        if seed.coverage != degenbot_pools::v3_state::PoolTickCoverage::Tracked {
            return Ok(());
        }
        if self.verify_level == VerifyLevel::Bootstrap
            && self.verified.lock().get(&identity) == Some(&seed.seed_block)
        {
            return Ok(());
        }
        let target = match identity {
            TickMapPoolIdentity::V3(address) => TickMapSampleTarget::V3(address),
            TickMapPoolIdentity::V4 { manager, pool_id } => {
                TickMapSampleTarget::V4 { manager, pool_id }
            }
        };
        let map = RpcLiquidityMap::tracked_with_spacing(
            seed.ticks.clone(),
            seed.bitmaps.clone(),
            tick_spacing,
        );
        verifier
            .verify(target, &map, seed.seed_block)
            .await
            .map_err(IngressDecline::Verify)?;
        self.verified.lock().insert(identity, seed.seed_block);
        Ok(())
    }

    /// Stage, policy-sample, and register explicit V4 state without replay
    /// overlay. This is the cold-hop/explicit-state ingress behavior.
    ///
    /// # Errors
    ///
    /// Returns a typed staging, verification, or registration decline.
    pub async fn admit_v4_verified(
        &self,
        ws: &mut Workspace,
        params: IngressV4Params,
        head: u64,
    ) -> Result<u64, IngressDecline> {
        let seed = self.stage_v4_tick_map_with_state_view(
            params.manager,
            params.pool_id,
            params.state_view,
            params.tick,
            params.tick_spacing,
            head,
        )?;
        self.verify_staged(
            TickMapPoolIdentity::V4 {
                manager: params.manager,
                pool_id: params.pool_id,
            },
            params.tick_spacing,
            &seed,
        )
        .await?;
        Self::register_v4(ws, params, seed, head)
    }

    /// Merge replay-provided V4 rows into the Db/Chain seed using the pool's
    /// real spacing, verify the final bitmap+tick map, then register it.
    ///
    /// # Errors
    ///
    /// Returns the same typed staging, verification, or registration declines
    /// as explicit V4 admission.
    pub async fn admit_v4_replay(
        &self,
        ws: &mut Workspace,
        params: IngressV4Params,
        overlay: HashMap<i32, TickInfo>,
        head: u64,
    ) -> Result<u64, IngressDecline> {
        let mut seed = self.stage_v4_tick_map_with_state_view(
            params.manager,
            params.pool_id,
            params.state_view,
            params.tick,
            params.tick_spacing,
            head,
        )?;
        merge_replay_overlay(&mut seed, overlay, params.tick_spacing, "V4")?;
        self.verify_staged(
            TickMapPoolIdentity::V4 {
                manager: params.manager,
                pool_id: params.pool_id,
            },
            params.tick_spacing,
            &seed,
        )
        .await?;
        Self::register_v4(ws, params, seed, head)
    }

    fn register_v4(
        ws: &mut Workspace,
        params: IngressV4Params,
        seed: TickMapSeed,
        head: u64,
    ) -> Result<u64, IngressDecline> {
        ws.register_with_state(
            PlanningPoolParams {
                address: params.manager,
                token0: params.token0,
                token1: params.token1,
            },
            ExplicitPoolState::V4 {
                pool_id: params.pool_id.0,
                fee: params.fee,
                tick_spacing: params.tick_spacing,
                hooks: params.hooks,
                hook_flags: derive_hook_flags(params.hooks),
                protocol_fee: 0,
                sqrt_price_x96: params.sqrt_price_x96,
                liquidity: params.liquidity,
                tick: params.tick,
                seed,
            },
            head,
        )
        .map_err(|error| IngressDecline::Register(format!("{error:?}")))
    }

    /// The per-block-memoized Db staging: fetch the pool's map + liquidity
    /// stamp, close the Db→head window by applying decoded Mint/Burn events,
    /// and hand the map to the shared Tracked intake. A Db miss is `None` (the
    /// Chain arm stages instead); a map with no stamp is staged verbatim,
    /// since its currency cannot be judged.
    ///
    /// # Errors
    ///
    /// [`IngressDecline::Db`] on a Db read failure, a backfill transport
    /// failure, or a tick/word outside its representable range.
    fn staged_db_arm(
        &self,
        identity: TickMapPoolIdentity,
        tick_spacing: i32,
        block: u64,
    ) -> Result<Option<StagedDbMap>, IngressDecline> {
        let mut memo = self.memo.lock();
        if memo.block != block {
            memo.block = block;
            memo.maps.clear();
        }
        if let Some(hit) = memo.maps.get(&identity) {
            let hit = hit.clone();
            drop(memo);
            self.emit_backfill_witness(identity, hit.as_ref());
            return Ok(hit);
        }
        let Some(arm) = self.db.as_ref() else {
            memo.maps.insert(identity, None);
            return Ok(None);
        };
        let (stamp, map) = match identity {
            TickMapPoolIdentity::V3(address) => (
                arm.db
                    .fetch_liquidity_update_block(address)
                    .map_err(|e| IngressDecline::Db(e.to_string()))?,
                arm.db
                    .fetch_liquidity_map(address)
                    .map_err(|e| IngressDecline::Db(e.to_string()))?,
            ),
            TickMapPoolIdentity::V4 { manager, pool_id } => (
                arm.db
                    .fetch_liquidity_update_block_v4(manager, pool_id)
                    .map_err(|e| IngressDecline::Db(e.to_string()))?,
                arm.db
                    .fetch_liquidity_map_v4(manager, pool_id)
                    .map_err(|e| IngressDecline::Db(e.to_string()))?,
            ),
        };
        let Some(map) = map else {
            memo.maps.insert(identity, None);
            return Ok(None);
        };
        let source_block = stamp.and_then(|source| u64::try_from(source).ok());
        let staged = match source_block {
            None => StagedDbMap {
                map,
                source_block,
                backfill: None,
            },
            Some(update_block) if update_block >= block => StagedDbMap {
                map,
                source_block,
                backfill: None,
            },
            Some(update_block) => {
                let from_block = update_block + 1;
                let events = match identity {
                    TickMapPoolIdentity::V3(address) => arm
                        .backfill_source
                        .fetch_v3_liquidity_events(address, from_block, block),
                    TickMapPoolIdentity::V4 { manager, pool_id } => arm
                        .backfill_source
                        .fetch_v4_liquidity_events(manager, pool_id, from_block, block),
                }
                .map_err(IngressDecline::Db)?;
                let backfilled = backfill_liquidity_map(map, tick_spacing, &events)?;
                StagedDbMap {
                    map: backfilled,
                    source_block,
                    backfill: Some(BackfillStamp {
                        from_block,
                        to_block: block,
                        events: events.len(),
                    }),
                }
            }
        };
        let for_witness = staged.clone();
        memo.maps.insert(identity, Some(staged));
        drop(memo);
        self.emit_backfill_witness(identity, Some(&for_witness));
        Ok(Some(for_witness))
    }

    /// Emit the per-admission backfill witness when `staged` carried a
    /// non-empty window (the window is what proves the map is current).
    fn emit_backfill_witness(&self, identity: TickMapPoolIdentity, staged: Option<&StagedDbMap>) {
        if let (Some(stamp), Some(witness)) = (
            staged.and_then(|staged| staged.backfill.as_ref()),
            self.witness.as_ref(),
        ) {
            witness.db_backfill(identity, stamp.from_block, stamp.to_block, stamp.events);
        }
    }
}

fn merge_replay_overlay(
    seed: &mut TickMapSeed,
    overlay: HashMap<i32, TickInfo>,
    tick_spacing: i32,
    family: &str,
) -> Result<(), IngressDecline> {
    for &tick in overlay.keys() {
        let compressed = tick.div_euclid(tick_spacing);
        let word = compressed >> 8;
        let bit = usize::try_from(compressed.rem_euclid(256)).map_err(|_| {
            IngressDecline::Db(format!("tick {tick} has an invalid {family} bitmap bit"))
        })?;
        seed.bitmaps
            .entry(word)
            .or_insert(U256::ZERO)
            .set_bit(bit, true);
    }
    seed.ticks.extend(overlay);
    Ok(())
}

/// Apply decoded liquidity events to a Db-staged map, sharing the updater's
/// exact per-tick accounting ([`apply_liquidity_mapping_update`]) so a
/// backfilled frame map equals the map the settlement pump would maintain.
///
/// Family-agnostic: V4 `ModifyLiquidity` events decode to the same
/// [`LiquidityUpdateEvent`], so the V4 ingress reuses this step unchanged.
fn backfill_liquidity_map(
    map: LiquidityMap,
    tick_spacing: i32,
    events: &[LiquidityUpdateEvent],
) -> Result<LiquidityMap, IngressDecline> {
    if events.is_empty() {
        return Ok(map);
    }
    let mut bitmap = db_bitmap_to_apply(map.tick_bitmap);
    let mut tick_data = db_ticks_to_apply(map.tick_data)?;
    for event in events {
        // Passing the event block as both `initial_state_block` and
        // `update_block` skips the in-range liquidity adjustment (the staged
        // seed's scalars are the caller's, read from slot0); only the tick map
        // matters here.
        let result = apply_liquidity_mapping_update(
            bitmap,
            tick_data,
            tick_spacing,
            0,
            U128::ZERO,
            event.block_number,
            event.block_number,
            event.tick_lower,
            event.tick_upper,
            event.liquidity_delta,
        );
        bitmap = result.tick_bitmap;
        tick_data = result.tick_data;
    }
    Ok(LiquidityMap {
        tick_bitmap: apply_bitmap_to_db(bitmap),
        tick_data: apply_ticks_to_db(tick_data)?,
    })
}

/// Widen the Db snapshot bitmap (word-keyed `i64`, no block stamp) into the
/// updater math's word-keyed `i32` form.
fn db_bitmap_to_apply(bitmap: HashMap<i64, DbBitmapAtWord>) -> HashMap<i32, ApplyBitmapAtWord> {
    bitmap
        .into_iter()
        .map(|(word, entry)| {
            // A tick-bitmap word is an int16 on-chain; the `i64` Db key always
            // fits `i32`.
            let word = i32::try_from(word).unwrap_or(i32::MAX);
            (
                word,
                ApplyBitmapAtWord {
                    bitmap: entry.bitmap,
                    block: 0,
                },
            )
        })
        .collect()
}

/// Widen the Db snapshot tick rows into the updater math's `I256`/`U128` form.
fn db_ticks_to_apply(
    ticks: HashMap<i32, DbLiquidityAtTick>,
) -> Result<HashMap<i32, ApplyLiquidityAtTick>, IngressDecline> {
    ticks
        .into_iter()
        .map(|(tick, entry)| {
            let net = I256::try_from(entry.liquidity_net)
                .map_err(|_| IngressDecline::Db(format!("tick {tick} net out of I256 range")))?;
            Ok((
                tick,
                ApplyLiquidityAtTick {
                    liquidity_net: net,
                    liquidity_gross: entry.liquidity_gross.to::<U128>(),
                    block: 0,
                },
            ))
        })
        .collect()
}

/// Narrow the updater math bitmap back into the Db snapshot shape.
fn apply_bitmap_to_db(bitmap: HashMap<i32, ApplyBitmapAtWord>) -> HashMap<i64, DbBitmapAtWord> {
    bitmap
        .into_iter()
        .map(|(word, entry)| {
            (
                i64::from(word),
                DbBitmapAtWord {
                    bitmap: entry.bitmap,
                },
            )
        })
        .collect()
}

/// Narrow the updater math tick rows back into the Db snapshot shape.
fn apply_ticks_to_db(
    ticks: HashMap<i32, ApplyLiquidityAtTick>,
) -> Result<HashMap<i32, DbLiquidityAtTick>, IngressDecline> {
    ticks
        .into_iter()
        .map(|(tick, entry)| {
            let net = i128::try_from(entry.liquidity_net)
                .map_err(|_| IngressDecline::Db(format!("tick {tick} net out of i128 range")))?;
            Ok((
                tick,
                DbLiquidityAtTick {
                    liquidity_gross: U256::from(entry.liquidity_gross.to::<u128>()),
                    liquidity_net: net,
                },
            ))
        })
        .collect()
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use alloy::primitives::{aliases::U128, B256};
    use degenbot_db::connection::DegenbotDb;
    use degenbot_db::error::DbError;
    use degenbot_db::snapshot::{BitmapAtWord, LiquidityAtTick};
    use degenbot_db::{ApplyBitmapAtWord, ApplyLiquidityAtTick, ExchangeFamily};
    use degenbot_pools::tick_fetch::{BootstrapTickError, BootstrapTickWord};
    use degenbot_pools::v3_state::PoolTickCoverage;

    const CHAIN: i64 = 1;
    const POOL: Address = Address::new([0x11; 20]);

    /// A `TickMapDb` fake with a per-call fetch counter (memo assertions).
    struct FakeDb {
        maps: HashMap<Address, LiquidityMap>,
        update_blocks: HashMap<Address, i64>,
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
            pool_address: Address,
        ) -> Result<Option<i64>, DbError> {
            Ok(self.update_blocks.get(&pool_address).copied())
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
            Ok(Some(BootstrapTickWord {
                bitmap: U256::from(1u8),
                word: 0,
                ticks,
            }))
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

    /// An empty backfill transport for tests that never stage a window.
    fn unused_source() -> Arc<FakeLogSource> {
        Arc::new(FakeLogSource {
            events: Vec::new(),
            calls: Arc::new(AtomicUsize::new(0)),
            ranges: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Wrap a Db handle with its (never-invoked) backfill transport.
    fn arm(db: Arc<dyn TickMapDb>) -> DbArm {
        DbArm::new(db, unused_source())
    }

    fn ingress_with(db: Option<Arc<dyn TickMapDb>>, fetches: &Arc<AtomicUsize>) -> PoolIngress {
        let _ = fetches;
        PoolIngress::new(db.map(arm), None)
    }

    #[test]
    fn db_arm_wins_and_stages_the_complete_map() {
        let fetches = Arc::new(AtomicUsize::new(0));
        let mut maps = HashMap::new();
        maps.insert(POOL, map_with_tick(120, 60));
        let db = Arc::new(FakeDb {
            maps,
            fetches: Arc::clone(&fetches),
            update_blocks: HashMap::new(),
        });
        let ingress = ingress_with(Some(db), &fetches);
        let seed = ingress.stage_v3_tick_map(POOL, 0, 60, 100).expect("stages");
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
            update_blocks: HashMap::new(),
        });
        let ingress = ingress_with(Some(db), &fetches);
        ingress.stage_v3_tick_map(POOL, 0, 60, 100).unwrap();
        ingress.stage_v3_tick_map(POOL, 0, 60, 100).unwrap();
        assert_eq!(
            fetches.load(Ordering::SeqCst),
            1,
            "the second read in the same block is memoized"
        );
        ingress.stage_v3_tick_map(POOL, 0, 60, 101).unwrap();
        assert_eq!(fetches.load(Ordering::SeqCst), 2, "a new block re-reads");
    }

    #[test]
    fn db_miss_without_chain_is_empty_sparse() {
        let fetches = Arc::new(AtomicUsize::new(0));
        let db = Arc::new(FakeDb {
            maps: HashMap::new(),
            fetches: Arc::clone(&fetches),
            update_blocks: HashMap::new(),
        });
        let ingress = ingress_with(Some(db), &fetches);
        let seed = ingress.stage_v3_tick_map(POOL, 0, 60, 100).expect("stages");
        assert_eq!(seed.source, TickMapSource::Chain);
        assert_eq!(seed.coverage, PoolTickCoverage::Sparse);
        assert!(seed.ticks.is_empty(), "no word is fabricated on a miss");
    }

    #[tokio::test]
    async fn sparse_chain_admission_is_bootstrap_only_and_never_sampled() {
        let fetches = Arc::new(AtomicUsize::new(0));
        let db = Arc::new(FakeDb {
            maps: HashMap::new(),
            fetches: Arc::clone(&fetches),
            update_blocks: HashMap::new(),
        });
        let chain = Arc::new(FakeChain {
            tick: 120,
            gross: 2_000,
            net: -700,
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let mut ingress = PoolIngress::new(Some(arm(db)), Some(chain));
        ingress.set_verify_level(VerifyLevel::Strict);
        ingress.set_verifier(Arc::new(FakeVerifier {
            calls: Arc::clone(&calls),
            fail: true,
        }));
        let mut ws = a_new_workspace();
        ingress
            .admit_v3_verified(&mut ws, params(POOL), 100)
            .await
            .expect("sparse pools admit without claiming full verification");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn chain_arm_stages_sparse_when_db_misses() {
        let fetches = Arc::new(AtomicUsize::new(0));
        let db = Arc::new(FakeDb {
            maps: HashMap::new(),
            fetches: Arc::clone(&fetches),
            update_blocks: HashMap::new(),
        });
        let chain = Arc::new(FakeChain {
            tick: 120,
            gross: 2_000,
            net: -700,
        });
        let ingress = PoolIngress::new(Some(arm(db)), Some(chain));
        let seed = ingress.stage_v3_tick_map(POOL, 0, 60, 100).expect("stages");
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
        async fn verify(
            &self,
            _target: TickMapSampleTarget,
            _map: &RpcLiquidityMap,
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

    /// An empty Db arm: an empty snapshot paired with its never-invoked
    /// backfill transport.
    fn empty_db() -> (DbArm, Arc<AtomicUsize>) {
        let fetches = Arc::new(AtomicUsize::new(0));
        let mut maps = HashMap::new();
        maps.insert(POOL, LiquidityMap::default());
        (
            DbArm::new(
                Arc::new(FakeDb {
                    maps,
                    fetches: Arc::clone(&fetches),
                    update_blocks: HashMap::new(),
                }),
                unused_source(),
            ),
            fetches,
        )
    }

    #[tokio::test]
    #[expect(clippy::items_after_statements, clippy::type_complexity)]
    async fn replay_overlay_is_merged_before_sample_verification() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(Vec::new()));
        struct RecordingVerifier {
            calls: Arc<AtomicUsize>,
            seen: Arc<Mutex<Vec<(HashMap<i32, TickInfo>, HashMap<i32, U256>, i32, u64)>>>,
        }
        #[async_trait::async_trait]
        impl TickMapSampleVerifier for RecordingVerifier {
            async fn verify(
                &self,
                _target: TickMapSampleTarget,
                map: &RpcLiquidityMap,
                block: u64,
            ) -> Result<(), String> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.seen.lock().push((
                    map.ticks.clone(),
                    map.bitmaps.clone(),
                    map.tick_spacing,
                    block,
                ));
                Ok(())
            }
        }
        let mut base_map = map_with_tick(120, 60);
        base_map
            .tick_bitmap
            .insert(7, BitmapAtWord { bitmap: U256::ZERO });
        let mut maps = HashMap::new();
        maps.insert(POOL, base_map);
        let db = Arc::new(FakeDb {
            maps,
            fetches: Arc::new(AtomicUsize::new(0)),
            update_blocks: HashMap::new(),
        });
        let mut ingress = PoolIngress::new(Some(arm(db)), None);
        ingress.set_verify_level(VerifyLevel::Strict);
        ingress.set_verifier(Arc::new(RecordingVerifier {
            calls: Arc::clone(&calls),
            seen: Arc::clone(&seen),
        }));
        let mut ws = Workspace::new();
        let mut overlay = HashMap::new();
        overlay.insert(
            -60,
            TickInfo {
                liquidity_gross: U128::from(700u64),
                liquidity_net: 500,
                block: 100,
            },
        );
        overlay.insert(
            180,
            TickInfo {
                liquidity_gross: U128::from(2_000u64),
                liquidity_net: -1_000,
                block: 100,
            },
        );
        ingress
            .admit_v3_replay(&mut ws, params(POOL), overlay, 100)
            .await
            .expect("replay admission crosses the verifier");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let (ticks, bitmaps, spacing, block) = seen.lock().as_slice()[0].clone();
        assert_eq!(bitmaps.get(&0), Some(&U256::from(12u8)));
        assert_eq!(
            bitmaps.get(&-1),
            Some(&(U256::from(1u8) << 255usize)),
            "negative compressed ticks use the signed word and non-negative bit"
        );
        assert_eq!(
            bitmaps.get(&7),
            Some(&U256::ZERO),
            "overlay projection preserves checked-empty words"
        );
        assert_eq!(spacing, 60);
        assert_eq!(block, 100);
        assert!(ticks.contains_key(&120));
        assert_eq!(ticks[&-60].liquidity_gross, U128::from(700u64));
        assert_eq!(ticks[&180].liquidity_gross, U128::from(2_000u64));
    }

    #[tokio::test]
    async fn replay_anchor_off_skips_sampling_but_still_registers() {
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
            .admit_v3_replay(&mut ws, params(POOL), HashMap::new(), 100)
            .await
            .expect("Off admits the replay anchor without sampling");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn replay_anchor_bootstrap_resamples_when_freshness_advances() {
        let (db, _) = empty_db();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut ingress = PoolIngress::new(Some(db), None);
        ingress.set_verify_level(VerifyLevel::Bootstrap);
        ingress.set_verifier(Arc::new(FakeVerifier {
            calls: Arc::clone(&calls),
            fail: false,
        }));
        ingress
            .admit_v3_replay(&mut a_new_workspace(), params(POOL), HashMap::new(), 100)
            .await
            .expect("first replay anchor admits");
        ingress
            .admit_v3_replay(&mut a_new_workspace(), params(POOL), HashMap::new(), 101)
            .await
            .expect("later replay anchor admits");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
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
            2,
            "a later freshness block is sampled again"
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
            update_blocks: HashMap::new(),
        });
        let mut ingress = PoolIngress::new(Some(arm(db)), None);
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

    /// A backfill source fake: records requested ranges and returns a fixed
    /// event list.
    struct FakeLogSource {
        events: Vec<LiquidityUpdateEvent>,
        calls: Arc<AtomicUsize>,
        ranges: Arc<Mutex<Vec<(Address, u64, u64)>>>,
    }

    impl LiquidityLogSource for FakeLogSource {
        fn fetch_v3_liquidity_events(
            &self,
            pool_address: Address,
            from_block: u64,
            to_block: u64,
        ) -> Result<Vec<LiquidityUpdateEvent>, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.ranges
                .lock()
                .push((pool_address, from_block, to_block));
            Ok(self.events.clone())
        }

        fn fetch_v4_liquidity_events(
            &self,
            _manager: Address,
            _pool_id: B256,
            _from_block: u64,
            _to_block: u64,
        ) -> Result<Vec<LiquidityUpdateEvent>, String> {
            Err("unexpected V4 fetch".into())
        }
    }

    /// A witness fake recording every backfill event.
    #[derive(Default)]
    struct FakeWitness {
        backfills: Mutex<Vec<(TickMapPoolIdentity, u64, u64, usize)>>,
    }

    impl IngressWitness for FakeWitness {
        fn db_backfill(
            &self,
            identity: TickMapPoolIdentity,
            from_block: u64,
            to_block: u64,
            events: usize,
        ) {
            self.backfills
                .lock()
                .push((identity, from_block, to_block, events));
        }
    }

    /// A Db map contributed at `update_block`.
    fn staged_db_with_window(update_block: i64) -> (Arc<FakeDb>, Arc<AtomicUsize>) {
        let fetches = Arc::new(AtomicUsize::new(0));
        let mut maps = HashMap::new();
        maps.insert(POOL, map_with_tick(120, 60));
        let mut update_blocks = HashMap::new();
        update_blocks.insert(POOL, update_block);
        let db = Arc::new(FakeDb {
            maps,
            fetches: Arc::clone(&fetches),
            update_blocks,
        });
        (db, fetches)
    }

    fn mint_source(calls: &Arc<AtomicUsize>) -> Arc<FakeLogSource> {
        Arc::new(FakeLogSource {
            events: vec![LiquidityUpdateEvent {
                block_number: 101,
                log_index: 0,
                tick_lower: 120,
                tick_upper: 180,
                liquidity_delta: I256::try_from(1_000u64).unwrap(),
            }],
            calls: Arc::clone(calls),
            ranges: Arc::new(Mutex::new(Vec::new())),
        })
    }

    #[test]
    fn window_backfill_advances_the_staged_map_to_head() {
        let (db, _) = staged_db_with_window(100);
        let source_calls = Arc::new(AtomicUsize::new(0));
        let witness = Arc::new(FakeWitness::default());
        let mut ingress = PoolIngress::new(Some(DbArm::new(db, mint_source(&source_calls))), None);
        ingress.set_witness(Arc::clone(&witness) as Arc<dyn IngressWitness>);

        let seed = ingress.stage_v3_tick_map(POOL, 0, 60, 102).expect("stages");
        assert_eq!(seed.source, TickMapSource::Db);
        assert_eq!(seed.coverage, PoolTickCoverage::Tracked);
        assert_eq!(seed.seed_block, 102, "the seed is stamped at head");
        let lower = seed.ticks.get(&120).expect("lower tick");
        assert_eq!(lower.liquidity_gross, U128::from(2_000u64));
        assert_eq!(lower.liquidity_net, 1_500);
        let upper = seed.ticks.get(&180).expect("upper tick");
        assert_eq!(upper.liquidity_gross, U128::from(1_000u64));
        assert_eq!(upper.liquidity_net, -1_000);

        assert_eq!(
            source_calls.load(Ordering::SeqCst),
            1,
            "one fetch per block"
        );
        assert_eq!(
            witness.backfills.lock().as_slice(),
            &[(TickMapPoolIdentity::V3(POOL), 101, 102, 1)],
            "the window is witnessed"
        );

        // The second admission in the same block reuses the memoized map but
        // re-witnesses the window (per-admission, not per-block).
        let _ = ingress.stage_v3_tick_map(POOL, 0, 60, 102).expect("stages");
        assert_eq!(source_calls.load(Ordering::SeqCst), 1, "memoized");
        assert_eq!(
            witness.backfills.lock().len(),
            2,
            "one witness per admission"
        );
    }

    #[test]
    fn head_fresh_db_stages_without_a_backfill_fetch() {
        let (db, _) = staged_db_with_window(100);
        let source_calls = Arc::new(AtomicUsize::new(0));
        let witness = Arc::new(FakeWitness::default());
        let mut ingress = PoolIngress::new(Some(DbArm::new(db, mint_source(&source_calls))), None);
        ingress.set_witness(Arc::clone(&witness) as Arc<dyn IngressWitness>);

        let seed = ingress.stage_v3_tick_map(POOL, 0, 60, 100).expect("stages");
        assert_eq!(seed.source, TickMapSource::Db);
        assert_eq!(seed.seed_block, 100);
        assert_eq!(
            source_calls.load(Ordering::SeqCst),
            0,
            "window 0 never fetches logs"
        );
        assert!(witness.backfills.lock().is_empty());
    }

    /// There is no window cap: a Db map whose ledger sits far behind head is
    /// still advanced to head by its decoded events rather than deferred.
    #[test]
    fn a_window_far_past_the_old_cap_still_backfills_to_head() {
        let (db, _) = staged_db_with_window(1);
        let source_calls = Arc::new(AtomicUsize::new(0));
        let witness = Arc::new(FakeWitness::default());
        let mut ingress = PoolIngress::new(Some(DbArm::new(db, mint_source(&source_calls))), None);
        ingress.set_witness(Arc::clone(&witness) as Arc<dyn IngressWitness>);

        let seed = ingress
            .stage_v3_tick_map(POOL, 0, 60, 100_000)
            .expect("stages");
        assert_eq!(seed.source, TickMapSource::Db, "no cap defers to Chain");
        assert_eq!(seed.seed_block, 100_000, "the seed is stamped at head");
        assert_eq!(
            source_calls.load(Ordering::SeqCst),
            1,
            "a ~100k-block window is fetched, not deferred"
        );
        assert_eq!(
            witness.backfills.lock().as_slice(),
            &[(TickMapPoolIdentity::V3(POOL), 2, 100_000, 1)],
            "the whole window is witnessed"
        );
    }

    struct FakeV4Db {
        map: Option<LiquidityMap>,
        update_block: i64,
    }

    impl TickMapDb for FakeV4Db {
        fn fetch_liquidity_map(
            &self,
            _pool_address: Address,
        ) -> Result<Option<LiquidityMap>, DbError> {
            Ok(None)
        }

        fn fetch_liquidity_map_v4(
            &self,
            pool_manager: Address,
            pool_id: B256,
        ) -> Result<Option<LiquidityMap>, DbError> {
            if pool_manager != V4_MANAGER || pool_id != V4_POOL_ID {
                return Err(DbError::Decode("unexpected V4 Db identity".into()));
            }
            Ok(self.map.clone())
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
            _pool_id: B256,
        ) -> Result<Option<i64>, DbError> {
            Ok(Some(self.update_block))
        }
    }

    type V4SourceCalls = Arc<Mutex<Vec<(Address, B256, u64, u64)>>>;

    struct FakeV4Source {
        calls: V4SourceCalls,
    }

    impl LiquidityLogSource for FakeV4Source {
        fn fetch_v3_liquidity_events(
            &self,
            _pool: Address,
            _from: u64,
            _to: u64,
        ) -> Result<Vec<LiquidityUpdateEvent>, String> {
            Err("unexpected V3 fetch".into())
        }

        fn fetch_v4_liquidity_events(
            &self,
            manager: Address,
            pool_id: B256,
            from: u64,
            to: u64,
        ) -> Result<Vec<LiquidityUpdateEvent>, String> {
            self.calls.lock().push((manager, pool_id, from, to));
            Ok(vec![
                LiquidityUpdateEvent {
                    block_number: 201,
                    log_index: 0,
                    tick_lower: -10,
                    tick_upper: 10,
                    liquidity_delta: I256::try_from(500_i64).unwrap(),
                },
                LiquidityUpdateEvent {
                    block_number: 202,
                    log_index: 0,
                    tick_lower: -10,
                    tick_upper: 10,
                    liquidity_delta: -I256::try_from(200_i64).unwrap(),
                },
            ])
        }
    }

    struct FailingV4Source;

    impl LiquidityLogSource for FailingV4Source {
        fn fetch_v3_liquidity_events(
            &self,
            _pool: Address,
            _from: u64,
            _to: u64,
        ) -> Result<Vec<LiquidityUpdateEvent>, String> {
            Err("unexpected V3 fetch".into())
        }

        fn fetch_v4_liquidity_events(
            &self,
            _manager: Address,
            _pool_id: B256,
            _from: u64,
            _to: u64,
        ) -> Result<Vec<LiquidityUpdateEvent>, String> {
            Err("archive node unavailable".into())
        }
    }

    const V4_MANAGER: Address = Address::new([0x44; 20]);
    const V4_POOL_ID: B256 = B256::new([0x55; 32]);

    fn v4_params() -> IngressV4Params {
        IngressV4Params {
            manager: V4_MANAGER,
            state_view: Some(Address::new([0x77; 20])),
            pool_id: V4_POOL_ID,
            token0: Address::new([0x02; 20]),
            token1: Address::new([0x03; 20]),
            fee: 500,
            tick_spacing: 10,
            hooks: Address::ZERO,
            sqrt_price_x96: U256::from(1_u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
        }
    }

    fn tracked_v4_map() -> LiquidityMap {
        let mut bitmap = U256::ZERO;
        bitmap.set_bit(12, true);
        LiquidityMap {
            tick_bitmap: HashMap::from([
                (0, BitmapAtWord { bitmap }),
                (7, BitmapAtWord { bitmap: U256::ZERO }),
            ]),
            tick_data: HashMap::from([(
                120,
                LiquidityAtTick {
                    liquidity_gross: U256::from(1_000_u64),
                    liquidity_net: 1_000,
                },
            )]),
        }
    }

    #[tokio::test]
    async fn v4_backfill_and_verifier_failures_are_typed_refusals() {
        let base_bitmap = U256::from(1_u8) << 255;
        let stale_db = || {
            Arc::new(FakeV4Db {
                map: Some(LiquidityMap {
                    tick_bitmap: HashMap::from([(
                        -1,
                        BitmapAtWord {
                            bitmap: base_bitmap,
                        },
                    )]),
                    tick_data: HashMap::from([(
                        -10,
                        LiquidityAtTick {
                            liquidity_gross: U256::from(1_000_u64),
                            liquidity_net: 1_000,
                        },
                    )]),
                }),
                update_block: 200,
            })
        };
        let mut backfill = PoolIngress::new(
            Some(DbArm::new(stale_db(), Arc::new(FailingV4Source))),
            None,
        );
        backfill.set_verify_level(VerifyLevel::Off);
        let err = backfill
            .admit_v4_verified(&mut Workspace::new(), v4_params(), 202)
            .await
            .expect_err("backfill failure is loud");
        assert!(matches!(err, IngressDecline::Db(detail) if detail.contains("archive node")));

        let mut verifier = PoolIngress::new(
            Some(DbArm::new(
                Arc::new(FakeV4Db {
                    map: Some(tracked_v4_map()),
                    update_block: 202,
                }),
                Arc::new(FakeV4Source {
                    calls: Arc::new(Mutex::new(Vec::new())),
                }),
            )),
            None,
        );
        verifier.set_verify_level(VerifyLevel::Strict);
        verifier.set_verifier(Arc::new(FakeVerifier {
            calls: Arc::new(AtomicUsize::new(0)),
            fail: true,
        }));
        let err = verifier
            .admit_v4_verified(&mut Workspace::new(), v4_params(), 202)
            .await
            .expect_err("verification failure is loud");
        assert!(matches!(err, IngressDecline::Verify(detail) if detail.contains("mismatch")));
    }

    #[tokio::test]
    async fn v4_replay_overlay_updates_real_spacing_bitmap_before_pool_manager_verification() {
        struct RecordingVerifier {
            seen: Arc<Mutex<Option<(TickMapSampleTarget, RpcLiquidityMap)>>>,
        }
        #[async_trait::async_trait]
        impl TickMapSampleVerifier for RecordingVerifier {
            async fn verify(
                &self,
                target: TickMapSampleTarget,
                map: &RpcLiquidityMap,
                _block: u64,
            ) -> Result<(), String> {
                *self.seen.lock() = Some((target, map.clone()));
                Ok(())
            }
        }

        let db = Arc::new(FakeV4Db {
            map: Some(tracked_v4_map()),
            update_block: 202,
        });
        let seen = Arc::new(Mutex::new(None));
        let mut ingress = PoolIngress::new(
            Some(DbArm::new(
                db,
                Arc::new(FakeV4Source {
                    calls: Arc::new(Mutex::new(Vec::new())),
                }),
            )),
            None,
        );
        ingress.set_verify_level(VerifyLevel::Strict);
        ingress.set_verifier(Arc::new(RecordingVerifier {
            seen: Arc::clone(&seen),
        }));
        let mut overlay = HashMap::new();
        overlay.insert(
            -10,
            TickInfo {
                liquidity_gross: U128::from(500_u64),
                liquidity_net: 500,
                block: 202,
            },
        );
        overlay.insert(
            130,
            TickInfo {
                liquidity_gross: U128::from(700_u64),
                liquidity_net: -700,
                block: 202,
            },
        );
        ingress
            .admit_v4_replay(&mut Workspace::new(), v4_params(), overlay, 202)
            .await
            .expect("V4 replay crosses the shared sample seam");

        let (target, map) = seen.lock().clone().expect("verifier observed final map");
        assert_eq!(
            target,
            TickMapSampleTarget::V4 {
                manager: V4_MANAGER,
                pool_id: V4_POOL_ID,
            }
        );
        assert_eq!(map.tick_spacing, 10);
        assert_eq!(
            map.bitmaps[&0],
            (U256::from(1_u8) << 12) | (U256::from(1_u8) << 13)
        );
        assert_eq!(map.bitmaps[&-1], U256::from(1_u8) << 255);
        assert_eq!(map.bitmaps[&7], U256::ZERO);
        assert!(map.ticks.contains_key(&-10));
        assert!(map.ticks.contains_key(&130));
    }

    #[tokio::test]
    async fn v4_strict_bootstrap_off_and_sparse_follow_the_shared_policy() {
        let strict_calls = Arc::new(AtomicUsize::new(0));
        let mut strict = PoolIngress::new(
            Some(DbArm::new(
                Arc::new(FakeV4Db {
                    map: Some(tracked_v4_map()),
                    update_block: 202,
                }),
                Arc::new(FakeV4Source {
                    calls: Arc::new(Mutex::new(Vec::new())),
                }),
            )),
            None,
        );
        strict.set_verify_level(VerifyLevel::Strict);
        strict.set_verifier(Arc::new(FakeVerifier {
            calls: Arc::clone(&strict_calls),
            fail: false,
        }));
        strict
            .admit_v4_verified(&mut Workspace::new(), v4_params(), 202)
            .await
            .unwrap();
        strict
            .admit_v4_verified(&mut Workspace::new(), v4_params(), 202)
            .await
            .unwrap();
        assert_eq!(strict_calls.load(Ordering::SeqCst), 2);

        let bootstrap_calls = Arc::new(AtomicUsize::new(0));
        let mut bootstrap = PoolIngress::new(
            Some(DbArm::new(
                Arc::new(FakeV4Db {
                    map: Some(tracked_v4_map()),
                    update_block: 202,
                }),
                Arc::new(FakeV4Source {
                    calls: Arc::new(Mutex::new(Vec::new())),
                }),
            )),
            None,
        );
        bootstrap.set_verify_level(VerifyLevel::Bootstrap);
        bootstrap.set_verifier(Arc::new(FakeVerifier {
            calls: Arc::clone(&bootstrap_calls),
            fail: false,
        }));
        bootstrap
            .admit_v4_verified(&mut Workspace::new(), v4_params(), 202)
            .await
            .unwrap();
        bootstrap
            .admit_v4_verified(&mut Workspace::new(), v4_params(), 202)
            .await
            .unwrap();
        assert_eq!(bootstrap_calls.load(Ordering::SeqCst), 1);

        let off_calls = Arc::new(AtomicUsize::new(0));
        let mut off = PoolIngress::new(
            Some(DbArm::new(
                Arc::new(FakeV4Db {
                    map: Some(tracked_v4_map()),
                    update_block: 202,
                }),
                Arc::new(FakeV4Source {
                    calls: Arc::new(Mutex::new(Vec::new())),
                }),
            )),
            None,
        );
        off.set_verify_level(VerifyLevel::Off);
        off.set_verifier(Arc::new(FakeVerifier {
            calls: Arc::clone(&off_calls),
            fail: true,
        }));
        off.admit_v4_verified(&mut Workspace::new(), v4_params(), 202)
            .await
            .unwrap();
        assert_eq!(off_calls.load(Ordering::SeqCst), 0);

        let sparse_calls = Arc::new(AtomicUsize::new(0));
        let mut sparse = PoolIngress::new(
            Some(DbArm::new(
                Arc::new(FakeV4Db {
                    map: None,
                    update_block: 202,
                }),
                Arc::new(FakeV4Source {
                    calls: Arc::new(Mutex::new(Vec::new())),
                }),
            )),
            None,
        );
        sparse.set_verify_level(VerifyLevel::Strict);
        sparse.set_verifier(Arc::new(FakeVerifier {
            calls: Arc::clone(&sparse_calls),
            fail: true,
        }));
        sparse
            .admit_v4_verified(&mut Workspace::new(), v4_params(), 202)
            .await
            .unwrap();
        assert_eq!(sparse_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn v4_db_to_head_map_equals_existing_db_updater_compute_result() {
        let (db, _) = DegenbotDb::open_in_memory_for_writes().unwrap();
        {
            let conn = db.lock();
            conn.execute_batch(&format!(
                "PRAGMA foreign_keys=OFF;
                 INSERT INTO exchanges (id, chain_id, name, active, factory) VALUES
                    (1, 1, 'uniswap_v4', 1, '{V4_MANAGER}');
                 INSERT INTO pool_managers
                    (id, address, chain, kind, state_view, exchange_id)
                 VALUES (1, '{V4_MANAGER}', 1, 'uniswap_v4', NULL, 1);
                 INSERT INTO managed_pools (id, kind, manager_id)
                 VALUES (902, 'uniswap_v4', 1);
                 INSERT INTO uniswap_v4_pools
                    (managed_pool_id, pool_hash, hooks, currency0_id, currency1_id,
                     fee_currency0, fee_currency1, fee_denominator, tick_spacing,
                     liquidity_update_block)
                 VALUES (902, '{V4_POOL_ID}', '{}', 1, 2, 500, 500, 1000000, 10, 200);",
                Address::ZERO.to_checksum(None),
            ))
            .unwrap();
        }
        let mut ticks = HashMap::new();
        ticks.insert(
            -10,
            ApplyLiquidityAtTick {
                liquidity_net: I256::try_from(1_000_i64).unwrap(),
                liquidity_gross: U128::from(1_000_u64),
                block: 0,
            },
        );
        let mut bitmaps = HashMap::new();
        bitmaps.insert(
            -1,
            ApplyBitmapAtWord {
                bitmap: U256::from(1_u8) << 255,
                block: 0,
            },
        );
        db.upsert_v4_liquidity_positions(902, &ticks).unwrap();
        db.upsert_v4_initialization_maps(902, &bitmaps).unwrap();
        let events = vec![
            LiquidityUpdateEvent {
                block_number: 201,
                log_index: 0,
                tick_lower: -10,
                tick_upper: 10,
                liquidity_delta: I256::try_from(500_i64).unwrap(),
            },
            LiquidityUpdateEvent {
                block_number: 202,
                log_index: 0,
                tick_lower: -10,
                tick_upper: 10,
                liquidity_delta: -I256::try_from(200_i64).unwrap(),
            },
        ];
        let conn = db.lock();
        let expected = DegenbotDb::compute_v4_liquidity_update_on_conn(
            &conn,
            &V4_POOL_ID.to_string(),
            1,
            &events,
        )
        .unwrap()
        .expect("seeded managed pool computes");
        drop(conn);

        let db = Arc::new(db);
        let ingress = PoolIngress::new(
            Some(DbArm::new(
                db,
                Arc::new(FakeV4Source {
                    calls: Arc::new(Mutex::new(Vec::new())),
                }),
            )),
            None,
        );
        let actual = ingress
            .stage_v4_tick_map(V4_MANAGER, V4_POOL_ID, 0, 10, 202)
            .expect("stages through PoolIngress");

        assert_eq!(actual.ticks.len(), expected.tick_data.len());
        for (tick, expected_tick) in expected.tick_data {
            let actual_tick = actual.ticks.get(&tick).expect("tick row");
            assert_eq!(actual_tick.liquidity_gross, expected_tick.liquidity_gross);
            assert_eq!(
                actual_tick.liquidity_net,
                i128::try_from(expected_tick.liquidity_net).unwrap()
            );
        }
        assert_eq!(actual.bitmaps.len(), expected.tick_bitmap.len());
        for (word, expected_word) in expected.tick_bitmap {
            assert_eq!(actual.bitmaps[&word], expected_word.bitmap);
        }
    }

    #[test]
    fn v4_db_to_head_seed_matches_existing_updater_math_with_full_provenance() {
        let mut base_bitmap = U256::ZERO;
        base_bitmap.set_bit(255, true);
        let db = Arc::new(FakeV4Db {
            map: Some(LiquidityMap {
                tick_bitmap: HashMap::from([(
                    -1,
                    BitmapAtWord {
                        bitmap: base_bitmap,
                    },
                )]),
                tick_data: HashMap::from([(
                    -10,
                    LiquidityAtTick {
                        liquidity_gross: U256::from(1_000_u64),
                        liquidity_net: 1_000,
                    },
                )]),
            }),
            update_block: 200,
        });
        let calls = Arc::new(Mutex::new(Vec::new()));
        let ingress = PoolIngress::new(
            Some(DbArm::new(
                db,
                Arc::new(FakeV4Source {
                    calls: Arc::clone(&calls),
                }),
            )),
            None,
        );

        let seed = ingress
            .stage_v4_tick_map(V4_MANAGER, V4_POOL_ID, 0, 10, 202)
            .expect("V4 Db map advances to head");

        assert_eq!(seed.source, TickMapSource::Db);
        assert_eq!(seed.coverage, PoolTickCoverage::Tracked);
        assert_eq!(
            seed.identity,
            TickMapPoolIdentity::V4 {
                manager: V4_MANAGER,
                pool_id: V4_POOL_ID,
            }
        );
        assert_eq!(seed.source_block, Some(200));
        assert_eq!(seed.seed_block, 202);
        assert_eq!(seed.ticks[&-10].liquidity_gross, U128::from(1_300_u64));
        assert_eq!(seed.ticks[&-10].liquidity_net, 1_300);
        assert_eq!(seed.ticks[&10].liquidity_gross, U128::from(300_u64));
        assert_eq!(seed.ticks[&10].liquidity_net, -300);
        assert_eq!(seed.bitmaps[&0], U256::from(2_u8));
        assert_eq!(seed.bitmaps[&-1], U256::from(1_u8) << 255usize);
        assert_eq!(
            calls.lock().as_slice(),
            &[(V4_MANAGER, V4_POOL_ID, 201, 202)]
        );
    }

    fn a_new_workspace() -> Workspace {
        Workspace::new()
    }
}
