//! The per-chunk config-dispatch context.
//!
//! [`ChunkContext`] owns the chunk-scoped dispatch constants (provider,
//! market/chain ids, Pool + oracle addresses) plus the block-pinned
//! [`RevisionMemo`], a `(implementation, selector, block) → revision` cache
//! over the 4 revision reads, so repeated same-implementation probes inside
//! one chunk issue one RPC. The context borrows its [`ChunkSpan`], so it
//! cannot outlive the chunk that built it — the block-pinned staleness rule
//! is enforced by construction, not by convention.
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
//! transaction — a chunk-start snapshot is the exact shape W6 disproves),
//! and row 11 (one memo instance per phase; the cold-boot bootstrap pass in
//! `run::fetch` builds its own [`RevisionMemo`] and calls
//! [`super::revision::match_proxy_id`] with it).

use std::collections::HashMap;

use crate::run::{apply_chunk_events_on_conn, AaveChunkEvent, AaveChunkWriteReport};
use crate::updater::run::substrate::{ChunkSubstrate, ASSET_KIND_V_TOKEN};
use alloy::primitives::{Address, U256};
use degenbot_db::DbError;
use degenbot_rpc::provider::AlloyProvider;
use rusqlite::Connection;

use super::build_discount_snapshot;
use super::dispatch_config_events;
use super::revision::{match_proxy_id, ProxyCreationResolution};
use super::revision_selector;
use super::{encode_no_arg_call, word0_to_u256, ConfigDispatchError};

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

/// The per-chunk config-dispatch context: the chunk-scoped dispatch constants
/// + the block-pinned [`RevisionMemo`], in ONE owner with a single lifetime.
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
    oracle_address: Option<Address>,
    memo: RevisionMemo,
}

impl<'span> ChunkContext<'span> {
    /// Build the context owning a fresh memo for one chunk.
    pub(crate) fn for_chunk(
        span: &'span ChunkSpan,
        provider: AlloyProvider,
        market_id: i64,
        chain_id: i64,
        pool_address: Address,
        oracle_address: Option<Address>,
    ) -> Self {
        Self {
            span,
            provider,
            market_id,
            chain_id,
            pool_address,
            oracle_address,
            memo: RevisionMemo::new(),
        }
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

    /// The `POOL` proxy address (`getConfiguration` + the ops parser's scope).
    pub(super) fn pool_address(&self) -> Address {
        self.pool_address
    }

    /// The `PRICE_ORACLE` address as captured at chunk start (the per-event
    /// `resolve_reserve_oracle_address` re-reads `conn` when this is `None`).
    pub(super) fn oracle_address(&self) -> Option<Address> {
        self.oracle_address
    }

    /// The block-pinned revision memo. Every revision read the dispatch issues
    /// resolves through here, so the memo's block lane is applied at exactly
    /// the reads that need it.
    pub(super) fn memo(&mut self) -> &mut RevisionMemo {
        &mut self.memo
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
        conn: &Connection,
        events: &[AaveChunkEvent],
        substrate: &mut ChunkSubstrate,
    ) -> Result<AaveChunkWriteReport, DbError> {
        apply_chunk_events_on_conn(conn, self.market_id, events, substrate)
    }

    /// Read `sig` on `target` at `block_number` through the memo — the one
    /// seam every revision read in the dispatch goes through.
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

    /// Run one log-transaction's overlay-mediated read + dispatch stages in
    /// the ONE order the read-your-own-writes contract requires.
    ///
    /// The sequence is fixed here and nowhere else:
    ///
    /// 1. **re-read the overlay's GHO vToken revision** — a prior log-
    ///    transaction's `Upgraded` apply bumped the cached row, and this read
    ///    must see the bump (a chunk-start snapshot would not).
    /// 2. **build the discount snapshot** — the GHO-discount pre-pass, gated
    ///    on the revision just read.
    /// 3. **dispatch the config events** — each event applies intra-dispatch via
    ///    [`ChunkContext::apply_events`], so a later event in the same
    ///    transaction sees an earlier event's write.
    ///
    /// Returning the resolved GHO addresses lets the caller parse the
    /// transaction's operations against the same overlay state.
    ///
    /// # Errors
    ///
    /// Propagates a substrate lookup failure or the dispatch's own error.
    pub(crate) async fn dispatch_transaction(
        &mut self,
        conn: &Connection,
        substrate: &mut ChunkSubstrate,
        tx_logs: &[&alloy::rpc::types::Log],
        block_number: u64,
    ) -> Result<TransactionDispatch, ConfigDispatchError> {
        // (0) Re-resolve the GHO row for THIS transaction — sees a prior
        //     transaction's write through the overlay's dirty-mark contract.
        let gho_asset = substrate.gho_asset(conn)?;
        let underlying = gho_asset
            .as_ref()
            .and_then(|g| g.gho_token_address.as_deref())
            .and_then(|s| s.parse().ok());
        let gho_vtoken = gho_asset
            .as_ref()
            .and_then(|g| g.v_token_address.as_deref())
            .and_then(|s| s.parse().ok());
        let stk_aave = gho_asset
            .as_ref()
            .and_then(|g| g.v_gho_discount_token.as_deref())
            .and_then(|s| s.parse().ok());

        // (1) Re-resolve the GHO vToken revision — the overlay's cached row
        //     carries the prior transaction's `Upgraded` bump.
        let vtoken_revision: Option<u32> = match gho_asset
            .as_ref()
            .and_then(|g| g.v_token_address.as_deref())
        {
            Some(addr_str) => substrate
                .lookup_asset_row(conn, self.market_id, ASSET_KIND_V_TOKEN, addr_str)?
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
            self.market_id,
            conn,
        )
        .await?;

        // (3) The config-event dispatch (RPC + substrate lookups). The
        //     revision reads ride the per-chunk memo; the same-block
        //     independent `eth_call`s ride the multicall batch.
        let config_events = dispatch_config_events(
            self,
            tx_logs,
            conn,
            gho_asset.as_ref(),
            block_number,
            substrate,
        )
        .await?;

        Ok(TransactionDispatch {
            discounts,
            config_events,
            gho_underlying_address: underlying,
            gho_vtoken_address: gho_vtoken,
            stk_aave_address: stk_aave,
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
        block_number: u64,
    ) -> Result<Option<ProxyCreationResolution>, ConfigDispatchError> {
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

/// The products of one log-transaction's dispatch stages — the GHO-discount
/// snapshot, the emitted config events, and the overlay-resolved GHO addresses
/// the caller's operation parser consumes.
#[derive(Debug)]
pub(crate) struct TransactionDispatch {
    /// The per-user GHO discount snapshot for this transaction.
    pub(crate) discounts: HashMap<Address, U256>,
    /// The config events dispatched (already applied intra-dispatch).
    pub(crate) config_events: Vec<AaveChunkEvent>,
    /// The GHO underlying (`GHO`) address, resolved from the overlay.
    pub(crate) gho_underlying_address: Option<Address>,
    /// The GHO vToken (`v_token`) address, resolved from the overlay.
    pub(crate) gho_vtoken_address: Option<Address>,
    /// The GHO discount token (stkAAVE) address, resolved from the overlay.
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
    Ok(word0_to_u256(&ret).unwrap_or(U256::ZERO))
}
