//! The live-miss tick-word source over [`ConstructionIo`] — the **Tick-word source**
//! capability's miss-path call site (GLOSSARY).
//!
//! The sparse-map miss path (`swap_simulation` -> `MissingTickWord`) reaches the
//! pool state's stored [`TickWordFetcher`] when a walk enters an unfetched bitmap
//! word. Until this adapter the only production impl wrapped a Python callable
//! (`PyTickWordFetcher`), so a pure-Rust consumer had no backfill at all: an
//! unfetched word was a hard `FetchFailed` while Python backfilled transparently.
//!
//! The read a word backfill needs already exists in this crate:
//! [`super::builder::bootstrap_v3_tick_map`] / `bootstrap_v4_tick_map` perform it
//! for the registration Chain arm over the generic RPC primitive. This adapter
//! reuses them, so one read implementation serves both call sites.
//!
//! ## Binding
//!
//! A word read needs the pool's descriptor — `address` + `tick_spacing`, or
//! `state_view` + `pool_id` + `tick_spacing` — facts only the construction arm
//! holds. The source is therefore constructed at the param-application site
//! (`route.rs` for V3, `build_register.rs` for V4) once the build resolved them,
//! and stored on the pooled state by registration. A source never reads the state
//! owner it is fetching for (that is the re-entrancy trap the trait documents).
//!
//! ## Sync bridging
//!
//! [`TickWordFetcher::fetch_missing_tick_word`] is synchronous and the io
//! primitives are `async`. The read is a rare, multi-second RPC on a miss path
//! that already released every state lock, so it bridges through the shared
//! runtime: `block_in_place` when the calling thread is already inside a Tokio
//! runtime (the pump's threads are), a plain `block_on` otherwise (construction,
//! the `PyO3` `detach` path).

use std::sync::Arc;

use alloy::primitives::Address;
use degenbot_pools::sim::tick_fetch::{FetchTickWordError, FetchedTickWord, TickWordFetcher};

use crate::bot_core::construction_io::{ConstructionIo, NoDb};

/// Which pool a bound source reads for.
#[derive(Debug, Clone, Copy)]
pub enum TickWordSourcePool {
    /// A V3-style concentrated-liquidity pool.
    V3 {
        /// The pool contract address (`tickBitmap(int16)` target).
        address: Address,
        /// The pool's tick spacing (bit -> tick resolution).
        tick_spacing: i32,
    },
    /// A V4 managed pool.
    V4 {
        /// The `StateView` contract exposing `getTickBitmap(bytes32,int16)`.
        state_view: Address,
        /// The pool's on-chain id.
        pool_id: [u8; 32],
        /// The pool's tick spacing.
        tick_spacing: i32,
    },
}

impl TickWordSourcePool {
    /// The tick spacing that maps a word position back to a tick.
    #[must_use]
    const fn tick_spacing(&self) -> i32 {
        match self {
            Self::V3 { tick_spacing, .. } | Self::V4 { tick_spacing, .. } => *tick_spacing,
        }
    }
}

/// A [`TickWordFetcher`] bound to one pool's descriptor, reading through the
/// construction I/O seam.
pub struct ConstructionIoTickWordSource {
    io: ConstructionIo,
    pool: TickWordSourcePool,
}

impl std::fmt::Debug for ConstructionIoTickWordSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConstructionIoTickWordSource")
            .field("pool", &self.pool)
            .finish_non_exhaustive()
    }
}

impl ConstructionIoTickWordSource {
    /// Bind a source to one pool.
    ///
    /// The DB half is unused by a tick-word read, so the handle is rebuilt from
    /// the RPC half alone ([`NoDb`] supplies the no-database arm) — the source
    /// stays independent of whether the session opened a database.
    #[must_use]
    pub fn attached(io: &ConstructionIo, pool: TickWordSourcePool) -> Arc<dyn TickWordFetcher> {
        let rpc = Arc::clone(&io.rpc);
        Arc::new(Self {
            io: ConstructionIo::new(Arc::new(NoDb), rpc),
            pool,
        })
    }
}

/// Drive a synchronous call site's future: `block_in_place` under a running
/// runtime (the pump's threads), plain `block_on` outside one.
fn bridge<F: std::future::Future>(fut: F) -> F::Output {
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::block_in_place(|| degenbot_core::runtime::get_runtime().block_on(fut))
    } else {
        degenbot_core::runtime::get_runtime().block_on(fut)
    }
}

impl TickWordFetcher for ConstructionIoTickWordSource {
    fn fetch_missing_tick_word(
        &self,
        _pool_id: u64,
        word: i32,
        block: u64,
    ) -> Result<FetchedTickWord, FetchTickWordError> {
        let tick_spacing = self.pool.tick_spacing();
        // The word's first tick: `(word << 8) * spacing`, the same mapping the
        // registration Chain arm starts from.
        let tick = word
            .checked_mul(256)
            .and_then(|t| t.checked_mul(tick_spacing))
            .ok_or(FetchTickWordError::OutOfRange)?;
        let read = async {
            match self.pool {
                TickWordSourcePool::V3 {
                    address,
                    tick_spacing,
                } => {
                    super::builder::bootstrap_v3_tick_map(
                        &self.io,
                        address,
                        tick,
                        tick_spacing,
                        block,
                    )
                    .await
                }
                TickWordSourcePool::V4 {
                    state_view,
                    pool_id,
                    tick_spacing,
                } => {
                    super::builder::bootstrap_v4_tick_map(
                        &self.io,
                        state_view,
                        pool_id,
                        tick,
                        tick_spacing,
                        block,
                    )
                    .await
                }
            }
        };
        let (ticks, _bitmap) = bridge(read).map_err(|_| FetchTickWordError::FetchFailed)?;
        Ok(FetchedTickWord { word, ticks })
    }
}
