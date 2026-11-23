//! The per-chunk config-dispatch context.
//!
//! [`ChunkContext`] owns the chunk-scoped dispatch constants (provider,
//! market/chain ids, Pool + oracle addresses) plus the block-pinned
//! [`RevisionMemo`], a `(implementation, selector, block) → revision` cache
//! over the 4 revision reads, so repeated same-implementation probes inside
//! one chunk issue one RPC. The context borrows its [`ChunkSpan`], the chunk
//! `Connection`, and the [`ChunkSubstrate`], so it cannot outlive the chunk
//! that built it — the block-pinned staleness rule is enforced by
//! construction, not by convention — and every dispatch arm takes the single
//! context instead of the exploded id + connection + substrate list.
//!
//! [`ChunkContext::dispatch_transaction`] is the chunk overlay's ONE
//! apply→bump→dispatch seam, in the fixed order the read-your-own-writes
//! contract requires: re-read the overlay's GHO vToken revision (a prior
//! log-transaction's `Upgraded` apply bumped the cached row), build the
//! discount snapshot off it, then dispatch the config events (each applied
//! intra-dispatch via [`ChunkContext::apply_events`]).
//!
//! Surface table: `tests/fixtures/cassettes/wave4/UPGRADE-MAP.md` §(A)
//! row 3 (the memo key MUST include the block lane), row 7 (stage 1 of
//! `dispatch_transaction` re-resolves the GHO vToken revision per
//! transaction — a chunk-start snapshot is the mutated-replay shape),
//! and row 11 (one memo instance per phase; the cold-boot bootstrap pass in
//! `run::fetch` builds its own [`RevisionMemo`] and calls
//! [`super::revision::match_proxy_id`] with it).

use std::collections::HashMap;

use crate::run::{apply_chunk_events_on_conn, AaveChunkEvent, AaveChunkWriteReport};
use crate::updater::run::substrate::{ChunkSubstrate, ASSET_KIND_V_TOKEN};
use alloy::primitives::{Address, Bytes, U256};
use degenbot_db::aave::AaveGhoAsset;
use degenbot_db::DbError;
use degenbot_rpc::provider::AlloyProvider;
use rusqlite::Connection;

use super::build_discount_snapshot;
use super::dispatch_config_events;
use super::revision::{match_proxy_id, ProxyCreationResolution};
use super::revision_selector;
use super::{
    encode_no_arg_call, eth_calls_batched_or_direct, require_word0_u256, ConfigDispatchError,
};

// ── the revision memo ──────────────────────────────────────────────────────

/// Per-run, block-pinned memo over the config dispatch's revision reads
/// (`ATOKEN_REVISION()` / `DEBT_TOKEN_REVISION()` / `POOL_REVISION()` /
/// `CONFIGURATOR_REVISION()`).
///
/// The key is `(implementation, selector, block)`: a revision is chain state
/// at a block, so one triple can never observe two values within a run — a
/// memo hit is definitionally the same read. The block lane is what makes an
/// `Upgraded` chunk safe: the upgrade dispatch reads the NEW implementation
/// at the upgrade block, and every later read sits at a later block (its own
/// key), so no read can be served a pre-upgrade value across the boundary.
///
/// Lifetime: one instance per chunk apply (`process_chunk_on_conn`), which
/// is the whole read span the dispatch can observe — chunks partition the
/// block range, so no cross-chunk key collision exists, and the memo never
/// outlives the run (a plain local, not global state).
#[derive(Debug, Default)]
pub(crate) struct RevisionMemo {
    entries: HashMap<(Address, [u8; 4], u64), U256>,
}

impl RevisionMemo {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The memo hit for one revision-read key, if any.
    pub(super) fn get(
        &self,
        target: &Address,
        selector: [u8; 4],
        block_number: u64,
    ) -> Option<U256> {
        self.entries
            .get(&(*target, selector, block_number))
            .copied()
    }

    /// Record one revision read (the value the chain answered at `block_number`).
    pub(super) fn insert(
        &mut self,
        target: &Address,
        selector: [u8; 4],
        block_number: u64,
        value: U256,
    ) {
        self.entries
            .insert((*target, selector, block_number), value);
    }

    /// The memoized [`read_uint256_return`]: one RPC per
    /// `(implementation, selector, block)` per run, identical results after.
    ///
    /// # Errors
    ///
    /// Propagates the underlying `eth_call` failure on a memo miss.
    pub(crate) async fn read(
        &mut self,
        provider: &AlloyProvider,
        target: &Address,
        sig: &str,
        block_number: u64,
    ) -> Result<U256, ConfigDispatchError> {
        let selector = revision_selector(sig);
        if let Some(v) = self.get(target, selector, block_number) {
            return Ok(v);
        }
        let v = read_uint256_return(provider, target, sig, block_number).await?;
        self.insert(target, selector, block_number, v);
        Ok(v)
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

/// The per-chunk config-dispatch context: the chunk-scoped dispatch constants,
/// the chunk `Connection`, the [`ChunkSubstrate`], and the block-pinned
/// [`RevisionMemo`], in ONE owner with a single lifetime.
///
/// Built by [`ChunkContext::for_chunk`] at the top of a chunk apply and
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
    conn: &'span Connection,
    substrate: &'span mut ChunkSubstrate,
    block_number: u64,
}

impl<'span> ChunkContext<'span> {
    /// Build the context for one chunk, borrowing the chunk `Connection` and
    /// the [`ChunkSubstrate`] for the context's whole lifetime.
    ///
    /// `block_number` starts at the span's first block; each log-transaction's
    /// dispatch enters its own block through [`ChunkContext::begin_transaction`]
    /// before any read or emitted event.
    pub(crate) fn for_chunk(
        span: &'span ChunkSpan,
        provider: AlloyProvider,
        market_id: i64,
        chain_id: i64,
        pool_address: Address,
        conn: &'span Connection,
        substrate: &'span mut ChunkSubstrate,
    ) -> Self {
        Self {
            span,
            provider,
            market_id,
            chain_id,
            pool_address,
            memo: RevisionMemo::new(),
            conn,
            substrate,
            block_number: span.chunk_start,
        }
    }

    /// Enter one log-transaction's dispatch scope: the block every read and
    /// every emitted event for that transaction rides.
    pub(crate) fn begin_transaction(&mut self, block_number: u64) {
        self.block_number = block_number;
    }

    /// The block the current log-transaction's dispatch reads + emits at.
    pub(super) fn block_number(&self) -> u64 {
        self.block_number
    }

    /// The chunk `Connection` — the single transaction the whole dispatch
    /// rides. Returned with the context's own lifetime so a caller may keep it
    /// while borrowing the substrate.
    pub(super) fn conn(&self) -> &'span Connection {
        self.conn
    }

    /// The chunk substrate, for the callers that drive it directly (the
    /// per-transaction ops parser, the end-of-chunk flush, the stkAAVE
    /// backfill).
    pub(crate) fn substrate_mut(&mut self) -> &mut ChunkSubstrate {
        &mut *self.substrate
    }

    /// The GHO asset row for the current transaction, re-resolved FRESH
    /// through the substrate's read-your-own-writes overlay on EVERY call.
    ///
    /// This is the ONE freshness rule for the GHO asset: the substrate's
    /// dirty-mark contract (every apply that touches `aave_gho_tokens` calls
    /// [`ChunkSubstrate::mark_gho_dirty`]) makes a repeated read a cache hit
    /// until a write invalidates it, so re-resolving per event is both
    /// always-correct (a same-tx `ReserveInitialized` or
    /// `DiscountTokenUpdated` at an earlier logIndex is visible) and cheap. A
    /// `None` here is the genuine "no GHO market row" state, never a stale
    /// snapshot.
    ///
    /// # Errors
    ///
    /// Propagates the substrate lookup failure.
    pub(crate) fn gho_asset(&mut self) -> Result<Option<AaveGhoAsset>, DbError> {
        let conn = self.conn;
        self.substrate.gho_asset(conn)
    }

    /// The provider every dispatch RPC rides.
    pub(super) fn provider(&self) -> &AlloyProvider {
        &self.provider
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
    pub(crate) fn apply_events(
        &mut self,
        events: &[AaveChunkEvent],
    ) -> Result<AaveChunkWriteReport, DbError> {
        let conn = self.conn;
        let market_id = self.market_id;
        let report = apply_chunk_events_on_conn(conn, market_id, events, &mut *self.substrate)?;
        // Same-transition RPC targets: the dispatch's own `getConfiguration`
        // + ops-parser scope RPCs the POOL proxy by this field, so an
        // `AddressSet` POOL rotation applied mid-chunk must repoint it at the
        // transition boundary (the DB row is the durable half; this is the
        // in-chunk half). The oracle target is NOT cached here —
        // `resolve_reserve_oracle_address` reads `conn` fresh per event.
        for event in events {
            if let AaveChunkEvent::ContractAddressChanged {
                name, new_address, ..
            } = event
            {
                if name == "POOL" {
                    self.pool_address = new_address.parse().map_err(|_| {
                        DbError::Decode(format!(
                            "POOL rotation target {new_address} is not an address"
                        ))
                    })?;
                }
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
    pub(crate) fn apply_deltas(
        &mut self,
        deltas: &[crate::projection::CanonicalDelta],
    ) -> Result<crate::run::CanonicalDeltaWriteReport, DbError> {
        crate::run::apply_canonical_deltas_on_conn(self.conn, deltas, &mut *self.substrate)
    }

    /// Read `sig` on `target` at `block_number` through the memo — one of the
    /// two seams every revision read in the dispatch goes through (the other is
    /// [`ChunkContext::read_revision_batch`]), so the memo's block lane is
    /// applied at exactly the reads that need it.
    ///
    /// # Errors
    ///
    /// Propagates the underlying `eth_call` failure on a memo miss.
    pub(super) async fn read_revision(
        &mut self,
        target: &Address,
        sig: &str,
        block_number: u64,
    ) -> Result<U256, ConfigDispatchError> {
        debug_assert!(
            self.span.contains(block_number),
            "chunk-dispatch revision read at a block outside the chunk span"
        );
        self.memo
            .read(&self.provider, target, sig, block_number)
            .await
    }

    /// Read several same-block calls in ONE transport pass, owning the revision
    /// memo for every [`BatchRead::Revision`] slot — the batched sibling of
    /// [`ChunkContext::read_revision`], and the second of the two seams every
    /// revision read goes through.
    ///
    /// A revision slot is a memo key lookup first: a hit issues no call. Every
    /// remaining revision miss and every plain slot fold into ONE
    /// [`eth_calls_batched_or_direct`] pass in slot order, and each answered
    /// revision value is recorded under its `(implementation, selector, block)`
    /// key before the call returns. Results come back aligned with `pending`,
    /// so the caller never touches the memo or the block lane directly.
    ///
    /// # Errors
    ///
    /// Propagates the underlying `eth_call` failure on a memo miss.
    pub(super) async fn read_revision_batch(
        &mut self,
        pending: &[BatchRead<'_>],
    ) -> Result<Vec<BatchReadResult>, ConfigDispatchError> {
        let block_number = self.block_number;
        debug_assert!(
            self.span.contains(block_number),
            "chunk-dispatch revision read at a block outside the chunk span"
        );
        let mut calls: Vec<(Address, Bytes)> = Vec::new();
        // The index into `calls` each slot's read occupies, or `None` when the
        // memo already answered the slot.
        let mut call_of_slot: Vec<Option<usize>> = Vec::with_capacity(pending.len());
        let mut results: Vec<Option<BatchReadResult>> = (0..pending.len()).map(|_| None).collect();
        for (i, slot) in pending.iter().enumerate() {
            match slot {
                BatchRead::Revision { target, sig } => {
                    let selector = revision_selector(sig);
                    if let Some(v) = self.memo.get(target, selector, block_number) {
                        results[i] = Some(BatchReadResult::Revision(v));
                        call_of_slot.push(None);
                    } else {
                        call_of_slot.push(Some(calls.len()));
                        calls.push((*target, encode_no_arg_call(sig)));
                    }
                }
                BatchRead::Plain { target, calldata } => {
                    call_of_slot.push(Some(calls.len()));
                    calls.push((*target, calldata.clone()));
                }
            }
        }
        let returns = eth_calls_batched_or_direct(&self.provider, &calls, block_number).await?;
        for (i, slot) in pending.iter().enumerate() {
            let Some(call_index) = call_of_slot[i] else {
                continue;
            };
            let ret = &returns[call_index];
            match slot {
                BatchRead::Revision { target, sig } => {
                    let selector = revision_selector(sig);
                    let v = require_word0_u256(ret, sig, target)?;
                    self.memo.insert(target, selector, block_number, v);
                    results[i] = Some(BatchReadResult::Revision(v));
                }
                BatchRead::Plain { .. } => {
                    results[i] = Some(BatchReadResult::Plain(ret.clone()));
                }
            }
        }
        // Every slot is filled by construction — a memo hit or its own batched
        // read. The guard keeps the invariant loud rather than assumed.
        results
            .into_iter()
            .map(|r| {
                r.ok_or_else(|| {
                    ConfigDispatchError::DecodeShape(
                        "read_revision_batch: a slot was left unfilled after the batched reads"
                            .to_string(),
                    )
                })
            })
            .collect()
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
    /// 3. **dispatch the config events** — each event applies intra-dispatch via
    ///    [`ChunkContext::apply_events`], so a later event in the same
    ///    transaction sees an earlier event's write.
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
        tx_logs: &[&alloy::rpc::types::Log],
    ) -> Result<TransactionDispatch, ConfigDispatchError> {
        let conn = self.conn;
        let market_id = self.market_id;
        let block_number = self.block_number;
        // (0) Re-resolve the GHO row for THIS transaction — sees a prior
        //     transaction's write through the overlay's dirty-mark contract.
        //     This is the TRANSACTION-START snapshot: the discount pre-pass
        //     below runs against it even when the tx later links the vToken.
        let gho_asset = self.gho_asset()?;
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
                .lookup_asset_row(conn, market_id, ASSET_KIND_V_TOKEN, addr_str)?
                .map(|row| row.v_token_revision),
            None => None,
        };

        // (2) The discount pre-pass (RPC + the DB-cache path) reads `conn`
        //     (sees prior transactions' writes).
        let discounts = build_discount_snapshot(
            self.provider(),
            tx_logs,
            block_number,
            gho_vtoken,
            vtoken_revision,
            market_id,
            conn,
        )
        .await?;

        // (3) The config-event dispatch (RPC + substrate lookups). The
        //     revision reads ride the per-chunk memo; the same-block
        //     independent `eth_call`s ride the multicall batch.
        let config_events = dispatch_config_events(self, tx_logs).await?;

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
        let gho_asset_post = self.gho_asset()?;
        let underlying = gho_asset_post
            .as_ref()
            .and_then(|g| g.gho_token_address.parse().ok());
        let gho_vtoken_post = gho_asset_post
            .as_ref()
            .and_then(|g| g.v_token_address.as_deref())
            .and_then(|s| s.parse().ok());
        let stk_aave_post = gho_asset_post
            .as_ref()
            .and_then(|g| g.v_gho_discount_token.as_deref())
            .and_then(|s| s.parse().ok());

        Ok(TransactionDispatch {
            discounts,
            config_events,
            gho_underlying_address: underlying,
            gho_vtoken_address: gho_vtoken_post,
            stk_aave_address: stk_aave_post,
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
        let block_number = self.block_number;
        match_proxy_id(
            &mut self.memo,
            &self.provider,
            id,
            proxy_address,
            implementation_address,
            block_number,
        )
        .await
    }
}

/// One slot in a [`ChunkContext::read_revision_batch`] pass, in caller order.
///
/// A revision slot is memo-keyed by the context (the batch derives the
/// selector, checks the memo, and records the answered value) and reads `sig`
/// as a no-arg call; a plain slot is any other same-block `eth_call` that
/// rides the same transport pass but is never cached.
#[derive(Debug)]
pub(super) enum BatchRead<'a> {
    /// A memoized revision read: `sig` on `target`, at the context block.
    Revision { target: Address, sig: &'a str },
    /// A plain same-block read: `calldata` on `target`, never memoized.
    Plain { target: Address, calldata: Bytes },
}

/// The decoded outcome of one [`BatchRead`], aligned by index with the pass.
/// A revision outcome is the memoized value; a plain outcome is the raw return
/// the caller decodes itself.
#[derive(Debug)]
pub(super) enum BatchReadResult {
    /// The revision value (also recorded in the memo).
    Revision(U256),
    /// The raw return of a plain read.
    Plain(Bytes),
}

/// The products of one log-transaction's dispatch stages — the GHO-discount
/// snapshot, the emitted config events, and the overlay-resolved GHO addresses
/// the caller's operation parser consumes.
#[derive(Debug)]
pub(crate) struct TransactionDispatch {
    /// The per-user GHO discount snapshot for this transaction.
    pub(crate) discounts: HashMap<Address, U256>,
    /// The config events dispatched (already applied intra-dispatch).
    pub(crate) config_events: Vec<AaveChunkEvent>,
    /// The GHO underlying (`GHO`) address, resolved POST-config (a same-tx
    /// `ReserveInitialized` link is visible).
    pub(crate) gho_underlying_address: Option<Address>,
    /// The GHO vToken (`v_token`) address, resolved POST-config (a same-tx
    /// `ReserveInitialized` link is visible).
    pub(crate) gho_vtoken_address: Option<Address>,
    /// The GHO discount token (stkAAVE) address, resolved POST-config — the
    /// address the post-apply discount refresh rides (an in-tx
    /// `DiscountTokenUpdated` rotation is visible, so the refresh reseeds
    /// from the NEW contract's ledger). The discount pre-pass does not
    /// consume this field: its transaction-start snapshot lives in stage (2).
    pub(crate) stk_aave_address: Option<Address>,
}

/// Read a `uint256`-returning no-arg call (`ATOKEN_REVISION()`,
/// `DEBT_TOKEN_REVISION()`).
async fn read_uint256_return(
    provider: &AlloyProvider,
    target: &Address,
    sig: &str,
    block_number: u64,
) -> Result<U256, ConfigDispatchError> {
    let calldata = encode_no_arg_call(sig);
    let ret = provider
        .eth_call(target, calldata, Some(block_number))
        .await?;
    require_word0_u256(&ret, sig, target)
}
