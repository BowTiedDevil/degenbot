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

use alloy::primitives::{Address, I256, U128, U256};
use degenbot_db::snapshot::{
    BitmapAtWord as DbBitmapAtWord, LiquidityAtTick as DbLiquidityAtTick, LiquidityMap, TickMapDb,
};
use degenbot_db::LiquidityUpdateEvent;
use degenbot_math::cl::liquidity_mapping::{
    apply_liquidity_mapping_update, BitmapAtWord as ApplyBitmapAtWord,
    LiquidityAtTick as ApplyLiquidityAtTick,
};
pub use degenbot_pool_updater::fetch::{AlloyV3LiquidityLogSource, V3LiquidityLogSource};
use degenbot_pools::tick_fetch::TickBootstrapRpc;
use degenbot_pools::v3_state::ClSlotLayout;
use degenbot_pools::TickInfo;
use degenbot_rpc::provider::AlloyProvider;
use hashbrown::HashMap;
use parking_lot::Mutex;

use crate::bot_core::liquidity_verifier::verify_v3_liquidity_map;
use crate::bot_core::planning::{ExplicitPoolState, PlanningPoolParams, Workspace};
use crate::bot_core::tick_assembly::{chain_arm, resolve_tick_map_arm, TickMapAssemblyError};

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

/// The ingress's backfill witness sink: the strategy layer's per-hop JSONL
/// capture, behind a trait so the ingress stays free of any capture
/// implementation.
pub trait IngressWitness: Send + Sync {
    /// A Db-staged map was advanced from `from_block` to `to_block` by
    /// `events` decoded liquidity events.
    fn db_backfill(&self, pool_address: Address, from_block: u64, to_block: u64, events: usize);
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
    backfill_source: Arc<dyn V3LiquidityLogSource>,
}

impl DbArm {
    /// Pair a Db snapshot handle with the transport that backfills its lag.
    #[must_use]
    pub fn new(db: Arc<dyn TickMapDb>, backfill_source: Arc<dyn V3LiquidityLogSource>) -> Self {
        Self {
            db,
            backfill_source,
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
        tick_spacing: i32,
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
        tick_spacing: i32,
        block: u64,
    ) -> Result<(), String> {
        verify_v3_liquidity_map(
            self.provider.as_ref(),
            address,
            ticks,
            tick_spacing,
            block,
            "ingress",
        )
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

/// A Db-staged map plus the backfill window that brought it to head.
#[derive(Clone)]
struct StagedDbMap {
    map: LiquidityMap,
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
    maps: HashMap<Address, Option<StagedDbMap>>,
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
    /// Pools whose staged map has already been sample-verified. Process-scoped
    /// by design: the per-frame workspace is re-sized every frame, but the
    /// verification result for a pool is a process-lifetime fact.
    verified: Mutex<HashSet<Address>>,
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
            verified: Mutex::new(HashSet::new()),
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
    pub fn v3_tick_map(
        &self,
        address: Address,
        tick: i32,
        tick_spacing: i32,
        block: u64,
    ) -> Result<TickMapSeed, IngressDecline> {
        // The Db arm is staged + backfilled to head under the per-block memo;
        // a Db miss or a corrupt map returns `None`, so the shared precedence
        // falls to the Chain arm below.
        let staged = self.staged_db_arm(address, tick_spacing, block)?;
        let db_arm = resolve_tick_map_arm::<IngressDecline, _>(
            &format!("{address}"),
            tick_spacing,
            block,
            || Ok(staged.map(|s| s.map)),
        )?;
        if let Some(seed) = db_arm {
            return Ok(seed);
        }
        // Chain arm: the Db missed (or no Db handle is wired).
        let chain_hit = match self.chain.as_deref() {
            Some(chain) => chain
                .bootstrap_v3_tick_word(&address.to_checksum(None), tick, tick_spacing, block)
                .map_err(TickMapAssemblyError::Chain)?
                .map(|word| word.ticks),
            None => None,
        };
        Ok(chain_arm(chain_hit).into_seed(block))
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
        self.verify_staged_v3(params.address, params.tick_spacing, &seed)
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
    async fn verify_staged_v3(
        &self,
        address: Address,
        tick_spacing: i32,
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
            .verify_v3(address, &seed.ticks, tick_spacing, seed.seed_block)
            .await
            .map_err(IngressDecline::Verify)?;
        self.verified.lock().insert(address);
        Ok(())
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
        address: Address,
        tick_spacing: i32,
        block: u64,
    ) -> Result<Option<StagedDbMap>, IngressDecline> {
        let mut memo = self.memo.lock();
        if memo.block != block {
            memo.block = block;
            memo.maps.clear();
        }
        if let Some(hit) = memo.maps.get(&address) {
            let hit = hit.clone();
            drop(memo);
            self.emit_backfill_witness(address, hit.as_ref());
            return Ok(hit);
        }
        let Some(arm) = self.db.as_ref() else {
            memo.maps.insert(address, None);
            return Ok(None);
        };
        // A missing stamp leaves the map's currency unknown: stage it verbatim
        // rather than inventing a zero window.
        let stamp = arm
            .db
            .fetch_liquidity_update_block(address)
            .map_err(|e| IngressDecline::Db(e.to_string()))?;
        let Some(map) = arm
            .db
            .fetch_liquidity_map(address)
            .map_err(|e| IngressDecline::Db(e.to_string()))?
        else {
            memo.maps.insert(address, None);
            return Ok(None);
        };
        let staged = match stamp.and_then(|s| u64::try_from(s).ok()) {
            None => StagedDbMap {
                map,
                backfill: None,
            },
            // Already at (or ahead of) head: nothing to close.
            Some(update_block) if update_block >= block => StagedDbMap {
                map,
                backfill: None,
            },
            Some(update_block) => {
                let from_block = update_block + 1;
                let events = arm
                    .backfill_source
                    .fetch_v3_liquidity_events(address, from_block, block)
                    .map_err(IngressDecline::Db)?;
                let backfilled = backfill_liquidity_map(map, tick_spacing, &events)?;
                StagedDbMap {
                    map: backfilled,
                    backfill: Some(BackfillStamp {
                        from_block,
                        to_block: block,
                        events: events.len(),
                    }),
                }
            }
        };
        let for_witness = staged.clone();
        memo.maps.insert(address, Some(staged));
        drop(memo);
        self.emit_backfill_witness(address, Some(&for_witness));
        Ok(Some(for_witness))
    }

    /// Emit the per-admission backfill witness when `staged` carried a
    /// non-empty window (the window is what proves the map is current).
    fn emit_backfill_witness(&self, address: Address, staged: Option<&StagedDbMap>) {
        if let (Some(stamp), Some(witness)) = (
            staged.and_then(|s| s.backfill.as_ref()),
            self.witness.as_ref(),
        ) {
            witness.db_backfill(address, stamp.from_block, stamp.to_block, stamp.events);
        }
    }
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
    use degenbot_db::error::DbError;
    use degenbot_db::snapshot::{BitmapAtWord, LiquidityAtTick};
    use degenbot_db::ExchangeFamily;
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
            update_blocks: HashMap::new(),
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
            update_blocks: HashMap::new(),
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
            update_blocks: HashMap::new(),
        });
        let chain = Arc::new(FakeChain {
            tick: 120,
            gross: 2_000,
            net: -700,
        });
        let ingress = PoolIngress::new(Some(arm(db)), Some(chain));
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
            _tick_spacing: i32,
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
        (
            DbArm::new(
                Arc::new(FakeDb {
                    maps: HashMap::new(),
                    fetches: Arc::clone(&fetches),
                    update_blocks: HashMap::new(),
                }),
                unused_source(),
            ),
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

    impl V3LiquidityLogSource for FakeLogSource {
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
    }

    /// A witness fake recording every backfill event.
    #[derive(Default)]
    struct FakeWitness {
        backfills: Mutex<Vec<(Address, u64, u64, usize)>>,
    }

    impl IngressWitness for FakeWitness {
        fn db_backfill(
            &self,
            pool_address: Address,
            from_block: u64,
            to_block: u64,
            events: usize,
        ) {
            self.backfills
                .lock()
                .push((pool_address, from_block, to_block, events));
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

        let seed = ingress.v3_tick_map(POOL, 0, 60, 102).expect("stages");
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
            &[(POOL, 101, 102, 1)],
            "the window is witnessed"
        );

        // The second admission in the same block reuses the memoized map but
        // re-witnesses the window (per-admission, not per-block).
        let _ = ingress.v3_tick_map(POOL, 0, 60, 102).expect("stages");
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

        let seed = ingress.v3_tick_map(POOL, 0, 60, 100).expect("stages");
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

        let seed = ingress.v3_tick_map(POOL, 0, 60, 100_000).expect("stages");
        assert_eq!(seed.source, TickMapSource::Db, "no cap defers to Chain");
        assert_eq!(seed.seed_block, 100_000, "the seed is stamped at head");
        assert_eq!(
            source_calls.load(Ordering::SeqCst),
            1,
            "a ~100k-block window is fetched, not deferred"
        );
        assert_eq!(
            witness.backfills.lock().as_slice(),
            &[(POOL, 2, 100_000, 1)],
            "the whole window is witnessed"
        );
    }

    fn a_new_workspace() -> Workspace {
        Workspace::new()
    }
}
