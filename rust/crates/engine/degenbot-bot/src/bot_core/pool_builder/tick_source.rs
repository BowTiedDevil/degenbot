//! The live-miss tick-word source over [`ConstructionIo`] — the **Tick-word source**
//! capability's miss-path call site (GLOSSARY).

//! The sparse-map miss path (`swap_simulation` -> `MissingTickWord`) reaches the
//! pool state's stored [`TickWordSource`] when a walk enters an unfetched bitmap
//! word. This impl is the core's own source: UNBOUND — one instance reads any
//! pool the key names, because the key carries the pool's read descriptor
//! (V3 address + spacing, or V4 `StateView` + pool id + spacing). The legacy
//! Python path stores a `PyTickWordSource` callback instead; a pure-Rust
//! consumer wires this impl and still gets sparse-map backfill.

//! The read a word backfill needs already exists in this crate:
//! [`super::builder::bootstrap_v3_tick_map`] / `bootstrap_v4_tick_map` perform it
//! for the registration Chain arm over the generic RPC primitive. This impl
//! reuses them, so one read implementation serves both call sites.

//! ## Sync bridging

//! [`TickWordSource::fetch_tick_word`] is synchronous and the io
//! primitives are `async`. The read is a rare, multi-second RPC on a miss path
//! that already released every state lock, so it bridges through the shared
//! runtime: `block_in_place` when the calling thread is already inside a Tokio
//! runtime (the pump's threads are), a plain `block_on` otherwise (construction,
//! the `PyO3` `detach` path).

use std::sync::Arc;

use degenbot_pools::sim::tick_fetch::{
    FetchedTickWord, TickWordFetchError, TickWordKey, TickWordSource,
};

use crate::bot_core::construction_io::{ConstructionIo, NoDb};

/// A [`TickWordSource`] reading through the construction I/O seam — unbound,
/// so ONE instance serves every pool the session constructs.
pub struct ConstructionIoTickWordSource {
    io: ConstructionIo,
}

impl std::fmt::Debug for ConstructionIoTickWordSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConstructionIoTickWordSource")
            .finish_non_exhaustive()
    }
}

impl ConstructionIoTickWordSource {
    /// Build the shared source over the session's construction I/O.
    ///
    /// The DB half is unused by a tick-word read, so the handle is rebuilt from
    /// the RPC half alone ([`NoDb`] supplies the no-database arm) — the source
    /// stays independent of whether the session opened a database.
    #[must_use]
    pub fn attached(io: &ConstructionIo) -> Arc<dyn TickWordSource> {
        let rpc = Arc::clone(&io.rpc);
        Arc::new(Self {
            io: ConstructionIo::new(Arc::new(NoDb), rpc),
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

impl TickWordSource for ConstructionIoTickWordSource {
    fn fetch_tick_word(
        &self,
        key: &TickWordKey,
        word: i32,
        block: u64,
    ) -> Result<Option<FetchedTickWord>, TickWordFetchError> {
        let tick_spacing = match key {
            TickWordKey::V3 { tick_spacing, .. } | TickWordKey::V4 { tick_spacing, .. } => {
                *tick_spacing
            }
        };
        // The word's first tick: `(word << 8) * spacing`, the same mapping the
        // registration Chain arm starts from.
        let tick = word
            .checked_mul(256)
            .and_then(|t| t.checked_mul(tick_spacing))
            .ok_or(TickWordFetchError::OutOfRange)?;
        let read = async {
            match *key {
                TickWordKey::V3 { address, .. } => {
                    super::builder::bootstrap_v3_tick_map(
                        &self.io,
                        address,
                        tick,
                        tick_spacing,
                        block,
                    )
                    .await
                }
                TickWordKey::V4 {
                    state_view,
                    pool_id,
                    ..
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
        let (ticks, bitmap) = bridge(read).map_err(|_| TickWordFetchError::FetchFailed)?;
        Ok(Some(FetchedTickWord {
            word,
            ticks,
            bitmap: Some(bitmap),
        }))
    }
}
