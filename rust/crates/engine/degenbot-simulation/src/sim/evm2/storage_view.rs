//! The storage-view seam `BotStateDb` reads a cache through.
//!
//! [`BotStateDb`](super::bot_state_db::BotStateDb) owns the engine's one
//! synchronous storage read path: a cache first, the RPC fallback second, and
//! the fallback's own answer recorded back into the cache. The cache has two
//! representations with the same shape — the legacy per-anchor
//! [`StorageMemo`] and a [`DeltaHub`](super::delta_snapshot::DeltaHub)
//! generation, whose chain carries its own delta over its ancestors and whose
//! recency metadata decides what retention still serves. This module is the
//! object-safe seam that names that shape, so the wrapper stays one
//! implementation instead of forking per representation.
//!
//! # What crosses the seam, and what never does
//!
//! [`StorageView::get`] is the candidate read: [`Option::Some`] is the cache's
//! answer at ITS anchor — a hit, and use the cache may record — while
//! [`Option::None`] sends the caller to the fallback. [`StorageView::put`]
//! hands the fallback's own answer back, so the next read of that identity is a
//! hit.
//!
//! Everything else stays outside. Accounts and bytecode belong to the warm code
//! cache, block hashes are per-chain facts no cache memoizes, and the fallback's
//! error is the caller's: a view never transforms one. The divergence
//! observation stays in `BotStateDb` too, because it watches the RPC answer,
//! not the cache. A view accepts no engine or candidate overlay either — a
//! value it serves was read from the chain at the view's own anchor, which is
//! what makes a value from another anchor a wrong price rather than a
//! stale-but-harmless one.
//!
//! # Migration notes (internal Rust API)
//!
//! `BotStateDb`'s cache field changed from
//! `storage_memo: Option<Arc<StorageMemo>>` to
//! `storage_view: Option<Arc<dyn StorageView + 'bot>>`, borrowing for the
//! wrapper's own lifetime so a view may borrow the session's provider. Nothing
//! else in the call graph moved:
//! [`BotStateDb::with_storage_memo`](super::bot_state_db::BotStateDb::with_storage_memo) and
//! [`BotStateDb::with_storage_memo_opt`](super::bot_state_db::BotStateDb::with_storage_memo_opt)
//! keep their signatures and install a memo-backed view, and a consumer that
//! read the field directly reads `storage_view` instead.
//!
//! Choosing a representation is a construction-site decision, and that is all
//! of it: the legacy path passes `StorageMemoCache::for_anchor`'s
//! `Arc<StorageMemo>`, a delta path passes the anchor generation it already
//! reads through ([`DeltaSnapshotCache::for_anchor`](super::delta_snapshot::DeltaSnapshotCache::for_anchor)
//! or [`BalHeadDriver::snapshot`](super::delta_snapshot::BalHeadDriver::snapshot)),
//! and both coerce to the trait object handed to
//! [`BotStateDb::with_storage_view`](super::bot_state_db::BotStateDb::with_storage_view).
//! Dispatch selecting between them is a later change: the arbitrage fan-out's
//! construction site (its per-anchor memo lookup feeding the block handle) is
//! where a delta cache would hand over a generation instead, and this seam only
//! makes both spellings available to the same synchronous wrapper, with the
//! memo path and the delta path each keeping its own fallback authority.
//!
//! One consequence belongs with the migration: a view MISS is the wrapper's own
//! fallback read, never a hub round, so a consumer that wants the round
//! machinery's single flight and batching keeps reading through the hub's own
//! seam (`DeltaHub::read_storage_handle`) rather than a view.

use alloy::primitives::{Address, StorageValue};

use evm2::interpreter::Word;

use crate::sim::StorageMemo;

/// A cache `BotStateDb` reads storage words through: a hit at the view's own
/// anchor, and a place to settle the fallback's answer.
///
/// Both methods take `&self` so a view is shared by every fan-out at its anchor
/// and so the trait is object-safe — `BotStateDb` holds one
/// `Option<Arc<dyn StorageView>>`, never a representation-specific field.
pub trait StorageView: Send + Sync {
    /// The cache's word for `key` of `address`, or [`Option::None`] when the
    /// view does not serve it (unknown, or expired under its retention policy)
    /// and the caller must take the fallback.
    ///
    /// A hit is a candidate read: the view may record the use its own recency
    /// policy needs.
    fn get(&self, address: Address, key: Word) -> Option<Word>;

    /// Record `value` — an answer the fallback itself produced at the view's
    /// anchor — for the next [`StorageView::get`] of the same identity.
    fn put(&self, address: Address, key: Word, value: StorageValue);
}

/// The legacy per-anchor memo as a view: its own `get`/`put`, counting
/// included, because the anchor memo's hit/miss profile is measured.
impl StorageView for StorageMemo {
    fn get(&self, address: Address, key: Word) -> Option<Word> {
        StorageMemo::get(self, address, key)
    }

    fn put(&self, address: Address, key: Word, value: StorageValue) {
        StorageMemo::put(self, address, key, value);
    }
}
