//! The evm2 port of the engine's cross-block warm cache
//! (`sim::evm::warm_code_cache`).
//!
//! Same staleness line and TTL semantics as the revm-side original: only the
//! immutable rows are cached — contract `AccountInfo` (existence + code hash,
//! EOA rows never: their nonce/balance mutate every block) and `Bytecode`
//! keyed by code hash — while storage and block hashes forward untouched.
//!
//! One structural difference is forced by evm2's DB seam: the original
//! implements revm's shared `DatabaseRef` (revm's `CacheDB` wraps a shared
//! ref); evm2's `CacheDB` composes over the mutable `Database` trait
//! (objectified as `DynDatabase`), so this port implements `Database` — the
//! `RwLock` already provided the interior mutability.
//!
//! The lab counters (`sim::sim_metrics`) live on this seam for the same reason
//! they lived on the original's: every call reaching the fallback is BY
//! DEFINITION a `CacheDB` miss, so the forward is the cold fetch. A fresh
//! entry served here is the cross-block reuse signal, and the latency split
//! separates an in-process serve from an RPC-reached one.

use alloy::primitives::map::{AddressHashMap, B256HashMap};
use alloy::primitives::{Address, B256, KECCAK256_EMPTY};
use parking_lot::{Mutex, RwLock};
use std::sync::Arc;
use std::time::Duration;

use degenbot_rpc::provider::AlloyProvider;

use ::evm2::bytecode::Bytecode;
use ::evm2::evm::{AccountInfo, Database};
use ::evm2::interpreter::Word;

use crate::sim::evm2::bal_heads::BalHeadPipeline;
use crate::sim::evm2::delta_snapshot::{BalHeadDriver, DeltaHub};
use crate::sim::evm2::hook_slots::HookSlotStore;
use crate::sim::hot_slots::HotSlotStore;
use crate::sim::prefetch::{
    block_on_ambient, path_relevance_addresses, resolve_read_anchor, PrefetchFacts,
};
use crate::sim::sim_metrics;
use crate::sim::storage_memo::{ReadAnchor, StorageMemo, StorageMemoCache};
use degenbot_core::diag;
use degenbot_execution::PathInfo;

/// The default per-entry TTL in blocks — the same safety-net value as the
/// revm-side original (≈ 1 day of mainnet blocks). A safety net, not the
/// primary correctness mechanism: in the bot's domain every cached contract
/// is immutable.
pub const WARM_CODE_CACHE_TTL_BLOCKS: u64 = 10_000;

/// The development opt-in that lets the dispatch fan-out read candidate storage
/// through a BAL delta generation
/// ([`BalHeadDriver`](crate::sim::evm2::delta_snapshot::BalHeadDriver)) instead
/// of the per-anchor [`StorageMemo`] plus the path/hot/hook prefetch lane.
///
/// # What the flag selects, and what it never changes
///
/// OFF (the default, and the shipped posture) is the legacy path unchanged: the
/// per-anchor memo, the ONE prefetch batch, and the same telemetry. NO BAL is
/// requested at all — the session installs no driver, so there is no code path
/// that could issue one.
///
/// ON resolves the fan-out anchor's exact lineage `(number, hash, parent_hash)`
/// in ONE header read and delivers it to the session's one
/// [`BalHeadDriver`]: the driver holds the session's delta cache, so every
/// fan-out at that exact head shares ONE generation and repeats no BAL work.
/// The head's relevance set is the fan-out's own
/// [`path_relevance_addresses`](crate::sim::prefetch::path_relevance_addresses)
/// and its retention is [`Retention::Unbounded`] — the flag adds no retention
/// knob. A head that settled READY answers candidate storage through its
/// `DeltaHub` (a [`StorageView`](super::storage_view::StorageView)) and pays no
/// prefetch batch; a head that is unprepared, past its readiness deadline,
/// superseded, unreadable (unsupported endpoint, malformed list) or
/// lineage-refused takes the legacy memo + prefetch lane with the reason
/// reported, and serves no parent value on that fallback.
///
/// The readiness deadline bounds the ONE thing the opt-in may wait for: how
/// long a fan-out may wait on its head's BAL before it settles on the legacy
/// path. Candidate execution is never blocked past it.
///
/// # Enabling it, and the metrics that say what happened
///
/// The flag rides the session's warm-cache owner, so a session opts in at ONE
/// construction site: [`WarmCodeCacheInner::shared_with_delta_dispatch`] (or
/// [`WarmCodeCacheInner::shared_with_hook_slots_and_delta_dispatch`] where the
/// owner also installs the durable hook store). There is no retention knob and
/// no second switch: the driver is built at
/// [`Retention::Unbounded`](crate::sim::evm2::delta_snapshot::Retention) and
/// [`DEFAULT_MAX_DEPTH`](crate::sim::evm2::delta_snapshot::DEFAULT_MAX_DEPTH).
///
/// The operator-facing switch is the typed `simulation.delta_dispatch`
/// (`DEGENBOT_DELTA_DISPATCH`) key, default OFF. Both production owners read it
/// ONCE at construction: the PyO3 engine's session owner installs the hook
/// store and this config on one cache, and the pure-Rust `MarketContext` owner
/// installs the config on its own. Nothing re-reads the flag per fan-out, and a
/// test-built cache ([`WarmCodeCacheInner::shared_default`]) stays OFF.
///
/// Every opt-in fan-out reports its selection on
/// `degenbot.dispatch.storage{mode,reason}` — `mode` is `delta` or `legacy`,
/// and `reason` is the closed cold set (`unprepared`, `deadline`,
/// `superseded`, `undelivered`, `lineage-refused`, `bal-unreadable`, `settled`,
/// `lineage-unreadable`) or `none`. A ready delta fan-out also leaves
/// `degenbot.storage.prefetch` untouched (it dispatches no batch), which is what
/// makes the two lanes distinguishable from a scrape while the flag is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeltaDispatchConfig {
    /// How long a delivered head may stay unprepared before every caller at it
    /// is told the deadline elapsed and takes the legacy path.
    pub readiness_deadline: Duration,
}

impl DeltaDispatchConfig {
    /// The readiness deadline the development opt-in uses: long enough for a
    /// local node's BAL, short enough that a stalled endpoint costs the fan-out
    /// a small fraction of a block.
    pub const DEVELOPMENT_READINESS_DEADLINE: Duration = Duration::from_millis(50);

    /// The development opt-in at
    /// [`Self::DEVELOPMENT_READINESS_DEADLINE`].
    #[must_use]
    pub const fn development() -> Self {
        Self {
            readiness_deadline: Self::DEVELOPMENT_READINESS_DEADLINE,
        }
    }

    /// The opt-in with an explicit readiness deadline (the A/B and test knob).
    #[must_use]
    pub const fn with_readiness_deadline(readiness_deadline: Duration) -> Self {
        Self { readiness_deadline }
    }
}

/// The storage representation one fan-out reads its candidates through.
pub enum FanOutStorage {
    /// The legacy per-anchor memo plus the path/hot/hook prefetch lane — the
    /// default, and the fallback for every head that did not settle ready.
    Legacy,
    /// A ready BAL delta generation at the fan-out's exact anchor.
    Delta(Arc<DeltaHub>),
}

/// Why a fan-out's storage selection resolved the way it did — the closed label
/// set the diagnostic line and the metric labels share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageSelectionOutcome {
    /// The opt-in is off: the legacy path, and no BAL request was made.
    Disabled,
    /// The exact head settled ready: the delta generation answers.
    Ready,
    /// The exact head did not settle ready in time (or settled cold); the
    /// legacy lane answers and the label says why.
    Cold(&'static str),
    /// The head's lineage could not be read, so no head was delivered.
    LineageUnreadable,
}

impl StorageSelectionOutcome {
    /// Which path this fan-out reads through (`delta` / `legacy`).
    #[must_use]
    pub const fn mode(&self) -> &'static str {
        match self {
            Self::Ready => "delta",
            Self::Disabled | Self::Cold(_) | Self::LineageUnreadable => "legacy",
        }
    }

    /// The reason label a legacy fallback reports, and `None` when no fallback
    /// happened.
    #[must_use]
    pub const fn reason(&self) -> Option<&'static str> {
        match self {
            Self::Disabled | Self::Ready => None,
            Self::Cold(reason) => Some(reason),
            Self::LineageUnreadable => Some("lineage-unreadable"),
        }
    }
}

/// What one fan-out's storage selection resolved to.
pub struct DispatchStorageSelection {
    /// The anchor the legacy memo (and the delta head) is fenced by.
    pub anchor: ReadAnchor,
    /// The representation this fan-out reads through.
    pub storage: FanOutStorage,
    /// The outcome label, for the diagnostic and metric surfaces.
    pub outcome: StorageSelectionOutcome,
}

impl DispatchStorageSelection {
    /// Whether a ready delta generation answers this fan-out (and the prefetch
    /// lane is therefore skipped).
    #[must_use]
    pub const fn is_delta(&self) -> bool {
        matches!(self.storage, FanOutStorage::Delta(_))
    }
}

/// Resolve one fan-out's storage selection at `block`.
///
/// OFF (no [`DeltaDispatchConfig`] on the session) is the legacy path exactly:
/// the number+hash read anchor and no BAL request anywhere.
///
/// ON resolves the block's exact lineage `(number, hash, parent_hash)` in ONE
/// header read, delivers it to the session's one [`BalHeadDriver`] with the
/// fan-out's own [`path_relevance_addresses`] and unbounded retention, and
/// answers with the head's generation when that exact head settled ready. A
/// head that is unprepared, past its readiness deadline, superseded,
/// unreadable (unsupported endpoint or malformed list), or lineage-refused
/// answers [`FanOutStorage::Legacy`] with its reason — the caller then runs the
/// existing memo + prefetch lane, and no parent value is served on that
/// fallback.
///
/// The BAL work rides this call: it is prepared and applied under the
/// configured readiness deadline, so a stalled or unsupported endpoint costs
/// the fan-out at most that deadline and never blocks candidate execution past
/// it. A repeated fan-out at the SAME exact head shares the settled generation
/// and fetches nothing.
#[must_use]
pub fn select_dispatch_storage(
    provider: &AlloyProvider,
    warm_cache: &Arc<RwLock<WarmCodeCacheInner>>,
    block: u64,
    paths: &[&PathInfo],
    facts: &PrefetchFacts,
) -> DispatchStorageSelection {
    let (config, heads) = {
        let cache = warm_cache.read();
        let Some(config) = cache.delta_dispatch() else {
            // Off: the legacy anchor read, and nothing that could request a BAL.
            return DispatchStorageSelection {
                anchor: resolve_read_anchor(provider, block),
                storage: FanOutStorage::Legacy,
                outcome: StorageSelectionOutcome::Disabled,
            };
        };
        (config, cache.delta_heads(provider))
    };
    let Some(heads) = heads else {
        return DispatchStorageSelection {
            anchor: resolve_read_anchor(provider, block),
            storage: FanOutStorage::Legacy,
            outcome: StorageSelectionOutcome::Disabled,
        };
    };

    // The opt-in's ONE header read: the whole lineage, never one read per hash.
    let lineage = match block_on_ambient(provider.get_block_lineage_at(block)) {
        Some(Ok(Some(lineage))) => lineage,
        other => {
            if let Some(Err(error)) = other {
                diag!(
                    domain = sim,
                    anchor = block,
                    "delta head lineage unreadable: {error}"
                );
            }
            return DispatchStorageSelection {
                anchor: ReadAnchor::at(block),
                storage: FanOutStorage::Legacy,
                outcome: StorageSelectionOutcome::LineageUnreadable,
            };
        }
    };
    let anchor = ReadAnchor::identified(lineage.number, lineage.hash);
    // Relevance is the APPLY's argument, not the delivery's: a head's delivery
    // may come from the block clock (no candidates, no relevance), and the set
    // only has to exist when the head's writes are applied. This lane is its
    // own applier for now, so it hands over the fan-out's own set.
    let relevance = path_relevance_addresses(paths.iter().copied(), facts);
    let ticket = heads.deliver(anchor, lineage.parent_hash);

    // A head that already settled at this exact anchor answers on the spot and
    // fetches nothing.
    let settled = heads.snapshot(anchor);
    if settled.readiness.is_ready() {
        return DispatchStorageSelection {
            anchor,
            storage: FanOutStorage::Delta(settled.hub),
            outcome: StorageSelectionOutcome::Ready,
        };
    }

    // Prepare and apply the head's BAL, bounded by the readiness deadline: a
    // stalled endpoint costs this fan-out the deadline and no more.
    let _ = block_on_ambient(async {
        let _ = tokio::time::timeout(config.readiness_deadline, heads.advance(ticket, &relevance))
            .await;
    });

    let view = heads.snapshot(anchor);
    match view.readiness.cold_reason() {
        None => DispatchStorageSelection {
            anchor,
            storage: FanOutStorage::Delta(view.hub),
            outcome: StorageSelectionOutcome::Ready,
        },
        Some(reason) => DispatchStorageSelection {
            anchor,
            storage: FanOutStorage::Legacy,
            outcome: StorageSelectionOutcome::Cold(reason.label()),
        },
    }
}

/// The cross-block persistent inner state — bytecode + account-existence
/// maps, each entry tagged with its load block for per-entry TTL expiry.
/// Same ownership shape as the original: held behind `Arc<RwLock<...>>` by
/// the engine owner, cloned into each per-block wrapper value.
pub struct WarmCodeCacheInner {
    /// `(loaded_block, AccountInfo)` per address. `None` (no-account) IS
    /// cached (existence-negative) — saves the cold read on the
    /// balanceOf-to-fresh-address case.
    accounts: AddressHashMap<(u64, Option<AccountInfo>)>,
    /// `(loaded_block, Bytecode)` per `code_hash` (immutable by hash).
    bytecode: B256HashMap<(u64, Bytecode)>,
    /// An entry loaded at `loaded_block` is a miss when
    /// `block - loaded_block > ttl_blocks` (strictly greater — an entry
    /// loaded THIS block is always fresh).
    ttl_blocks: u64,
    /// The session's per-anchor storage memo (the MUTABLE row this layer
    /// deliberately never caches cross-block): a block's sim fan-outs share
    /// the memo of the block they pin their RPC reads to, and the anchor
    /// moving retires it. Owned here because this value is the engine's
    /// session-scoped shared-read-cache handle — the same one every handle
    /// build receives.
    storage_memo: StorageMemoCache,
    /// The session's capture-learned hot-slot set (the prewarm's history half,
    /// see [`HotSlotStore`]): CROSS-block by design — what one anchor's captures
    /// teach is exactly what the next anchor's prewarm wants, so it outlives the
    /// per-anchor memo it feeds. Owned here for the same reason the memo cache
    /// is: this value is the engine's session-scoped shared-read-cache handle.
    hot_slots: Arc<HotSlotStore>,

    /// The durable per-hook slot-set store the black-box quoter learns and the
    /// prewarm joins (ADR-090): CROSS-block and cross-RUN by design — a hook's
    /// touched-slot set is learned once and outlives every anchor that taught
    /// it. `None` until the engine owner installs one (a session without it
    /// joins no hook slots; nothing else changes).
    hook_slots: Option<Arc<HookSlotStore>>,

    /// The dispatch fan-out's development opt-in ([`DeltaDispatchConfig`]).
    /// `None` — the default — is the legacy memo + prefetch path with no BAL
    /// read anywhere.
    delta_dispatch: Option<DeltaDispatchConfig>,

    /// The session's one BAL delta head driver, installed on the first opt-in
    /// fan-out. Session-scoped on purpose: every fan-out at one exact head
    /// shares its generation, so a block's lanes repeat no BAL work.
    delta_heads: Mutex<Option<Arc<BalHeadDriver>>>,

    /// The session's canonical-head BAL pipeline — the accepted-header consumer
    /// the composition installs on the stage surface, and the ONE owner of the
    /// session's off-path preparation.
    ///
    /// `None` — and unreachable — while the opt-in is off, so an OFF session
    /// installs no sink and requests nothing. Present-but-UNARMED is the window
    /// between construction (the Python engine's boot) and the batch executor's
    /// arming, and it is inert by construction: an unarmed pipeline delivers
    /// nothing.
    bal_pipeline: Mutex<Option<Arc<BalHeadPipeline>>>,
}

impl WarmCodeCacheInner {
    /// An empty inner state with the given per-entry TTL.
    #[must_use]
    fn new(ttl_blocks: u64) -> Self {
        Self {
            accounts: AddressHashMap::default(),
            bytecode: B256HashMap::default(),
            ttl_blocks,
            storage_memo: StorageMemoCache::new(),
            hot_slots: Arc::new(HotSlotStore::new()),
            hook_slots: None,
            delta_dispatch: None,
            delta_heads: Mutex::new(None),
            bal_pipeline: Mutex::new(None),
        }
    }

    /// The engine-owner construction shape: a shared empty inner state with
    /// the default TTL, cloned into each per-block wrapper.
    #[must_use]
    pub fn shared_default() -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(Self::new(WARM_CODE_CACHE_TTL_BLOCKS)))
    }

    /// A shared inner state with a custom TTL (TTL-boundary tests).
    #[must_use]
    pub fn shared_with_ttl(ttl_blocks: u64) -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(Self::new(ttl_blocks)))
    }

    /// The engine-owner construction shape with the durable per-hook slot-set
    /// store installed (ADR-090) — the store the black-box quoter learns and
    /// the prewarm joins. A named constructor rather than a post-hoc install so
    /// the owner never takes a lock it then holds across a call.
    #[must_use]
    pub fn shared_with_hook_slots(store: Arc<HookSlotStore>) -> Arc<RwLock<Self>> {
        let mut inner = Self::new(WARM_CODE_CACHE_TTL_BLOCKS);
        inner.hook_slots = Some(store);
        Arc::new(RwLock::new(inner))
    }

    /// The engine-owner construction shape with the durable per-hook slot-set
    /// store AND the dispatch BAL delta opt-in installed — the session owner
    /// that reads the development flag ([`DeltaDispatchConfig`]) once at boot
    /// and hands the same session one driver for its life.
    #[must_use]
    pub fn shared_with_hook_slots_and_delta_dispatch(
        store: Arc<HookSlotStore>,
        config: DeltaDispatchConfig,
    ) -> Arc<RwLock<Self>> {
        let mut inner = Self::new(WARM_CODE_CACHE_TTL_BLOCKS);
        inner.hook_slots = Some(store);
        inner.delta_dispatch = Some(config);
        Arc::new(RwLock::new(inner))
    }

    /// A shared inner state carrying the dispatch BAL delta opt-in and no hook
    /// store (the tests' and the A/B's construction shape).
    #[must_use]
    pub fn shared_with_delta_dispatch(config: DeltaDispatchConfig) -> Arc<RwLock<Self>> {
        let mut inner = Self::new(WARM_CODE_CACHE_TTL_BLOCKS);
        inner.delta_dispatch = Some(config);
        Arc::new(RwLock::new(inner))
    }

    /// The dispatch BAL delta opt-in this session carries, when it carries one.
    #[must_use]
    pub const fn delta_dispatch(&self) -> Option<DeltaDispatchConfig> {
        self.delta_dispatch
    }

    /// The session's BAL delta head driver, installed on FIRST use when the
    /// opt-in is on — `None` (and no BAL request anywhere) while it is off.
    ///
    /// Installed under this cache's `&self` (the caller holds the engine's
    /// `RwLock` read guard) and handed back as an owned `Arc` so the caller
    /// drops the guard before any provider I/O. One driver per session, so the
    /// driver's generations, its delivered head, and its BAL work are shared by
    /// every fan-out at one exact anchor.
    #[must_use]
    pub fn delta_heads(&self, provider: &AlloyProvider) -> Option<Arc<BalHeadDriver>> {
        let config = self.delta_dispatch?;
        let mut slot = self.delta_heads.lock();
        Some(Arc::clone(slot.get_or_insert_with(|| {
            Arc::new(BalHeadDriver::new(provider, config.readiness_deadline))
        })))
    }

    /// The session's canonical-head BAL pipeline, present exactly when the
    /// opt-in is on — and UNARMED until [`Self::arm_bal_heads`] runs.
    ///
    /// The composition installs this value as the stage surface's head sink
    /// (`install_canonical_head_sink`); because installation needs no provider
    /// and no arming, it happens at engine boot, before any executor exists.
    /// An OFF session gets `None` here, so it installs no sink at all.
    #[must_use]
    pub fn bal_head_pipeline(&self) -> Option<Arc<BalHeadPipeline>> {
        self.delta_dispatch?;
        let mut slot = self.bal_pipeline.lock();
        Some(Arc::clone(
            slot.get_or_insert_with(BalHeadPipeline::unarmed),
        ))
    }

    /// Arm the session's pipeline with its one head driver (the batch
    /// executor's construction calls this, because that is the one place in
    /// either composition where the provider, the warm cache and the opt-in
    /// already meet).
    ///
    /// A no-op — no driver, no request — when the opt-in is off (there is no
    /// pipeline to arm) and when the pipeline is already armed (one generation
    /// space per session). Arming builds the driver but fetches nothing.
    pub fn arm_bal_heads(&self, provider: &AlloyProvider) {
        let Some(pipeline) = self.bal_head_pipeline() else {
            return;
        };
        let Some(driver) = self.delta_heads(provider) else {
            return;
        };
        pipeline.arm(driver);
    }

    /// Is an entry loaded at `loaded_block` still fresh at `block`?
    fn is_fresh(&self, loaded_block: u64, block: u64) -> bool {
        block.saturating_sub(loaded_block) <= self.ttl_blocks
    }

    /// The storage memo for the read anchor `anchor` — the SAME memo for every
    /// fan-out at that anchor, a fresh one as soon as the anchor moves, changes
    /// its block (a same-height reorg), or loses its hash (see
    /// [`StorageMemoCache`]). Takes `&self`: callers hold this inner behind the
    /// engine's `RwLock` read guard.
    #[must_use]
    pub fn storage_memo_for(&self, anchor: ReadAnchor) -> Arc<StorageMemo> {
        self.storage_memo.for_anchor(anchor)
    }

    /// The session's capture-learned hot-slot set — the prewarm's history half,
    /// and the store every handle session's captures teach. Takes `&self` and
    /// hands back an owned handle so the caller can drop the `RwLock` guard
    /// before any provider I/O (the fan-out never holds it across a read).
    #[must_use]
    pub fn hot_slots(&self) -> Arc<HotSlotStore> {
        Arc::clone(&self.hot_slots)
    }

    /// The installed per-hook slot-set store, if the engine owner set one.
    /// Takes `&self` and hands back an owned handle for the same reason
    /// [`Self::hot_slots`] does: the fan-out never holds the lock guard across
    /// provider I/O.
    #[must_use]
    pub fn hook_slots(&self) -> Option<Arc<HookSlotStore>> {
        self.hook_slots.clone()
    }
}

/// A cross-block warm `Database` wrapper — caches accounts + bytecode (the
/// immutable rows) with a per-entry TTL, forwards storage + block hashes
/// (the mutable-per-block rows, never cached).
pub struct WarmCodeCache<Db>
where
    Db: Database,
{
    /// The cross-block persistent maps, shared via the engine owner's arc.
    cache: Arc<RwLock<WarmCodeCacheInner>>,
    /// The block this cache view is for (drives per-entry TTL expiry).
    block: u64,
    /// The per-block cold-miss fallback.
    db: Db,
}

impl<Db> WarmCodeCache<Db>
where
    Db: Database,
{
    /// Build a per-block view over a shared inner cache + a per-block
    /// fallback (`BotStateDb` over the RPC adapter in the target stack).
    #[must_use]
    pub fn with_owner(cache: Arc<RwLock<WarmCodeCacheInner>>, block: u64, db: Db) -> Self {
        Self { cache, block, db }
    }

    /// The number of cold reads the fallback has served — the parity gate's
    /// cache-effectiveness probe.
    #[cfg(test)]
    pub fn inner(&self) -> &Db {
        &self.db
    }
}

impl<Db> Database for WarmCodeCache<Db>
where
    Db: Database,
{
    type Error = Db::Error;

    /// Serve from the warm cache if the entry is fresh; else forward + cache
    /// the result (existence-negative `None` IS cached). EOA rows are NEVER
    /// cached — an EOA's nonce + balance mutate every block and the warm
    /// layer is cross-block.
    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        {
            let guard = self.cache.read();
            if let Some((loaded_block, info)) = guard.accounts.get(address) {
                if guard.is_fresh(*loaded_block, self.block) {
                    // Lab: the cross-block warm cache served a `CacheDB` miss
                    // (reuse signal — no underlying call happened).
                    sim_metrics::record_basic_warm_hit();
                    return Ok(info.clone());
                }
            }
        }
        let (info, dur) = sim_metrics::timed(|| self.db.get_account(address));
        sim_metrics::record_basic(dur);
        let info = info?;
        let is_contract = info
            .as_ref()
            .is_some_and(|i| i.code_hash != KECCAK256_EMPTY);
        if is_contract {
            let mut guard = self.cache.write();
            guard.accounts.insert(*address, (self.block, info.clone()));
        }
        Ok(info)
    }

    /// Serve bytecode from the warm cache if fresh; else forward + cache.
    /// Defensive like the original: a fallback whose `get_account` eager-loads
    /// code (the RPC adapter's shape) never reaches this.
    fn get_code_by_hash(&mut self, code_hash: &B256) -> Result<Bytecode, Self::Error> {
        {
            let guard = self.cache.read();
            if let Some((loaded_block, code)) = guard.bytecode.get(code_hash) {
                if guard.is_fresh(*loaded_block, self.block) {
                    return Ok(code.clone());
                }
            }
        }
        let (code, dur) = sim_metrics::timed(|| self.db.get_code_by_hash(code_hash));
        sim_metrics::record_code(dur);
        let code = code?;
        let mut guard = self.cache.write();
        guard
            .bytecode
            .insert(*code_hash, (self.block, code.clone()));
        Ok(code)
    }

    /// Forward — storage is the mutable-per-block row, NEVER cached.
    fn get_storage(&mut self, address: &Address, key: &Word) -> Result<Word, Self::Error> {
        // Lab: storage ALWAYS forwards — the latency split sizes the
        // in-process-served (memo/anchor) vs RPC-reached shares.
        let (value, dur) = sim_metrics::timed(|| self.db.get_storage(address, key));
        sim_metrics::record_storage(dur);
        value
    }

    /// Forward — block hashes are per-chain, NEVER cached.
    fn get_block_hash(&mut self, number: &Word) -> Result<B256, Self::Error> {
        sim_metrics::record_block_hash();
        self.db.get_block_hash(number)
    }
}

#[cfg(test)]
#[expect(clippy::panic, clippy::expect_used)]
mod tests {
    use super::*;
    use alloy::primitives::{Bytes, U256};
    use std::cell::Cell;
    use std::convert::Infallible;

    const CONTRACT: Address = Address::new([0xaa; 20]);
    const EOA: Address = Address::new([0xee; 20]);

    fn contract_info() -> AccountInfo {
        AccountInfo::default()
            .with_balance(U256::from(1u64))
            .with_code(Bytecode::new_legacy(Bytes::from_static(&[0x60, 0x00])))
    }

    /// A counting mock fallback: scripted account answers, zero storage.
    struct CountingDb {
        account: Option<AccountInfo>,
        account_calls: Cell<u64>,
        storage_calls: Cell<u64>,
    }

    impl CountingDb {
        fn new(account: Option<AccountInfo>) -> Self {
            Self {
                account,
                account_calls: Cell::new(0),
                storage_calls: Cell::new(0),
            }
        }
    }

    impl Database for CountingDb {
        type Error = Infallible;
        fn get_account(&mut self, _address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
            self.account_calls.set(self.account_calls.get() + 1);
            Ok(self.account.clone())
        }
        fn get_code_by_hash(&mut self, _code_hash: &B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::default())
        }
        fn get_storage(&mut self, _address: &Address, _key: &Word) -> Result<Word, Self::Error> {
            self.storage_calls.set(self.storage_calls.get() + 1);
            Ok(Word::ZERO)
        }
        fn get_block_hash(&mut self, _number: &Word) -> Result<B256, Self::Error> {
            Ok(B256::ZERO)
        }
    }

    #[test]
    fn contract_account_is_cached_across_wrappers() {
        let inner = WarmCodeCacheInner::shared_default();
        let mut first =
            WarmCodeCache::with_owner(inner.clone(), 100, CountingDb::new(Some(contract_info())));
        let info = first.get_account(&CONTRACT).unwrap();
        assert!(info.is_some());
        assert_eq!(first.inner().account_calls.get(), 1, "cold miss forwarded");

        let mut second =
            WarmCodeCache::with_owner(inner, 101, CountingDb::new(Some(contract_info())));
        let info = second.get_account(&CONTRACT).unwrap();
        assert!(info.is_some());
        assert_eq!(
            second.inner().account_calls.get(),
            0,
            "warm hit served the next block"
        );
    }

    #[test]
    fn eoa_account_is_never_cached() {
        let inner = WarmCodeCacheInner::shared_default();
        let mut first = WarmCodeCache::with_owner(inner.clone(), 100, CountingDb::new(None));
        assert!(first.get_account(&EOA).unwrap().is_none());

        let mut second = WarmCodeCache::with_owner(inner, 100, CountingDb::new(None));
        assert!(second.get_account(&EOA).unwrap().is_none());
        assert_eq!(
            second.inner().account_calls.get(),
            1,
            "EOA rows always forward"
        );
    }

    #[test]
    fn storage_always_forwards() {
        let inner = WarmCodeCacheInner::shared_default();
        let mut cache = WarmCodeCache::with_owner(inner, 100, CountingDb::new(None));
        for _ in 0..3 {
            cache.get_storage(&CONTRACT, &Word::from(8u64)).unwrap();
        }
        assert_eq!(cache.inner().storage_calls.get(), 3);
    }

    /// A provider no request may reach: any dispatch is a bug, so it panics.
    #[derive(Debug, Clone)]
    struct ForbiddenTransport;

    impl tower::Service<alloy::rpc::json_rpc::RequestPacket> for ForbiddenTransport {
        type Response = alloy::rpc::json_rpc::ResponsePacket;
        type Error = alloy::transports::TransportError;
        type Future = alloy::transports::TransportFut<'static>;

        fn poll_ready(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: alloy::rpc::json_rpc::RequestPacket) -> Self::Future {
            let methods: Vec<String> = match req {
                alloy::rpc::json_rpc::RequestPacket::Single(one) => {
                    vec![one.method().to_string()]
                }
                alloy::rpc::json_rpc::RequestPacket::Batch(many) => {
                    many.iter().map(|one| one.method().to_string()).collect()
                }
            };
            panic!("no request may be dispatched here, got {methods:?}");
        }
    }

    fn forbidden_provider() -> AlloyProvider {
        use alloy::providers::RootProvider;
        use alloy::rpc::client::ClientBuilder;

        let client = ClientBuilder::default().transport(ForbiddenTransport, true);
        let root: RootProvider = RootProvider::new(client);
        AlloyProvider::from_provider(Arc::new(root))
    }

    /// The shipped posture: no opt-in, so the session holds no head driver and
    /// nothing that could request a BAL — the transport is never reached.
    #[test]
    fn the_delta_dispatch_opt_in_is_off_by_default_and_requests_nothing() {
        let inner = WarmCodeCacheInner::shared_default();
        assert_eq!(inner.read().delta_dispatch(), None);
        assert!(
            inner.read().delta_heads(&forbidden_provider()).is_none(),
            "an opt-out session has no head driver"
        );
    }

    /// One driver per session: the first opt-in fan-out installs it, later ones
    /// share the same handle (so every fan-out at one exact head shares one
    /// generation and repeats no BAL work), and installation itself fetches
    /// nothing.
    #[test]
    fn the_delta_head_driver_is_installed_once_and_shared() {
        let inner =
            WarmCodeCacheInner::shared_with_delta_dispatch(DeltaDispatchConfig::development());
        let provider = forbidden_provider();
        let cache = inner.read();
        assert_eq!(
            cache.delta_dispatch(),
            Some(DeltaDispatchConfig::development())
        );
        let first = cache.delta_heads(&provider).expect("the opt-in is on");
        let second = cache.delta_heads(&provider).expect("the opt-in is on");
        assert!(
            Arc::ptr_eq(&first, &second),
            "one session-scoped driver, never one per fan-out"
        );
        assert_eq!(
            first.delivered(),
            None,
            "installation delivers no head and fetches nothing"
        );
    }

    /// The production construction shape carries the opt-in AND the durable
    /// hook store — the two session-scoped stores are installed by one owner,
    /// and neither displaces the other.
    #[test]
    fn the_hook_store_and_the_delta_opt_in_ride_the_same_owner() {
        let store = Arc::new(HookSlotStore::in_memory(::evm2::SpecId::CANCUN));
        let inner = WarmCodeCacheInner::shared_with_hook_slots_and_delta_dispatch(
            Arc::clone(&store),
            DeltaDispatchConfig::with_readiness_deadline(Duration::from_millis(7)),
        );
        let cache = inner.read();
        assert!(cache.hook_slots().is_some(), "the hook store survives");
        assert_eq!(
            cache.delta_dispatch(),
            Some(DeltaDispatchConfig::with_readiness_deadline(
                Duration::from_millis(7)
            ))
        );
        assert!(cache.delta_heads(&forbidden_provider()).is_some());
    }

    /// The shipped OFF posture installs NO head sink and can issue no BAL
    /// request: there is no pipeline to install, arming is a no-op, and the
    /// session has no head driver at all. A transport that panics on any
    /// dispatch proves the negative.
    #[test]
    fn the_opt_in_off_session_installs_no_sink_and_issues_no_bal_request() {
        let store = Arc::new(HookSlotStore::in_memory(::evm2::SpecId::CANCUN));
        let inner = WarmCodeCacheInner::shared_with_hook_slots(store);
        let provider = forbidden_provider();
        let cache = inner.read();
        assert_eq!(cache.delta_dispatch(), None, "the flag is off");
        assert!(
            cache.bal_head_pipeline().is_none(),
            "an OFF session has no pipeline, so the stage surface installs no sink"
        );
        cache.arm_bal_heads(&provider);
        assert!(
            cache.delta_heads(&provider).is_none(),
            "no driver, so nothing could ever fetch a head's list"
        );
        assert!(cache.bal_head_pipeline().is_none());
    }

    /// An ON session's pipeline exists from boot but is UNARMED until the batch
    /// executor's construction arms it: feeding it heads reaches no provider
    /// (it holds no driver), and arming itself fetches nothing.
    #[test]
    fn an_unarmed_pipeline_delivers_nothing_and_requests_nothing() {
        use degenbot_core::canonical_head::{CanonicalHead, CanonicalHeadSink};
        let inner =
            WarmCodeCacheInner::shared_with_delta_dispatch(DeltaDispatchConfig::development());
        let provider = forbidden_provider();
        let cache = inner.read();
        let pipeline = cache.bal_head_pipeline().expect("the opt-in is on");
        assert!(!pipeline.is_armed(), "boot constructs, it does not arm");

        for number in 100..103u64 {
            pipeline.on_canonical_head(CanonicalHead::new(
                number,
                B256::new([u8::try_from(number % 251).unwrap_or(1); 32]),
                B256::ZERO,
            ));
        }
        assert!(
            pipeline.driver().is_none(),
            "an unarmed pipeline owns no driver: no head is delivered, nothing is fetched"
        );
        assert!(!pipeline.is_armed());

        cache.arm_bal_heads(&provider);
        assert!(
            pipeline.is_armed(),
            "arming is what gives the session its one driver"
        );
        assert!(pipeline.driver().is_some());
        assert_eq!(
            pipeline.driver().and_then(|driver| driver.delivered()),
            None,
            "arming delivers no head and fetches nothing"
        );
    }

    #[test]
    fn stale_entry_refreshes_past_the_ttl() {
        let inner = WarmCodeCacheInner::shared_with_ttl(10);
        let mut first =
            WarmCodeCache::with_owner(inner.clone(), 100, CountingDb::new(Some(contract_info())));
        first.get_account(&CONTRACT).unwrap();

        let mut fresh =
            WarmCodeCache::with_owner(inner.clone(), 110, CountingDb::new(Some(contract_info())));
        fresh.get_account(&CONTRACT).unwrap();
        assert_eq!(
            fresh.inner().account_calls.get(),
            0,
            "at the TTL boundary the entry is fresh"
        );

        let mut stale =
            WarmCodeCache::with_owner(inner, 111, CountingDb::new(Some(contract_info())));
        stale.get_account(&CONTRACT).unwrap();
        assert_eq!(
            stale.inner().account_calls.get(),
            1,
            "past the TTL the entry re-fetches"
        );
    }
}
