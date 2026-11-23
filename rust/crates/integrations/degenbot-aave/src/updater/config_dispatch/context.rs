//! The per-chunk config-dispatch context.
//!
//! [`ChunkContext`] owns the chunk-scoped dispatch constants (provider,
//! market/chain ids, Pool + oracle addresses) plus the block-pinned
//! [`RevisionMemo`], a `(implementation, selector, block) → revision` cache
//! over the 4 revision reads, so repeated same-implementation probes inside
//! one chunk issue one RPC. The context borrows its [`ChunkSpan`] and the
//! [`ChunkSubstrate`] and takes no `Connection` at all, so it cannot outlive
//! the chunk that built it — the block-pinned staleness rule is enforced by
//! construction, not by convention — and every dispatch arm takes the single
//! context instead of the exploded id + substrate list.
//!
//! [`ChunkContext::dispatch_transaction`] is the chunk overlay's ONE
//! apply→bump→dispatch seam, in the fixed order the read-your-own-writes
//! contract requires: re-read the overlay's GHO vToken revision (a prior
//! log-transaction's `Upgraded` projection bumped the cached row), build the
//! discount snapshot off it, then dispatch the configuration events, each
//! reduced into a positioned delta and projected onto the substrate overlay
//! (the chunk's one write transaction persists them).
//!
//! Surface table: `tests/fixtures/cassettes/wave4/UPGRADE-MAP.md` §(A)
//! row 3 (the memo key MUST include the block lane), row 7 (stage 1 of
//! `dispatch_transaction` re-resolves the GHO vToken revision per
//! transaction — a chunk-start snapshot is the mutated-replay shape),
//! and row 11 (one memo instance per phase; the cold-boot bootstrap pass in
//! `run::fetch` builds its own [`RevisionMemo`] and calls
//! [`super::revision::match_proxy_id`] with it).

use std::collections::HashMap;

#[cfg(test)]
use crate::projection::DeltaEffect;
use crate::projection::{CanonicalDelta, ConfigurationEffect, ProjectionView};
#[cfg(test)]
use crate::run::CanonicalDeltaWriteReport;
use crate::updater::run::substrate::{ChunkSubstrate, ASSET_KIND_V_TOKEN};
use alloy::primitives::{Address, U256};
use degenbot_core::canonical_head::{ChainAnchor, TxRef};
use degenbot_db::aave::AaveGhoAsset;
use degenbot_db::DbError;
use degenbot_rpc::chain_evidence::{resolve_parent_anchor, BlockReads};
use degenbot_rpc::provider::AlloyProvider;
#[cfg(test)]
use rusqlite::Connection;

use super::build_discount_snapshot;
use super::dispatch_config_events;
use super::revision::{match_proxy_id, ProxyCreationResolution};
use super::revision_selector;
use super::{
    encode_no_arg_call, encode_single_address_call, require_word0_u256, ConfigDispatchError,
};

// ── the revision memo ──────────────────────────────────────────────────────

/// Per-run, block-pinned memo over the config dispatch's revision reads
/// (`ATOKEN_REVISION()` / `DEBT_TOKEN_REVISION()` / `POOL_REVISION()` /
/// `CONFIGURATOR_REVISION()`).
///
/// The key is `(implementation, selector, ChainAnchor)`: a revision is chain
/// state at one exact block, so one triple can never observe two values within a
/// run — a memo hit is definitionally the same read. The anchor lane is what
/// makes an `Upgraded` chunk safe: the upgrade dispatch reads the NEW
/// implementation at the upgrade block, and every later read sits at a later
/// block (its own key), so no read can be served a pre-upgrade value across the
/// boundary. A same-height branch is a different anchor, so it is a different
/// key: no hit can cross an abandoned hash (ADR-105 D1, D3).
///
/// Lifetime: one instance per chunk resolution (`ChunkResolver`), which is the
/// whole read span the dispatch can observe — chunks partition the block range,
/// so no cross-chunk key collision exists, and the memo never outlives the run
/// (a plain local, not global state).
#[derive(Debug, Default)]
pub(crate) struct RevisionMemo {
    entries: HashMap<(Address, [u8; 4], ChainAnchor), U256>,
}

impl RevisionMemo {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The memo hit for one revision-read key, if any.
    pub(crate) fn get(
        &self,
        target: &Address,
        selector: [u8; 4],
        anchor: ChainAnchor,
    ) -> Option<U256> {
        self.entries.get(&(*target, selector, anchor)).copied()
    }

    /// Record one revision read (the value the chain answered at `anchor`).
    pub(crate) fn insert(
        &mut self,
        target: &Address,
        selector: [u8; 4],
        anchor: ChainAnchor,
        value: U256,
    ) {
        self.entries.insert((*target, selector, anchor), value);
    }

    /// The memoized [`read_uint256_return`]: one RPC per
    /// `(implementation, selector, block)` per run, identical results after.
    ///
    /// # Errors
    ///
    /// Propagates the underlying `eth_call` failure on a memo miss.
    pub(crate) async fn read(
        &mut self,
        reads: &BlockReads<'_>,
        target: &Address,
        sig: &str,
    ) -> Result<U256, ConfigDispatchError> {
        let anchor = reads.anchor();
        let selector = revision_selector(sig);
        if let Some(v) = self.get(target, selector, anchor) {
            return Ok(v);
        }
        let v = read_uint256_return(reads, target, sig).await?;
        self.insert(target, selector, anchor, v);
        Ok(v)
    }
}

// ── the chunk's parent read positions ─────────────────────────────────────

/// The chunk's resolved parent read positions, keyed by the child anchor.
///
/// An intentional pre-block observation (the stkAAVE `balanceOf` at the block
/// before the transaction that acts on it) reads at the child's PARENT, and a
/// parent is an exact identity: one header read per distinct child anchor,
/// validated against that child's own parent hash. The memo makes the shared
/// case one read per chunk while keeping every position branch-exact.
#[derive(Debug, Default)]
pub(crate) struct ParentAnchors {
    entries: HashMap<ChainAnchor, ChainAnchor>,
}

impl ParentAnchors {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The exact anchor of `anchor`'s parent block.
    ///
    /// # Errors
    ///
    /// [`degenbot_rpc::chain_evidence::ParentAnchorError`] for a genesis block,
    /// a missing parent header, a parent on another chain, or a same-height
    /// substitution; the read failure itself propagates.
    pub(crate) async fn of(
        &mut self,
        provider: &AlloyProvider,
        anchor: ChainAnchor,
    ) -> Result<ChainAnchor, ConfigDispatchError> {
        if let Some(known) = self.entries.get(&anchor) {
            return Ok(*known);
        }
        let parent = resolve_parent_anchor(provider, anchor).await?;
        self.entries.insert(anchor, parent);
        Ok(parent)
    }
}

// ── the per-chunk dispatch context ─────────────────────────────────────────

/// The block span one chunk apply covers (`[chunk_start, chunk_end]`).
///
/// A [`ChunkContext`] borrows one of these, which is what makes the
/// block-pinned revision rule structural rather than a comment-enforced
/// convention: a context can only be built inside a chunk's span and cannot
/// be carried across a chunk boundary.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ChunkSpan {
    chunk_start: u64,
    chunk_end: u64,
}

impl ChunkSpan {
    /// The `[chunk_start, chunk_end]` span one chunk apply covers.
    pub(crate) fn new(chunk_start: u64, chunk_end: u64) -> Self {
        Self {
            chunk_start,
            chunk_end,
        }
    }

    /// Whether `block_number` lies in this span. The block lane of the memo
    /// key is meaningful only for reads the chunk actually covers; a read
    /// outside the span is a caller error, not a cache miss.
    fn contains(self, block_number: u64) -> bool {
        self.chunk_start <= block_number && block_number <= self.chunk_end
    }
}

/// A whole-range [`ChunkSpan`] for the in-crate unit tests: one shared
/// instance the test-built contexts borrow, so a single-block fixture sits
/// inside any test's span.
#[cfg(test)]
pub(super) fn whole_range_span() -> &'static ChunkSpan {
    use std::sync::OnceLock;
    static SPAN: OnceLock<ChunkSpan> = OnceLock::new();
    SPAN.get_or_init(|| ChunkSpan::new(0, u64::MAX))
}

/// The per-chunk config-dispatch context: the provider, the chunk-scoped
/// dispatch constants, the [`ChunkSubstrate`], and the block-pinned
/// [`RevisionMemo`], in ONE owner with a single lifetime.
///
/// Built by [`ChunkContext::for_chunk`] at the top of a chunk resolution and
/// threaded by `&mut` through the dispatch. The constructor borrows a
/// [`ChunkSpan`] for the whole context lifetime, so the context cannot outlive
/// or cross a chunk boundary — the memo's block-pinned staleness rule (an
/// `Upgraded` in one chunk must never serve a pre-upgrade revision to a later
/// chunk) is enforced by construction, not by convention.
pub(crate) struct ChunkContext<'span> {
    span: &'span ChunkSpan,
    provider: AlloyProvider,
    market_id: i64,
    chain_id: i64,
    pool_address: Address,
    memo: RevisionMemo,
    substrate: &'span mut ChunkSubstrate,
    tx: Option<TxRef>,
    block_number: u64,
    parent_anchors: ParentAnchors,
    /// The stkAAVE balance observations resolution resolved out-of-band (a
    /// `balanceOf` at the exact parent block); the dispatch loop emits them as
    /// their own deltas before the transfer they seed.
    pending_observations: Vec<(i64, alloy::primitives::U256)>,
}

impl<'span> ChunkContext<'span> {
    /// Build the context for one chunk, borrowing the [`ChunkSubstrate`] for the
    /// context's whole lifetime.
    ///
    /// `block_number` starts at the span's first block; each log-transaction's
    /// dispatch enters its own block through [`ChunkContext::begin_transaction`]
    /// before any read or emitted event.
    #[cfg(test)]
    pub(crate) fn for_chunk(
        span: &'span ChunkSpan,
        provider: AlloyProvider,
        market_id: i64,
        chain_id: i64,
        pool_address: Address,
        substrate: &'span mut ChunkSubstrate,
    ) -> Self {
        Self::for_chunk_with_memo(
            span,
            provider,
            market_id,
            chain_id,
            pool_address,
            RevisionMemo::new(),
            substrate,
        )
    }

    /// Build the context with the chunk's already-populated revision memo, so
    /// the reserve-identity reads the seed acquired and the dispatch's own
    /// revision reads share one block-pinned memo.
    pub(crate) fn for_chunk_with_memo(
        span: &'span ChunkSpan,
        provider: AlloyProvider,
        market_id: i64,
        chain_id: i64,
        pool_address: Address,
        memo: RevisionMemo,
        substrate: &'span mut ChunkSubstrate,
    ) -> Self {
        Self {
            span,
            provider,
            market_id,
            chain_id,
            pool_address,
            memo,
            substrate,
            tx: None,
            block_number: span.chunk_start,
            parent_anchors: ParentAnchors::new(),
            pending_observations: Vec::new(),
        }
    }

    /// Enter one log-transaction's dispatch scope: the exact transaction every
    /// read and every emitted delta for that transaction rides.
    pub(crate) fn begin_transaction(&mut self, tx: TxRef) {
        self.tx = Some(tx);
        self.block_number = tx.anchor().number();
    }

    /// The current transaction's exact reference.
    ///
    /// # Errors
    ///
    /// `ConfigDispatchError::DecodeShape` when a dispatch read arrives before
    /// any transaction scope was entered.
    pub(super) fn tx(&self) -> Result<TxRef, ConfigDispatchError> {
        self.tx.ok_or_else(|| {
            ConfigDispatchError::DecodeShape(
                "configuration dispatch outside a transaction scope".to_string(),
            )
        })
    }

    /// The exact anchor of the current transaction's block — the read position
    /// every configuration, revision, metadata and discount read of this
    /// transaction inherits.
    ///
    /// # Errors
    ///
    /// `ConfigDispatchError::DecodeShape` when a dispatch read arrives before any
    /// transaction scope was entered.
    pub(super) fn anchor(&self) -> Result<ChainAnchor, ConfigDispatchError> {
        let tx = self.tx()?;
        Ok(*tx.anchor())
    }

    /// The hash-pinned read view of one exact block.
    pub(super) fn reads_at(&self, anchor: ChainAnchor) -> BlockReads<'_> {
        BlockReads::at(&self.provider, anchor)
    }

    /// The hash-pinned read view of the current transaction's block.
    ///
    /// # Errors
    ///
    /// `ConfigDispatchError::DecodeShape` when a dispatch read arrives before any
    /// transaction scope was entered.
    pub(super) fn reads(&self) -> Result<BlockReads<'_>, ConfigDispatchError> {
        let anchor = self.anchor()?;
        Ok(self.reads_at(anchor))
    }

    /// The exact anchor of the block before the current transaction: the read
    /// position of an intentional pre-block observation.
    ///
    /// # Errors
    ///
    /// The parent resolution refusal (a same-height substitution is one), or the
    /// header read failure.
    pub(super) async fn parent_anchor(&mut self) -> Result<ChainAnchor, ConfigDispatchError> {
        let anchor = self.anchor()?;
        self.parent_anchors.of(&self.provider, anchor).await
    }

    /// Take the chunk's resolved parent positions, so the post-dispatch refresh
    /// keeps resolving into the same memo after the context is dropped.
    pub(crate) fn take_parent_anchors(&mut self) -> ParentAnchors {
        std::mem::take(&mut self.parent_anchors)
    }

    /// Take the chunk's revision memo, so the resolver keeps the block-pinned
    /// reads the dispatch answered after the context is dropped.
    pub(crate) fn take_memo(&mut self) -> RevisionMemo {
        std::mem::take(&mut self.memo)
    }

    /// One `balanceOf(user)` at the exact PARENT block of the current
    /// transaction — the intentional pre-block position the stkAAVE seed reads.
    ///
    /// # Errors
    ///
    /// The parent resolution refusal, the exact read failure, or a short return.
    pub(super) async fn read_stk_aave_balance_at_parent(
        &mut self,
        token: Address,
        user: Address,
    ) -> Result<U256, ConfigDispatchError> {
        let parent = self.parent_anchor().await?;
        let ret = self
            .reads_at(parent)
            .call(
                token,
                encode_single_address_call("balanceOf(address)", &user),
            )
            .await?;
        require_word0_u256(&ret, "balanceOf(address)", &token)
    }

    /// One log's exact position under the current transaction.
    ///
    /// # Errors
    ///
    /// `ConfigDispatchError::DecodeShape` when the log carries no block-wide
    /// index (canonical ingress already refuses one — this is the loud
    /// backstop) or the dispatch has no transaction scope.
    pub(super) fn log_ref(
        &self,
        log: &alloy::rpc::types::Log,
    ) -> Result<degenbot_core::canonical_head::LogRef, ConfigDispatchError> {
        let tx = self.tx()?;
        let index = log.log_index.ok_or_else(|| {
            ConfigDispatchError::DecodeShape(
                "configuration log carries no block-wide log index".to_string(),
            )
        })?;
        Ok(degenbot_core::canonical_head::LogRef::new(tx, index))
    }

    /// Enter a transaction scope by block number for the in-crate fixtures:
    /// one deterministic transaction per block, so a fixture's emitted deltas
    /// carry a stable exact position.
    #[cfg(test)]
    pub(super) fn begin_transaction_at(&mut self, block_number: u64) {
        self.begin_transaction(crate::updater::config_dispatch::fixture_tx(block_number));
    }

    /// Persist a configuration effect through the canonical writer for the
    /// in-crate fixtures (the production path reduces through the dispatch
    /// loop instead).
    #[cfg(test)]
    pub(super) fn apply_effects_for_test(
        &mut self,
        conn: &Connection,
        effects: &[ConfigurationEffect],
    ) -> Result<crate::run::CanonicalDeltaWriteReport, DbError> {
        let tx = self
            .tx()
            .map_err(|error| DbError::Decode(format!("{error}")))?;
        let mut report = crate::run::CanonicalDeltaWriteReport::default();
        for effect in effects {
            let delta = CanonicalDelta::new(
                degenbot_core::canonical_head::LogRef::new(tx, 0),
                None,
                DeltaEffect::Configuration(effect.clone()),
            );
            report = self.apply_configuration_delta(conn, &delta)?;
        }
        Ok(report)
    }

    /// Record one resolved stkAAVE balance observation for the dispatch loop to
    /// emit (and for the projection to read).
    pub(super) fn push_observation(&mut self, user_id: i64, balance: alloy::primitives::U256) {
        self.pending_observations.push((user_id, balance));
    }

    /// Drain the observations the current event resolved.
    pub(super) fn take_observations(&mut self) -> Vec<(i64, alloy::primitives::U256)> {
        std::mem::take(&mut self.pending_observations)
    }

    /// The block the current log-transaction's dispatch reads + emits at.
    pub(super) fn block_number(&self) -> u64 {
        self.block_number
    }

    /// The chunk substrate, for the callers that drive it directly (the
    /// per-transaction ops parser, the end-of-chunk flush, the stkAAVE
    /// backfill).
    pub(crate) fn substrate_mut(&mut self) -> &mut ChunkSubstrate {
        &mut *self.substrate
    }

    /// The generation a revision-changing effect is about to replace, read from
    /// the substrate's current position before the effect is projected.
    pub(crate) fn superseded_generation(
        &self,
        effect: &crate::projection::ConfigurationEffect,
    ) -> Option<crate::projection::GenerationRef> {
        self.substrate.superseded_generation(effect)
    }

    /// The GHO asset row at the current transaction's read position.
    ///
    /// This is the ONE freshness rule for the GHO asset: every GHO-row effect
    /// projects onto the substrate's row as it dispatches
    /// ([`ChunkSubstrate::project_gho`]), so a same-tx `ReserveInitialized` or
    /// `DiscountTokenUpdated` at an earlier logIndex is visible to every later
    /// read. A `None` here is the genuine "no GHO market row" state, never a
    /// stale snapshot.
    ///
    /// # Errors
    ///
    /// Propagates the substrate lookup failure.
    pub(crate) fn gho_asset(&mut self) -> Option<AaveGhoAsset> {
        self.substrate.gho_asset()
    }

    /// The chunk's interned reserve identity for an underlying token address.
    ///
    /// # Errors
    ///
    /// The seed refusal when the chunk never initialized the reserve.
    pub(crate) fn reserve(
        &self,
        underlying: &str,
    ) -> Result<
        crate::updater::run::seed::ChunkReserve,
        crate::updater::run::seed::IncompleteChunkSeed,
    > {
        self.substrate.reserve(underlying).cloned()
    }

    /// The market the dispatch resolves ids against.
    pub(super) fn market_id(&self) -> i64 {
        self.market_id
    }

    /// The chain the dispatch's erc20 + GHO rows belong to.
    pub(super) fn chain_id(&self) -> i64 {
        self.chain_id
    }

    /// The `POOL` proxy address (`getConfiguration` + the ops parser's
    /// scope). A same-chunk `AddressSet` POOL rotation updates it in place at
    /// the transition boundary, so a later transaction's RPC targets follow
    /// the replacement instead of the chunk-start snapshot.
    pub(crate) fn pool_address(&self) -> Address {
        self.pool_address
    }

    /// The chunk overlay's ONE apply seam: commit `events` to `conn` and let
    /// the apply arms record every cache-visible write into `substrate` (the
    /// `Upgraded` revision bump, the scaled-token balances, the GHO dirty
    /// mark) before any later read consults the overlay.
    ///
    /// The read-your-own-writes order is apply-then-read by construction,
    /// because this is the only place the chunk loop writes an event's effect
    /// to the overlay: the per-tx GHO revision re-resolve, the discount
    /// pre-pass, and the next log-transaction's dispatch all read the overlay
    /// the previous apply left behind. A refactor that dispatched a read ahead
    /// of the write it depends on would have to bypass this method.
    ///
    /// # Errors
    ///
    /// Propagates [`degenbot_db::DbError`] from any apply failure — the caller
    /// drops the chunk `Transaction` (rollback).
    /// Repoint the in-chunk POOL endpoint after an `AddressSet` rotation: the
    /// dispatch's own `getConfiguration` and the ops parser scope RPCs at the
    /// POOL proxy through this field, and the durable row moves in the chunk's
    /// write transaction.
    ///
    /// # Errors
    ///
    /// [`DbError::Decode`] when the rotation target is not an address.
    pub(super) fn repoint_pool(&mut self, effect: &ConfigurationEffect) -> Result<(), DbError> {
        if let ConfigurationEffect::ContractAddressChanged {
            name, new_address, ..
        } = effect
        {
            if name == "POOL" {
                self.pool_address = new_address.parse().map_err(|_| {
                    DbError::Decode(format!(
                        "POOL rotation target {new_address} is not an address"
                    ))
                })?;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn apply_configuration_delta(
        &mut self,
        conn: &Connection,
        delta: &CanonicalDelta,
    ) -> Result<CanonicalDeltaWriteReport, DbError> {
        let report = self.apply_deltas(conn, std::slice::from_ref(delta))?;
        // Same-transition RPC targets: the dispatch's own `getConfiguration`
        // + ops-parser scope RPCs the POOL proxy by this field, so an
        // `AddressSet` POOL rotation applied mid-chunk must repoint it at the
        // transition boundary (the DB row is the durable half; this is the
        // in-chunk half). The oracle target is NOT cached here —
        // `resolve_reserve_oracle_address` reads `conn` fresh per event.
        if let DeltaEffect::Configuration(ConfigurationEffect::ContractAddressChanged {
            name,
            new_address,
            ..
        }) = delta.effect()
        {
            if name == "POOL" {
                self.pool_address = new_address.parse().map_err(|_| {
                    DbError::Decode(format!(
                        "POOL rotation target {new_address} is not an address"
                    ))
                })?;
            }
        }
        Ok(report)
    }

    /// The operation family's apply seam: persist the transaction's canonical
    /// deltas through the same substrate overlay, so every written position
    /// state is visible to a later read exactly as the SQL probe would report
    /// it. Reduction already advanced the projection view; this call is the
    /// storage half, and it performs no resolution.
    ///
    /// # Errors
    ///
    /// Propagates [`degenbot_db::DbError`] from any apply failure — the caller
    /// drops the chunk `Transaction` (rollback).
    #[cfg(test)]
    pub(crate) fn apply_deltas(
        &mut self,
        conn: &Connection,
        deltas: &[crate::projection::CanonicalDelta],
    ) -> Result<crate::run::CanonicalDeltaWriteReport, DbError> {
        crate::run::apply_canonical_deltas_on_conn(conn, deltas, &mut *self.substrate)
    }

    /// Read `sig` on `target` at `block_number` through the memo — the one seam
    /// every revision read in the dispatch goes through, so the memo's block
    /// lane is applied at exactly the reads that need it.
    ///
    /// # Errors
    ///
    /// Propagates the underlying `eth_call` failure on a memo miss.
    pub(super) async fn read_revision(
        &mut self,
        target: &Address,
        sig: &str,
    ) -> Result<U256, ConfigDispatchError> {
        let anchor = self.anchor()?;
        debug_assert!(
            self.span.contains(anchor.number()),
            "chunk-dispatch revision read at a block outside the chunk span"
        );
        let reads = BlockReads::at(&self.provider, anchor);
        self.memo.read(&reads, target, sig).await
    }

    /// Run one log-transaction's overlay-mediated read + dispatch stages in
    /// the ONE order the read-your-own-writes contract requires.
    ///
    /// The sequence is fixed here and nowhere else:
    ///
    /// 1. **re-read the overlay's GHO vToken revision** — a prior log-
    ///    transaction's `Upgraded` apply bumped the cached row, and this read
    ///    must see the bump (a chunk-start snapshot would not).
    /// 2. **build the discount snapshot** — the GHO-discount pre-pass, gated
    ///    on the revision just read, against the TRANSACTION-START GHO row
    ///    (a same-tx link is invisible to it by design).
    /// 3. **dispatch the configuration events** — each is reduced into a
    ///    positioned, generation-qualified delta and projected onto the
    ///    substrate overlay, so a later event in the same transaction sees an
    ///    earlier event's effect. The projection view advances separately, in
    ///    canonical event order with the transaction's operations, and the
    ///    chunk's one write transaction persists the deltas.
    /// 4. **re-resolve the GHO row for the operation parser** — the tx-start
    ///    read predates a same-tx `ReserveInitialized` linking the GHO
    ///    vToken; the parser's Mint/Burn classification must see the link
    ///    (the later operation executed on initialized chain state).
    ///
    /// Returning the resolved GHO addresses lets the caller parse the
    /// transaction's operations against the same overlay state.
    ///
    /// # Errors
    ///
    /// Propagates a substrate lookup failure or the dispatch's own error.
    pub(crate) async fn dispatch_transaction(
        &mut self,
        view: &mut ProjectionView,
        tx_logs: &[&alloy::rpc::types::Log],
    ) -> Result<TransactionDispatch, ConfigDispatchError> {
        // (0) The GHO row as THIS transaction sees it — the overlay carries
        //     every prior transaction's projection. This is the
        //     TRANSACTION-START snapshot: the discount pre-pass below runs
        //     against it even when the tx later links the vToken.
        let gho_asset = self.gho_asset();
        let gho_vtoken = gho_asset
            .as_ref()
            .and_then(|g| g.v_token_address.as_deref())
            .and_then(|s| s.parse().ok());

        // (1) Re-resolve the GHO vToken revision — the overlay's cached row
        //     carries the prior transaction's `Upgraded` bump.
        let vtoken_revision: Option<u32> = match gho_asset
            .as_ref()
            .and_then(|g| g.v_token_address.as_deref())
        {
            Some(addr_str) => self
                .substrate
                .lookup_asset_row(ASSET_KIND_V_TOKEN, addr_str)?
                .map(|row| row.v_token_revision),
            None => None,
        };

        // (2) The discount pre-pass (RPC + the DB-cache path) reads `conn`
        //     (sees prior transactions' writes).
        let anchor = self.anchor()?;
        let discounts = build_discount_snapshot(
            &BlockReads::at(&self.provider, anchor),
            tx_logs,
            gho_vtoken,
            vtoken_revision,
            self.substrate,
        )
        .await?;

        // (3) The config-event dispatch (RPC + substrate lookups). The
        //     revision reads ride the per-chunk memo; the same-block
        //     independent `eth_call`s ride the multicall batch.
        let deltas = dispatch_config_events(self, view, tx_logs).await?;

        // (4) Post-config GHO re-resolve for the operation parser: a same-tx
        //     `ReserveInitialized` linking the GHO vToken marked the row
        //     dirty, so this read sees the link and the tx's Mint/Burn logs
        //     classify as GHO — the initialized state the later operation
        //     executed on. The discount pre-pass above deliberately keeps
        //     its transaction-start snapshot; only the addresses the
        //     POST-apply consumers ride are fresh: the operation-parsing
        //     addresses, and the discount token the post-apply refresh
        //     reads — a same-tx `DiscountTokenUpdated` rotation must refresh
        //     against the NEW contract's ledger, never the tx-start one.
        let gho_asset_post = self.gho_asset();
        let underlying = gho_asset_post
            .as_ref()
            .and_then(|g| g.gho_token_address.parse().ok());
        let gho_vtoken_post = gho_asset_post
            .as_ref()
            .and_then(|g| g.v_token_address.as_deref())
            .and_then(|s| s.parse().ok());
        Ok(TransactionDispatch {
            discounts,
            deltas,
            gho_underlying_address: underlying,
            gho_vtoken_address: gho_vtoken_post,
        })
    }

    /// The `ProxyCreated` resolution through this chunk's memo + provider —
    /// the chunk-dispatch sibling of the `match_proxy_id` free fn the cold-boot
    /// bootstrap pass calls with its own memo.
    ///
    /// # Errors
    ///
    /// Propagates the revision `eth_call` failure on a memo miss.
    pub(super) async fn resolve_proxy_created(
        &mut self,
        id: &alloy::primitives::B256,
        proxy_address: &Address,
        implementation_address: &Address,
    ) -> Result<Option<ProxyCreationResolution>, ConfigDispatchError> {
        let anchor = self.anchor()?;
        let reads = BlockReads::at(&self.provider, anchor);
        match_proxy_id(
            &mut self.memo,
            &reads,
            id,
            proxy_address,
            implementation_address,
        )
        .await
    }
}

/// The products of one log-transaction's dispatch stages — the GHO-discount
/// snapshot, the emitted config events, and the overlay-resolved GHO addresses
/// the caller's operation parser consumes.
#[derive(Debug)]
pub(crate) struct TransactionDispatch {
    /// The per-user GHO discount snapshot for this transaction.
    pub(crate) discounts: HashMap<Address, U256>,
    /// The configuration deltas this transaction reduced: the caller advances
    /// the projection view in canonical event order and the chunk's one write
    /// transaction persists them.
    pub(crate) deltas: Vec<CanonicalDelta>,
    /// The GHO underlying (`GHO`) address, resolved POST-config (a same-tx
    /// `ReserveInitialized` link is visible).
    pub(crate) gho_underlying_address: Option<Address>,
    /// The GHO vToken (`v_token`) address, resolved POST-config (a same-tx
    /// `ReserveInitialized` link is visible).
    pub(crate) gho_vtoken_address: Option<Address>,
}

/// Read a `uint256`-returning no-arg call (`ATOKEN_REVISION()`,
/// `DEBT_TOKEN_REVISION()`).
async fn read_uint256_return(
    reads: &BlockReads<'_>,
    target: &Address,
    sig: &str,
) -> Result<U256, ConfigDispatchError> {
    let calldata = encode_no_arg_call(sig);
    let ret = reads.call(*target, calldata).await?;
    require_word0_u256(&ret, sig, target)
}
