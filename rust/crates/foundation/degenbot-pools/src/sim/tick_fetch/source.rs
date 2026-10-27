//! The ONE tick-word read seam — both sparse-read call sites
//! (GLOSSARY **Tick-word source**).

//! A tick-word read answers the same choreography everywhere (compute the
//! word's bitmap -> read it -> enumerate set bits -> read each initialized
//! tick's liquidity); only the pool identity the read is addressed by
//! differs. One trait, one key value, one fetched-word value, one error enum
//! serve both:

//! - the **registration-time Chain arm** of `assemble_{v3,v4}_tick_map`
//!   (Store -> Db -> Chain precedence) — the source is unbound, the caller
//!   holds the pool descriptor and builds the key per read;
//! - the **live-pump miss path** during sparse swap simulation
//!   (`MissingTickWord` -> fetch -> merge -> retry) — the descriptor comes
//!   from the pool's own identity at the call site.

//! ## Key shape

//! [`TickWordKey`] carries the pool's read descriptor (identity only — the
//! `word` is a per-call argument): one identity serves a whole reconcile
//! loop (the Db-seed verification reads every word's bitmap against one
//! key), and a bound fetcher's descriptor outlives any single word.

//! ## Sync, not async

//! The trait method is synchronous, and every implementation owns the bridge to
//! its `async` reads through [`degenbot_core::runtime::bridged_block_on`] (an
//! in-runtime `block_in_place` hand-off, the shared runtime otherwise). The
//! callable-from contract that buys — inside the ambient multi-threaded
//! runtime, on a construction thread, on a driver's `detach` thread, or in a
//! bare standalone consumer — is stated on the trait. A future pure-async
//! standalone `Bot` API can introduce a sibling `async` trait sharing the
//! same value types.

use hashbrown::HashMap;

use alloy::primitives::{Address, U256};

use degenbot_decoders::v4_swap_decoder::V4PoolId;

use crate::TickInfo;

use crate::state::v3_state::V3PoolIdentity;

use crate::state::v4_state::V4PoolIdentity;

/// One pool's tick-word read descriptor — the identity a
/// [`TickWordSource`] addresses a word read by.
///
/// The word position is a per-call argument (one identity serves a whole
/// reconcile loop). `tick_spacing` rides the key because it maps a word
/// position back to tick space — the source cannot read it from anywhere
/// else without a second lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TickWordKey {
    /// A V3-style concentrated-liquidity pool: `tickBitmap(int16)` +
    /// `ticks(int24)` on the pool contract.
    V3 {
        /// The pool contract address.
        address: Address,
        /// The pool's tick spacing (bit -> tick resolution).
        tick_spacing: i32,
    },
    /// A V4 managed pool: `getTickBitmap(bytes32,int16)` +
    /// `getTickLiquidity(bytes32,int24)` on the **`StateView`** contract
    /// (NOT the `PoolManager` — V4 state reads route through a separate
    /// `StateView` deployment).
    V4 {
        /// The `StateView` contract address.
        state_view: Address,
        /// The pool's on-chain id.
        pool_id: V4PoolId,
        /// The pool's tick spacing.
        tick_spacing: i32,
    },
}

impl From<&V3PoolIdentity> for TickWordKey {
    fn from(identity: &V3PoolIdentity) -> Self {
        Self::V3 {
            address: identity.address,
            tick_spacing: identity.tick_spacing,
        }
    }
}

impl From<&V4PoolIdentity> for TickWordKey {
    fn from(identity: &V4PoolIdentity) -> Self {
        Self::V4 {
            state_view: identity.state_view,
            pool_id: identity.pool_id,
            tick_spacing: identity.pool_key.tick_spacing,
        }
    }
}

impl TickWordKey {
    /// The pool's tick spacing — the bit -> tick resolution the word -> tick
    /// mapping needs (both key variants carry it; the identity is otherwise
    /// arm-specific).
    #[must_use]
    pub fn tick_spacing(&self) -> i32 {
        match self {
            Self::V3 { tick_spacing, .. } | Self::V4 { tick_spacing, .. } => *tick_spacing,
        }
    }
}

/// One fetched tick-bitmap word's data, returned by [`TickWordSource`].
///
/// `ticks` may be empty — a fetched word whose bitmap is all-zero has no
/// initialized ticks, but the `word` is nonetheless now "known" (the caller
/// records it in `known_bitmap_words` so it is not re-fetched). This mirrors
/// the Python bitmap-store rule: in sparse mode a region is unknown unless
/// its word key is in the lazy-loaded map, regardless of the bitmap value.
#[derive(Debug, Clone)]
pub struct FetchedTickWord {
    /// The tick-bitmap word position that was fetched.
    pub word: i32,
    /// The initialized ticks within this word (`{tick: TickInfo}`). May be empty.
    pub ticks: HashMap<i32, TickInfo>,
    /// The exact bitmap word returned by the contract, when the read is a
    /// full-word Chain read. It is provenance for full-map verification, not
    /// a value reconstructed from `ticks`; a miss-path backfill that does not
    /// surface it carries `None`.
    pub bitmap: Option<U256>,
}

impl FetchedTickWord {
    /// A fetched word with no bitmap provenance (the miss-path shape).
    #[must_use]
    pub fn new(word: i32, ticks: HashMap<i32, TickInfo>) -> Self {
        Self {
            word,
            ticks,
            bitmap: None,
        }
    }
}

/// Why a [`TickWordSource`] read could not complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TickWordFetchError {
    /// The fetch itself failed (RPC error, timeout, transport, revert).
    #[error("rpc failure (transport, timeout, or revert)")]
    FetchFailed,
    /// Malformed return data (short bytes, bad ABI decode, type-tag mismatch).
    #[error("malformed return data (short bytes or bad abi decode)")]
    InvalidReturn,
    /// The requested `word` is outside the source's supported range.
    #[error("word outside the supported range")]
    OutOfRange,
}

/// The one tick-word read seam — a sparse read of one pool's tick-bitmap
/// word plus the liquidity of every initialized tick in it.
///
/// Implementations may do I/O (RPC). Registration-time consumers run the
/// Chain arm AFTER the Store arm's closure has returned and dropped the
/// `BotState` read guard (two-phase lock protocol — no `BotState` guard held
/// across the RPC read). A pyo3-adapter impl that re-enters the GIL must do
/// so via the standard `Python::attach` per-call hop pattern.
///
/// A read is callable from a synchronous call site on EITHER side of the
/// runtime boundary: an impl bridges its async reads with
/// [`degenbot_core::runtime::bridged_block_on`], which hands the calling
/// worker's scheduler duties off when the caller is already inside the ambient
/// multi-threaded runtime. The calling thread is blocked for the whole word's
/// RPCs either way, so callers must not hold a lock they cannot afford to hold
/// for a multi-second RPC.
///
/// `Send + Sync` so it can live behind an `Arc`/`RwLock` on the engine/pool
/// side.
pub trait TickWordSource: Send + Sync + std::fmt::Debug {
    /// Fetch the tick-bitmap word `word` for the pool addressed by `key`,
    /// plus the liquidity for every initialized tick in that word, at
    /// snapshot block `block`.
    ///
    /// Returns `Ok(None)` for an all-zero bitmap (no initialized ticks in
    /// this word — do not seed; the pool registers with `tick_data=None,
    /// coverage="sparse"`). A miss-path caller maps `None` to a known-empty
    /// word (recorded in `known_bitmap_words`, no ticks merged).
    ///
    /// # Errors
    ///
    /// Returns [`TickWordFetchError::FetchFailed`] on RPC failure (timeout,
    /// transport, revert), [`TickWordFetchError::InvalidReturn`] on a
    /// malformed return (short bytes, bad ABI decode), or
    /// [`TickWordFetchError::OutOfRange`] if `word` is outside the source's
    /// supported range.
    fn fetch_tick_word(
        &self,
        key: &TickWordKey,
        word: i32,
        block: u64,
    ) -> Result<Option<FetchedTickWord>, TickWordFetchError>;
}
