//! `Bot` — the per-chain orchestrator facade (ADR-006 D4).
//!
//! Extracted from `bot_core/mod.rs` as the realization of ADR-006 D4's
//! "`Bot` (the interface) — thin facade" row: `BotState` (the pure-data
//! registries/swap math/reorg journal) stays in `mod.rs`; the orchestrator
//! facade — `chain_id`, the shared `Arc<RwLock<BotState>>` handed to
//! `PyBot`/`ArbitrageEngine`, the `LogDispatcher` event bus, the construction-
//! I/O handle — lives here as its own `pub(crate)`-deep module with its own
//! test seam. The reachability path `degenbot_bot::bot_core::Bot` is preserved
//! by `pub use bot::Bot;` in `mod.rs` (the 4 external reachers — `block_pump`,
//! `degenbot-python/bot/mod.rs`, `degenbot-python/bot/pump.rs` — are
//! byte-identical).
//!
//! The other ADR-006 D4 helper rows (`LogDispatcher`/`BlockPump`/
//! `ReorgCoordinator`) already live as sibling
//! `bot_core/*.rs` files; `bot.rs` is the last one to file-extract.
//! ( the former `SolveCoordinator` row is dissolved — the arb engine's
//! `EngineStages`/`StageHandlers` surface replaced it.)

use std::sync::Arc;

use crate::bot_core::snapshot_verify::SnapshotLoadError;
use degenbot_substrate::session_registry::SessionObjectRegistry;
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::EpochDelta;
use degenbot_substrate::{log_dispatcher, BotState};

/// The per-chain orchestrator: a thin facade over a shared
/// [`BotState`] (the pure-data registries/swap math/reorg journal) plus the
/// `chain_id` (ADR-006 D1) and, in later slices, the cohesive helpers
/// (`LogDispatcher` / `BlockPump` / `ReorgCoordinator`) — the engine seam is
/// the arb engine's `StageHandlers` surface (the former
/// `SolveCoordinator` is dissolved).
///
/// `PyBot` owns a `Bot` outright (not behind a lock) and hands out clones of
/// [`Bot::state_arc`] so `PyLiquidityPool` / `PyErc20Token` / `ArbitrageEngine`
/// all reach ONE Rust-owned `BotState` (N handles → one state — the Polars
/// three-layer invariant, preserved). The standalone-Rust path (D4) runs the
/// whole bot through this facade without Python.
pub struct Bot {
    /// The chain this bot orchestrates (ADR-006 D1+D5: one `Bot` per chain).
    /// Read by the standalone-Rust path; `PyBot` currently stubs `0` —
    /// real wiring lands when ADR-006 D4 makes `chain_id` a Bot-level
    /// construction-time invariant used for cross-chain validation (see
    /// `docs/adr/ADR-006-bot-as-per-chain-orchestrator.md` §D4).
    chain_id: u64,
    /// The shared pure-data state. Handles clone this `Arc`.
    state: Arc<StateLock<BotState>>,
    /// The session object registry — the one per-session owner of canonical
    /// identity for this session's pools and tokens. Identity only: it holds no
    /// provider, DB handle, or tick fetcher, and never writes `state`; the live
    /// state stays with [`BotState`]. Scoped to the session (ADR-006 D5: one
    /// `Bot` per chain): [`Bot::new`] mints the session's root registry, and
    /// [`Bot::with_core`] adopts the one the session owner already resolved, so
    /// every `Bot` in a session shares one identity key space.
    registry: Arc<SessionObjectRegistry>,
    /// The per-`Bot` event bus (ADR-006 D4). The pump drives
    /// [`dispatch_log`](Self::dispatch_log) per WS log.
    dispatcher: log_dispatcher::LogDispatcher,
    /// The epoch's touched-pool ledger: every
    /// successful log application records its touched `(HopType, pool_id)`
    /// key here as a BYPRODUCT of [`dispatch_log`](Self::dispatch_log).
    /// Shared with the drain seam (the coordinator consumes it at solve
    /// time) so exactly ONE dirty-tracking mechanism exists.
    delta: Arc<EpochDelta>,
    /// The construction-I/O handle (architecture review 2025-07-18 / candidate 1).
    /// `None` for a bare `Bot::new(chain_id)` (the test-fixture + standalone-
    /// Rust-no-I/O path). The Python `Bot.__init__` path attaches one via
    /// [`Bot::set_construction_io`] built from the extracted `AlloyProvider`
    /// and an optional held `DegenbotDb`; the 7 generic RPC + 12 DB atomic
    /// methods on `PyBotIo` delegate to this, the 27 choreography wrappers stay
    /// on `PyBotIo` for now (deleted with the builder-choreography port).
    ///
    /// Interior-mutable (`RwLock`) so a `Bot` shared via `Arc` can have the
    /// handle attached post-construction (the `PyBot` path: `PyBot::new(chain_id)`
    /// happens before the provider is known, then `set_construction_io` attaches).
    construction_io:
        parking_lot::RwLock<Option<Arc<crate::bot_core::construction_io::ConstructionIo>>>,
}

impl Bot {
    /// Construct a new orchestrator for `chain_id` over a fresh `BotState` and
    /// the session's freshly minted object registry — the session ROOT, for a
    /// caller that owns the session rather than adopting one. A caller
    /// adopting an existing session's core uses [`Bot::with_core`] and hands
    /// that session's registry in, so it never starts a second one.
    ///
    /// ADR-006 slice 8b: the Python `Bot` facade is single-chain and passes the
    /// real `chain_id` via `PyBot::new(chain_id)`; `0` is the default for the
    /// bare-fixture test path. The construction-I/O handle is `None` until
    /// [`Bot::set_construction_io`] attaches one (the Python path does this at
    /// `Bot.__init__` time).
    /// Immutable accessor for the per-Bot event bus (the pump wires its
    /// per-pump completeness stance into the dispatcher's strict fault here).
    #[must_use]
    pub fn dispatcher(&self) -> &log_dispatcher::LogDispatcher {
        &self.dispatcher
    }

    #[must_use]
    pub fn new(chain_id: u64) -> Self {
        Self {
            chain_id,
            state: Arc::new(StateLock::new(BotState::new())),
            registry: Arc::new(SessionObjectRegistry::new(chain_id)),
            dispatcher: log_dispatcher::LogDispatcher::with_uniswap_decoders(),
            delta: Arc::new(EpochDelta::new(0u64)),
            construction_io: parking_lot::RwLock::new(None),
        }
    }

    /// Construct a `Bot` that **adopts** an already-resolved session: a shared
    /// `BotState` core, that session's ONE object registry, and a fresh
    /// `LogDispatcher` (ADR-006 D4). Used so a `Bot` + a `ArbitrageEngine`
    /// (and a sibling `PyBot`) all read/write the SAME `BotState` — the engine
    /// gets the core via `ArbitrageEngine::with_core`, `BlockPump`'s `Bot`
    /// shares it, and `dispatch_log` writes flow through to the engine's reads.
    ///
    /// # The registry is supplied, never minted
    ///
    /// `session_registry` is an argument because the caller — the session
    /// owner — has already resolved it. A session is one chain and one
    /// registry (ADR-006 D5), so a constructor that minted its own would let a
    /// second adopting `Bot` over the same core silently start a second
    /// identity key space for the same session: the violation this signature
    /// removes. Every adopter joins the registry it is handed, and there is no
    /// other minting path on `Bot` besides [`Bot::new`], which is itself a
    /// session ROOT (it allocates the `BotState` too). A consumer that needs
    /// its own identity space is therefore declaring a new session, and
    /// `Bot::new` is the only way to say that.
    ///
    /// The chain scope is read off `session_registry`
    /// ([`SessionObjectRegistry::chain_id`]) rather than passed separately, so
    /// the orchestrator's chain and the identity key space's chain are one
    /// value by construction and cannot drift apart. It does not carry a
    /// `construction_io` handle (the original owner attached one if needed;
    /// adopters that need I/O re-attach via [`Bot::with_construction_io`]).
    #[must_use]
    pub fn with_core(
        core: Arc<StateLock<BotState>>,
        session_registry: Arc<SessionObjectRegistry>,
    ) -> Self {
        Self {
            chain_id: session_registry.chain_id(),
            state: core,
            registry: session_registry,
            dispatcher: log_dispatcher::LogDispatcher::with_uniswap_decoders(),
            delta: Arc::new(EpochDelta::new(0u64)),
            construction_io: parking_lot::RwLock::new(None),
        }
    }

    /// The chain this bot orchestrates. Used by the standalone-Rust path;
    /// `PyBot` exposes it as a `#[getter]` so the Python `Bot` facade can
    /// assert its `default_chain_id` was wired through (ADR-006 D4).
    #[must_use]
    pub const fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// Attach a construction-I/O handle (architecture review 2025-07-18 /
    /// candidate 1). The Python `Bot.__init__` path builds the handle from the
    /// extracted `AlloyProvider` + an optional held `DegenbotDb` and attaches
    /// it here; the standalone-Rust path builds + attaches directly.
    ///
    /// Idempotent: a second call replaces the prior handle.
    pub fn set_construction_io(&self, io: crate::bot_core::construction_io::ConstructionIo) {
        *self.construction_io.write() = Some(Arc::new(io));
    }

    /// Hand out a clone of the construction-I/O handle, when attached. `PyBotIo`'s
    /// 7 generic RPC + 12 DB atomic methods reach this to delegate through the
    /// trait objects (`Arc<dyn DbConstruction + Send + Sync>` /
    /// `Arc<dyn RpcConstruction + Send + Sync>`); the 27 choreography wrappers
    /// stay on `PyBotIo` this slice. `None` for a bare bot with no I/O attached.
    #[must_use]
    pub fn construction_io_arc(
        &self,
    ) -> Option<Arc<crate::bot_core::construction_io::ConstructionIo>> {
        self.construction_io.read().clone()
    }

    /// Hand out a clone of the shared `Arc<RwLock<BotState>>` so a sibling
    /// consumer (`PyLiquidityPool` / `PyErc20Token` / `ArbitrageEngine`) reaches
    /// the SAME state this orchestrator owns. This is the Polars three-layer
    /// sharing seam (ADR-005, revised by ADR-006 D4).
    #[must_use]
    pub fn state_arc(&self) -> Arc<StateLock<BotState>> {
        Arc::clone(&self.state)
    }

    /// Hand out a handle to this session's object registry so a consumer (the
    /// engine, a strategy) asks the session's one registry for the canonical
    /// object rather than keeping a private identity map. Clones share the one
    /// registry, exactly as [`state_arc`](Self::state_arc) clones share one
    /// `BotState`: a second `Bot` that adopted the same session reads through
    /// to the same canonical objects.
    #[must_use]
    pub fn session_registry(&self) -> Arc<SessionObjectRegistry> {
        Arc::clone(&self.registry)
    }

    /// Record the snapshot seed block `S` on `BotState` from a held-tx DB
    /// handle. The single entry point a standalone Rust
    /// consumer and the pyo3 `PyBot` constructor both call.
    ///
    /// `db` is a [`degenbot_db::snapshot::TickMapDb`] — typically a
    /// [`degenbot_db::snapshot_db::SnapshotDb`] opened with a held deferred
    /// read transaction so `S` + every per-pool `fetch_liquidity_map` read
    /// share one frozen DB snapshot across `build_paths` (the consistency
    /// replacement for the retired `SnapshotStore`).
    ///
    /// Records `S = min(fetch_newest_update_block(V3), V4)`. `None`
    /// for a family with no pools / NULL `last_update_block`; if BOTH families
    /// are `None`, `S` is `None` (cold-start path — the pump anchors on
    /// `first_observed_block`, no snapshot gap to backfill).
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotLoadError::Db`] on a DB read failure.
    ///
    /// # Panics
    ///
    /// Panics if a `fetch_newest_update_block` returns a negative block number
    /// (invalid DB state — `SQLite` stores block numbers as signed `i64`, but
    /// on-chain block numbers are non-negative).
    pub fn load_snapshot_from_db(
        &self,
        db: &dyn degenbot_db::snapshot::TickMapDb,
        chain_id: u64,
    ) -> Result<(), SnapshotLoadError> {
        let chain = i64::try_from(chain_id)
            .map_err(|_| SnapshotLoadError::Range(format!("chain_id {chain_id} exceeds i64")))?;
        let mut state = self
            .state
            .write_at(degenbot_substrate::state_lock::LockSite::Core);
        let now_v3 = db
            .fetch_newest_update_block(chain, degenbot_db::read::ExchangeFamily::V3)
            .map_err(SnapshotLoadError::from)?;
        let now_v4 = db
            .fetch_newest_update_block(chain, degenbot_db::read::ExchangeFamily::V4)
            .map_err(SnapshotLoadError::from)?;
        // S = min(fetch_newest_update_block(V3), V4), ignoring None families.
        #[expect(clippy::expect_used)] // block numbers are non-negative (documented)
        let s = match (state.snapshot_seed_block(), now_v3, now_v4) {
            (None, Some(v3), Some(v4)) => {
                Some(u64::try_from(v3.min(v4)).expect("block number non-negative"))
            }
            (None, Some(v3), None) => Some(u64::try_from(v3).expect("block number non-negative")),
            (None, None, Some(v4)) => Some(u64::try_from(v4).expect("block number non-negative")),
            (None, None, None) => None,
            (existing, _, _) => existing,
        };
        state.set_snapshot_seed_block(s);
        Ok(())
    }

    /// Drive one WS log through the event bus (ADR-006 D4). Decode via a
    /// registered decoder, apply to `BotState` under a write guard, release,
    /// then record the touched pool into the epoch `EpochDelta`. The pump
    /// calls this per log.
    #[hotpath::measure(impl_type = "Bot")]
    pub fn dispatch_log(&self, log: &alloy::rpc::types::Log) {
        self.dispatcher
            .dispatch(log, &self.state, Some(&self.delta));
    }

    /// Decode `log` into a [`DecodedPoolEvent`] without applying (ADR-006 slice 7).
    /// `ReorgCoordinator` uses this on `removed: true` logs to identify the
    /// target pool before restoring it from the journal.
    pub fn try_decode_log(
        &self,
        log: &alloy::rpc::types::Log,
    ) -> Option<log_dispatcher::DecodedPoolEvent> {
        self.dispatcher.try_decode_log(log)
    }

    /// Resolve a decoded event's `pool_id` against `BotState` (ADR-006 slice 7).
    /// V2/V3 by address, V4 by `(pool_manager, pool_id)` key.
    pub fn resolve_pool_id(&self, event: &log_dispatcher::DecodedPoolEvent) -> Option<u64> {
        event.resolve_pool_id(
            &self
                .state
                .read_at(degenbot_substrate::state_lock::LockSite::Core),
        )
    }

    /// Restore `pool_id`'s state to just before `block` (ADR-006 slice 7).
    /// Writes the journal's landed-at state into the current mutable fields.
    /// Pre-check [`has_state_prior_to`](Self::has_state_prior_to) first — the
    /// V3/V4 journal `restore_before_block` panics on an empty journal.
    pub fn restore_pool_before_block(&self, pool_id: u64, block: u64) {
        // Discard the trait result — the reorg coordinator path is fire-and-
        // forget (too-deep was pre-checked via `has_state_prior_to`).
        let _ = self
            .state
            .write_at(degenbot_substrate::state_lock::LockSite::Core)
            .restore_pool_before_block(pool_id, block);
    }

    /// Peek the newest reorg-journal delta block for `pool_id` (
    /// idempotent-noop detection for the `degenbot.reorg.restore` spans).
    /// `None` when unregistered or the journal is empty.
    #[must_use]
    pub fn newest_journal_block(&self, pool_id: u64) -> Option<u64> {
        self.state
            .read_at(degenbot_substrate::state_lock::LockSite::Core)
            .newest_journal_block(pool_id)
    }

    /// Does `pool_id`'s journal have state at or before `block`? (ADR-006
    /// slice 7.) `false` → a too-deep reorg; `ReorgCoordinator` returns
    /// `Err(NoStatePriorToBlock)` and the pump shuts down gracefully.
    #[must_use]
    pub fn has_state_prior_to(&self, pool_id: u64, block: u64) -> bool {
        self.state
            .read_at(degenbot_substrate::state_lock::LockSite::Core)
            .has_state_prior_to(pool_id, block)
    }

    /// The shared epoch-delta ledger: log application records touched
    /// pools here; the wiring hands clones to
    /// the drain seam so affected-path derivation reads this ledger.
    #[must_use]
    pub fn active_delta(&self) -> Arc<EpochDelta> {
        Arc::clone(&self.delta)
    }

    /// Record `pool_id` as touched in the epoch ledger.
    /// `ReorgCoordinator` calls this after a per-pool restore so the
    /// re-restored pool re-enters the delta + re-solves at the next drain
    /// tick. `hop` is the restored event's family (the coordinator reads it
    /// off the decoded log — no classification lookup) and `block` is the
    /// block the recorded dirt pertains to (forward: the decoded log's
    /// block; reorg: the rewind target) — the ledger buckets by it.
    pub fn record_pool_state_changed(
        &self,
        pool_id: u64,
        hop: degenbot_solvers::mixed::HopType,
        block: u64,
    ) {
        self.delta.record_affected(hop, pool_id, block);
    }

    // === Journal/state-read cluster (moved from the PyO3 shell, ergo
    // === 2IIHQA): the shell keeps only arg parsing + PyErr mapping; the lock
    // === discipline and the per-family guards live here so the
    // === standalone-Rust path gets the same reads.

    /// Number of registered pools (the registry of record's live count).
    #[must_use]
    pub fn pool_count(&self) -> usize {
        self.state
            .read_at(degenbot_substrate::state_lock::LockSite::Core)
            .pool_count()
    }

    /// Is `pool_id` registered? The `get_pool` handle-minting pre-check.
    #[must_use]
    pub fn has_pool(&self, pool_id: u64) -> bool {
        self.state
            .read_at(degenbot_substrate::state_lock::LockSite::Core)
            .has_pool(pool_id)
    }

    /// Is `address` registered as a token? The `get_token` handle-minting
    /// pre-check.
    #[must_use]
    pub fn has_token(&self, address: &alloy::primitives::Address) -> bool {
        self.state
            .read_at(degenbot_substrate::state_lock::LockSite::Core)
            .has_token(address)
    }

    /// Number of deltas in the reorg journal for a V2 pool. `0` when the id
    /// is unregistered or NOT a V2 pool — the per-family contract the PyO3
    /// wrapper's inline guard used to carry.
    #[must_use]
    pub fn v2_journal_len(&self, pool_id: u64) -> usize {
        let state = self
            .state
            .read_at(degenbot_substrate::state_lock::LockSite::Core);
        // Family guard: 0 for non-V2 (preserves the per-family contract).
        if state.get_v2_pool_state(pool_id).is_none() {
            0
        } else {
            state.pool_journal_len(pool_id).unwrap_or(0)
        }
    }

    /// Number of deltas in the reorg journal for a V3/V4 pool. `0` when the
    /// id is unregistered or not a concentrated-liquidity pool — the
    /// per-family contract.
    #[must_use]
    pub fn v3_journal_len(&self, pool_id: u64) -> usize {
        let state = self
            .state
            .read_at(degenbot_substrate::state_lock::LockSite::Core);
        // Family guard: 0 for non-V3/V4 (preserves the per-family contract).
        if state.get_v3_or_v4_pool(pool_id).is_none() {
            0
        } else {
            state.pool_journal_len(pool_id).unwrap_or(0)
        }
    }

    /// Discard V2 reorg journal deltas earlier than `block`. `Ok(())` when
    /// the id is unregistered or not a V2 pool — the V2 no-op contract — and
    /// a [`degenbot_pools::state_history::JournalError`] when the target is
    /// past the newest delta (it would remove every known state).
    pub fn v2_discard_before_block(
        &self,
        pool_id: u64,
        block: u64,
    ) -> Result<(), degenbot_pools::state_history::JournalError> {
        let mut state = self
            .state
            .write_at(degenbot_substrate::state_lock::LockSite::Core);
        // Family guard: no-op for non-V2 (V2's contract).
        if state.get_v2_pool_state(pool_id).is_none() {
            return Ok(());
        }
        state
            .discard_pool_before_block(pool_id, block)
            .unwrap_or(Ok(()))
    }

    /// Discard V3 reorg journal deltas earlier than `block` — the V3 twin of
    /// [`Bot::v2_discard_before_block`]: non-CL ids are `Ok(())` no-ops.
    pub fn v3_discard_before_block(
        &self,
        pool_id: u64,
        block: u64,
    ) -> Result<(), degenbot_pools::state_history::JournalError> {
        let mut state = self
            .state
            .write_at(degenbot_substrate::state_lock::LockSite::Core);
        // Family guard: no-op for non-V3/V4 (V3's contract).
        if state.get_v3_or_v4_pool(pool_id).is_none() {
            return Ok(());
        }
        state
            .discard_pool_before_block(pool_id, block)
            .unwrap_or(Ok(()))
    }

    /// Restore a V2 pool's state to the landed-at state strictly before
    /// `block` (ADR-005 slice 4). Returns the post-restore
    /// `(reserve0, reserve1, update_block)` — the read-after-restore contract
    /// (ADR-016): the restore trait returns `()`, and the post-restore fields
    /// ARE the before-values. `Ok(None)` when the id is unregistered or not a
    /// V2 pool (the no-op contract); [`JournalError`](degenbot_pools::state_history::JournalError)
    /// when the target is at/before the registration delta (too deep).
    pub fn v2_restore_before_block(
        &self,
        pool_id: u64,
        block: u64,
    ) -> Result<
        Option<(alloy::primitives::U256, alloy::primitives::U256, u64)>,
        degenbot_pools::state_history::JournalError,
    > {
        let mut state = self
            .state
            .write_at(degenbot_substrate::state_lock::LockSite::Core);
        // Family guard: `Ok(None)` for non-V2 (the PyO3 no-op contract).
        if state.get_v2_pool_state(pool_id).is_none() {
            return Ok(None);
        }
        match state.restore_pool_before_block(pool_id, block) {
            None => Ok(None),
            Some(Err(e)) => Err(e),
            Some(Ok(())) => {
                #[expect(clippy::expect_used)] // invariant-guarded (family confirmed above)
                let s = state
                    .get_v2_pool_state(pool_id)
                    .expect("V2 pool confirmed above");
                Ok(Some((
                    s.reserve0.to::<alloy::primitives::U256>(),
                    s.reserve1.to::<alloy::primitives::U256>(),
                    s.update_block,
                )))
            }
        }
    }

    /// Restore a V3/V4 pool's state to the landed-at state strictly before
    /// `block` (ADR-005 slice 4). Returns the post-restore
    /// `(sqrt_price_x96, liquidity, tick, update_block)` — the
    /// read-after-restore contract (ADR-016 D4). `Ok(None)` when the id is
    /// unregistered or not a CL pool; the CL journal panics on an empty
    /// journal, so a caller must pre-check
    /// [`has_state_prior_to`](Self::has_state_prior_to) — same discipline as
    /// [`restore_pool_before_block`](Self::restore_pool_before_block).
    pub fn v3_restore_before_block(
        &self,
        pool_id: u64,
        block: u64,
    ) -> Result<
        Option<(alloy::primitives::U256, u128, i32, u64)>,
        degenbot_pools::state_history::JournalError,
    > {
        let mut state = self
            .state
            .write_at(degenbot_substrate::state_lock::LockSite::Core);
        // Family guard: `Ok(None)` for non-V3/V4 (the PyO3 no-op contract).
        if state.get_v3_or_v4_pool(pool_id).is_none() {
            return Ok(None);
        }
        match state.restore_pool_before_block(pool_id, block) {
            None => Ok(None),
            Some(Err(e)) => Err(e),
            Some(Ok(())) => {
                #[expect(clippy::expect_used)] // invariant-guarded (family confirmed above)
                let s = state
                    .get_v3_or_v4_pool(pool_id)
                    .expect("V3/V4 pool confirmed above");
                Ok(Some((
                    s.sqrt_price_x96(),
                    s.liquidity(),
                    s.tick(),
                    s.update_block(),
                )))
            }
        }
    }

    /// Encode a V2 swap call shaped for submission:
    /// `(to_address_hex, calldata_hex, value)`. `Ok(None)` when the pool id
    /// is not registered (the Python facade's no-op contract); a typed
    /// [`degenbot_substrate::EncodeSwapError`] otherwise (e.g. a registered
    /// family with no swap-call encoder).
    pub fn encode_swap(
        &self,
        pool_id: u64,
        zero_for_one: bool,
        amount_out: alloy::primitives::U256,
        recipient: alloy::primitives::Address,
    ) -> Result<Option<(String, String, u64)>, degenbot_substrate::EncodeSwapError> {
        let call = match self
            .state
            .read_at(degenbot_substrate::state_lock::LockSite::Core)
            .encode_swap(pool_id, zero_for_one, amount_out, recipient)
        {
            Ok(call) => call,
            // NotRegistered maps to the no-op the Python facade returns.
            Err(degenbot_substrate::EncodeSwapError::NotRegistered { .. }) => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(Some((
            format!("{:#x}", call.to),
            format!("0x{}", alloy::hex::encode(call.data)),
            call.value.to::<u64>(),
        )))
    }

    /// Start the block pump. Placeholder — the `BlockPump` wiring lands in
    /// ADR-006 slice 5; until then this panics to make the unwired state loud.
    #[expect(clippy::unimplemented)] // deliberate until ADR-006 slice 5 wires BlockPump
    pub fn start(&self) {
        unimplemented!("BlockPump wiring lands in ADR-006 slice 5");
    }
}

#[expect(clippy::expect_used)]
#[cfg(test)]
mod tests {
    use crate::bot_core::RegisterV2PoolParams;
    use alloy::primitives::{aliases::U112, Address};

    /// The orchestrator carries `chain_id` (ADR-006 D1) and shares one
    /// `BotState` across `state_arc()` clones — N handles reach one
    /// Rust-owned state (the Polars three-layer invariant, preserved). This is
    /// the lone `Bot`-facade test; `Bot` is a thin deep interface over
    /// `BotState` (ADR-006 D4), so its behaviour is covered by the
    /// `BotState`-side registration/apply/reorg tests + the `PyBot` integration
    /// tests that construct `Bot::new` + read `state_arc()`.
    #[test]
    fn bot_facade_holds_chain_id_and_shares_bot_state() {
        // The orchestrator carries the chain id (D1).
        let bot = super::Bot::new(5);
        assert_eq!(bot.chain_id(), 5);

        // `state_arc()` hands out the shared `Arc<RwLock<BotState>>`.
        let state = bot.state_arc();

        // A pool registered through the shared state is visible to a SECOND
        // clone of the same Arc — proving N handles reach one Rust-owned
        // state (the Polars three-layer invariant, preserved).
        let params = RegisterV2PoolParams {
            address: Address::from([0x11u8; 20]),
            token0: Address::from([0x01u8; 20]),
            token1: Address::from([0x02u8; 20]),
            reserve0: U112::from(1000),
            reserve1: U112::from(2000),
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            factory: Address::from([0x33u8; 20]),
            update_block: 0,
            variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
            stable_swap: false,
            fee_denominator: None,
            ..Default::default()
        };
        state
            .write_at(degenbot_substrate::state_lock::LockSite::Core)
            .register_v2_pool(&params)
            .expect("test setup: V2 registration");

        let state2 = bot.state_arc();
        assert_eq!(
            state2
                .read_at(degenbot_substrate::state_lock::LockSite::Core)
                .pool_count(),
            1,
            "state_arc() must share one BotState"
        );
        assert!(state2
            .read_at(degenbot_substrate::state_lock::LockSite::Core)
            .has_pool(1));
    }

    /// The orchestrator owns the session's ONE object registry and hands out
    /// shared handles to it, so two consumers (the engine, a strategy) join
    /// the same canonical object instead of keeping private identity maps. The
    /// registry is identity only: registering an object writes no live state.
    #[test]
    fn session_registry_is_per_bot_shared_and_identity_only() {
        use degenbot_substrate::session_registry::{ObjectRefusal, PoolIdentity};
        use std::sync::Arc;

        let bot = super::Bot::new(5);
        let registry = bot.session_registry();
        assert_eq!(registry.chain_id(), 5, "the registry is session-scoped");

        let identity = PoolIdentity::v2(Address::from([0x44u8; 20]));
        assert!(matches!(
            registry.resolve_pool(&identity),
            Err(ObjectRefusal::UnknownPoolIdentity { .. })
        ));
        let engine = registry.get_or_create_pool(identity.clone());
        let strategy = bot.session_registry().get_or_create_pool(identity.clone());
        assert!(
            Arc::ptr_eq(&engine, &strategy),
            "two consumers share the session's one canonical object"
        );
        assert_eq!(registry.pool_count(), 1);
        assert_eq!(
            bot.state_arc()
                .read_at(degenbot_substrate::state_lock::LockSite::Core)
                .pool_count(),
            0,
            "an object registration writes no live state"
        );

        let other_session = super::Bot::new(5).session_registry();
        assert!(
            !Arc::ptr_eq(&engine, &other_session.get_or_create_pool(identity)),
            "identity is per session, not process-wide"
        );
    }

    /// The adoption seam joins the session's ONE registry instead of minting a
    /// second one: two `Bot`s constructed over the same shared core and the
    /// same session registry observe the same canonical object, so an adopting
    /// consumer cannot fork the session's identity key space. The session root
    /// (`Bot::new`) is the only path that mints, so a genuinely separate
    /// session still gets its own key space.
    #[test]
    fn adopting_bots_over_one_core_share_the_session_registry() {
        use degenbot_substrate::session_registry::{PoolIdentity, SessionObjectRegistry};
        use degenbot_substrate::BotState;
        use std::sync::Arc;

        let core = Arc::new(degenbot_substrate::state_lock::StateLock::new(
            BotState::new(),
        ));
        let registry = Arc::new(SessionObjectRegistry::new(7));
        let first = super::Bot::with_core(Arc::clone(&core), Arc::clone(&registry));
        let second = super::Bot::with_core(Arc::clone(&core), Arc::clone(&registry));

        assert_eq!(
            first.chain_id(),
            7,
            "an adopted Bot reports its session registry's chain, never a placeholder"
        );
        assert_eq!(second.chain_id(), first.chain_id());
        assert!(
            Arc::ptr_eq(&first.state_arc(), &second.state_arc()),
            "both adopters share the one core, as before"
        );
        assert!(
            Arc::ptr_eq(&first.session_registry(), &second.session_registry()),
            "adoption joins the session's one registry rather than minting a second"
        );

        let identity = PoolIdentity::v3(Address::from([0x66u8; 20]));
        let via_first = first
            .session_registry()
            .get_or_create_pool(identity.clone());
        let via_second = second.session_registry().get_or_create_pool(identity);
        assert!(
            Arc::ptr_eq(&via_first, &via_second),
            "two adopting Bots observe the same canonical object handle"
        );
        assert_eq!(registry.pool_count(), 1, "one entry for the shared session");

        // A session ROOT is the only way to get a fresh key space, and it
        // brings its own core — not a second identity space over this one.
        let other_session = super::Bot::new(7);
        assert!(
            !Arc::ptr_eq(
                &via_first,
                &other_session
                    .session_registry()
                    .get_or_create_pool(PoolIdentity::v3(Address::from([0x66u8; 20])))
            ),
            "a new session root has its own identity key space"
        );
    }

    // === Journal/state-read cluster moved onto the facade (ergo 2IIHQA) ===
    // The PyO3 shell (`degenbot-python/bot/mod.rs`) keeps only arg parsing +
    // error mapping; the logic these tests pin lives on `Bot`.

    /// A V2 registration fixture scoped to this cluster's tests.
    fn v2_fixture(address: Address) -> crate::bot_core::RegisterV2PoolParams {
        crate::bot_core::RegisterV2PoolParams {
            address,
            token0: Address::from([0x01u8; 20]),
            token1: Address::from([0x02u8; 20]),
            reserve0: U112::from(1000),
            reserve1: U112::from(2000),
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            factory: Address::from([0x33u8; 20]),
            update_block: 10,
            variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
            stable_swap: false,
            fee_denominator: None,
            ..Default::default()
        }
    }

    /// A V3 registration fixture scoped to this cluster's tests (no genesis
    /// journal delta — the CL family journals from the first event).
    fn v3_fixture(address: Address) -> crate::bot_core::RegisterV3PoolParams {
        crate::bot_core::RegisterV3PoolParams {
            address,
            token0: Address::from([0x01u8; 20]),
            token1: Address::from([0x02u8; 20]),
            fee: 3_000,
            tick_spacing: 60,
            factory: Address::from([0x33u8; 20]),
            sqrt_price_x96: alloy::primitives::U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: hashbrown::HashMap::new(),
            update_block: 100,
            tick_data_block: None,
            coverage: crate::bot_core::PoolTickCoverage::Sparse,
            fetcher: None,
            ..Default::default()
        }
    }

    /// The facade answers registry reads (`pool_count` / `has_pool` /
    /// `has_token`) without the caller reaching into `BotState` directly.
    #[test]
    fn bot_facade_answers_registry_reads() {
        let bot = super::Bot::new(5);
        assert_eq!(bot.pool_count(), 0);
        assert!(!bot.has_pool(1));

        let pool_id = bot
            .state_arc()
            .write_at(degenbot_substrate::state_lock::LockSite::Core)
            .register_v2_pool(&v2_fixture(Address::from([0x11u8; 20])))
            .expect("test setup: V2 registration");

        assert_eq!(bot.pool_count(), 1);
        assert!(bot.has_pool(pool_id));
        assert!(!bot.has_pool(999));

        let token = Address::from([0x77u8; 20]);
        bot.state_arc()
            .write_at(degenbot_substrate::state_lock::LockSite::Core)
            .register_token(token, "T".to_string(), "T".to_string(), 18, 5);
        assert!(bot.has_token(&token));
        assert!(!bot.has_token(&Address::from([0x78u8; 20])));
    }

    /// Journal-length reads carry the per-family contract: 0 for unregistered
    /// ids and for the WRONG family (the guard the PyO3 wrapper used to
    /// inline), the live journal length for the right one. V2 registration
    /// pushes a genesis delta; V3 registration pushes none.
    #[test]
    fn journal_len_reads_carry_the_family_guard() {
        let bot = super::Bot::new(5);
        let (v2_id, v3_id) = {
            let core = bot.state_arc();
            let mut state = core.write_at(degenbot_substrate::state_lock::LockSite::Core);
            let v2_id = state
                .register_v2_pool(&v2_fixture(Address::from([0x11u8; 20])))
                .expect("test setup: V2 registration");
            let v3_id = state
                .register_v3_pool(&v3_fixture(Address::from([0x22u8; 20])))
                .expect("test setup: V3 registration");
            (v2_id, v3_id)
        };

        assert_eq!(bot.v2_journal_len(v2_id), 1, "V2 genesis delta");
        assert_eq!(bot.v2_journal_len(999), 0, "unregistered id");
        assert_eq!(
            bot.v3_journal_len(v2_id),
            0,
            "family guard: V2 id on V3 read"
        );
        assert_eq!(
            bot.v2_journal_len(v3_id),
            0,
            "family guard: V3 id on V2 read"
        );
        assert_eq!(bot.v3_journal_len(v3_id), 0, "V3 pushes no genesis delta");

        // A Swap update journals the priors, so the count becomes 1.
        bot.state_arc()
            .write_at(degenbot_substrate::state_lock::LockSite::Core)
            .update_v3_pool(
                Address::from([0x22u8; 20]),
                alloy::primitives::U256::from(2u128) << 96,
                2_000_000,
                60,
                200,
                vec![],
            );
        assert_eq!(bot.v3_journal_len(v3_id), 1);
    }

    /// V2 restore/discard through the facade: restore returns the
    /// read-after-restore scalars (ADR-016 — the post-restore fields ARE the
    /// landed-at values), errors on a too-deep target, and no-ops
    /// (`Ok(None)` / `Ok(())`) off-family or unregistered.
    #[test]
    fn v2_discard_and_restore_before_block() {
        let bot = super::Bot::new(5);
        let (v2_id, v3_id) = {
            let core = bot.state_arc();
            let mut state = core.write_at(degenbot_substrate::state_lock::LockSite::Core);
            let v2_id = state
                .register_v2_pool(&v2_fixture(Address::from([0x11u8; 20])))
                .expect("test setup: V2 registration");
            let v3_id = state
                .register_v3_pool(&v3_fixture(Address::from([0x22u8; 20])))
                .expect("test setup: V3 registration");
            (v2_id, v3_id)
        };

        // Apply a Sync at block 20 (journals the genesis reserves, lands new).
        assert_eq!(
            bot.state_arc()
                .write_at(degenbot_substrate::state_lock::LockSite::Core)
                .apply_sync_by_pool_id(v2_id, U112::from(1500), U112::from(2500), 20),
            Some(v2_id)
        );

        // Restore to just before block 20: the genesis landed-at state.
        assert_eq!(
            bot.v2_restore_before_block(v2_id, 20),
            Ok(Some((
                alloy::primitives::U256::from(1000u64),
                alloy::primitives::U256::from(2000u64),
                10,
            ))),
            "read-after-restore: the post-restore reserves ARE the priors"
        );
        assert_eq!(
            bot.v2_journal_len(v2_id),
            1,
            "the block-20 delta was popped"
        );

        // A target at/before the genesis delta is too deep: typed error.
        assert_eq!(
            bot.v2_restore_before_block(v2_id, 5),
            Err(degenbot_pools::state_history::JournalError::NoStatePriorToBlock { block: 5 })
        );

        // Off-family / unregistered: Ok(None) — the PyO3 no-op contract.
        assert_eq!(bot.v2_restore_before_block(999, 5), Ok(None));
        assert_eq!(bot.v2_restore_before_block(v3_id, 5), Ok(None));

        // Discard trims deltas earlier than the target, errors past the newest.
        assert_eq!(
            bot.state_arc()
                .write_at(degenbot_substrate::state_lock::LockSite::Core)
                .apply_sync_by_pool_id(v2_id, U112::from(1800), U112::from(2800), 20),
            Some(v2_id)
        );
        bot.v2_discard_before_block(v2_id, 15)
            .expect("discard within the journal");
        assert_eq!(
            bot.v2_journal_len(v2_id),
            1,
            "the genesis delta at block 10 was trimmed"
        );
        assert_eq!(
            bot.v2_discard_before_block(v2_id, 100),
            Err(degenbot_pools::state_history::JournalError::NoStateAtOrAfterBlock { block: 100 })
        );

        // Off-family / unregistered discards are Ok no-ops.
        bot.v2_discard_before_block(999, 15)
            .expect("unregistered id is a no-op");
        bot.v2_discard_before_block(v3_id, 15)
            .expect("off-family id is a no-op");
    }

    /// V3 restore through the facade: the post-restore scalars are the priors
    /// the popped delta carried (read-after-restore, ADR-016 D4), and the
    /// family guard returns `Ok(None)` off-family / unregistered.
    #[test]
    fn v3_restore_before_block_returns_the_popped_priors() {
        let bot = super::Bot::new(5);
        let (v2_id, v3_id) = {
            let core = bot.state_arc();
            let mut state = core.write_at(degenbot_substrate::state_lock::LockSite::Core);
            let v2_id = state
                .register_v2_pool(&v2_fixture(Address::from([0x11u8; 20])))
                .expect("test setup: V2 registration");
            let v3_id = state
                .register_v3_pool(&v3_fixture(Address::from([0x22u8; 20])))
                .expect("test setup: V3 registration");
            (v2_id, v3_id)
        };

        bot.state_arc()
            .write_at(degenbot_substrate::state_lock::LockSite::Core)
            .update_v3_pool(
                Address::from([0x22u8; 20]),
                alloy::primitives::U256::from(2u128) << 96,
                2_000_000,
                60,
                200,
                vec![],
            );

        assert_eq!(
            bot.v3_restore_before_block(v3_id, 200),
            Ok(Some((
                alloy::primitives::U256::from(1u128) << 96,
                1_000_000,
                0,
                100,
            ))),
            "read-after-restore: registration-state priors"
        );
        assert_eq!(bot.v3_restore_before_block(999, 200), Ok(None));
        assert_eq!(bot.v3_restore_before_block(v2_id, 200), Ok(None));
    }

    /// `encode_swap` through the facade: the submission-shaped
    /// `(to, calldata, value)` triple on a hit, `Ok(None)` for an
    /// unregistered id, and a typed `UnsupportedFamily` refusal off-family.
    /// (Byte-level calldata pinning lives in the Python seam's eth_abi
    /// oracle tests — tests/arbitrage/test_solvers/test_py_bot.py.)
    #[test]
    fn encode_swap_shapes_the_submission_call() {
        let bot = super::Bot::new(5);
        let (v2_id, v3_id) = {
            let core = bot.state_arc();
            let mut state = core.write_at(degenbot_substrate::state_lock::LockSite::Core);
            let v2_id = state
                .register_v2_pool(&v2_fixture(Address::from([0x11u8; 20])))
                .expect("test setup: V2 registration");
            let v3_id = state
                .register_v3_pool(&v3_fixture(Address::from([0x22u8; 20])))
                .expect("test setup: V3 registration");
            (v2_id, v3_id)
        };

        let (to, calldata, value) = bot
            .encode_swap(
                v2_id,
                true,
                alloy::primitives::U256::from(181),
                Address::from([0x55u8; 20]),
            )
            .expect("registered V2 encodes")
            .expect("pool is registered");
        assert_eq!(to, format!("{:#x}", Address::from([0x11u8; 20])));
        assert!(
            calldata.starts_with("0x022c0d9f"),
            "V2 swap selector: {calldata}"
        );
        assert_eq!(value, 0, "no ETH sent");

        assert!(
            matches!(
                bot.encode_swap(999, true, alloy::primitives::U256::from(1), Address::ZERO),
                Ok(None)
            ),
            "unregistered id means None"
        );
        assert!(
            matches!(
                bot.encode_swap(v3_id, true, alloy::primitives::U256::from(1), Address::ZERO),
                Err(degenbot_substrate::EncodeSwapError::UnsupportedFamily { .. })
            ),
            "V3 has no swap-call encoder"
        );
    }

    /// `build_rpc_log` (moved from the PyO3 shell) reconstructs the WS-log
    /// shape `(address, topics, data, block_number)` into the
    /// `alloy::rpc::types::Log` the dispatcher consumes, with the parse
    /// refusals the shell mapped to `ValueError`.
    #[test]
    fn build_rpc_log_reconstructs_the_ws_log_shape() {
        let log = crate::bot_core::build_rpc_log(
            "0xaAaAaAaaAaAaAaaAaAAAAAAAAaaaAaAaAaaAaaAa",
            vec![
                "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef".to_string(),
                "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd".to_string(),
            ],
            "abcdef",
            42,
        )
        .expect("valid ws log shape");
        assert_eq!(
            log.inner.address,
            "0xaAaAaAaaAaAaAaaAaAAAAAAAAaaaAaAaAaaAaaAa"
                .parse::<alloy::primitives::Address>()
                .expect("parse")
        );
        assert_eq!(log.inner.topics().len(), 2);
        assert_eq!(
            log.inner.data.data,
            alloy::primitives::Bytes::from(vec![0xab, 0xcd, 0xef])
        );
        assert_eq!(log.block_number, Some(42));
        assert!(!log.removed);

        let err = crate::bot_core::build_rpc_log("nothex", vec![], "0x", 1).unwrap_err();
        assert!(err.contains("Invalid address"), "{err}");
        let err = crate::bot_core::build_rpc_log(
            "0xaAaAaAaaAaAaAaaAaAAAAAAAAaaaAaAaAaaAaaAa",
            vec!["zz".to_string()],
            "0x",
            1,
        )
        .unwrap_err();
        assert!(err.contains("Invalid topic"), "{err}");
        let err = crate::bot_core::build_rpc_log(
            "0xaAaAaAaaAaAaAaaAaAAAAAAAAaaaAaAaAaaAaaAa",
            vec![],
            "zz",
            1,
        )
        .unwrap_err();
        assert!(err.contains("Invalid data hex"), "{err}");
    }
}
