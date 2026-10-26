//! The live-miss tick-word source over [`ConstructionIo`] — the **Tick-word source**
//! capability's miss-path call site (GLOSSARY).

//! The sparse-map miss path (`swap_simulation` -> `MissingTickWord`) reaches the
//! pool state's stored [`TickWordSource`] when a walk enters an unfetched bitmap
//! word. This impl is the core's own source: UNBOUND — one instance reads any
//! pool the key names, because the key carries the pool's read descriptor
//! (V3 address + spacing, or V4 `StateView` + pool id + spacing). The legacy
//! Python path stores a `PyTickWordSource` callback instead; a pure-Rust
//! consumer wires this impl and still gets sparse-map backfill.

//! The read itself is the ONE transport-generic choreography
//! ([`degenbot_rpc::tick_bootstrap::fetch_tick_word_via`]): this source and
//! the registration Chain arm (`bootstrap_{v3,v4}_tick_map`) drive the same
//! loop, differing only in the unreadable-bitmap policy — this miss path
//! REFUSES, the registration arm degrades to an empty word (the legacy
//! graceful fallback). This source is also the [`ConstructionIo`]
//! [`TickWordTransport`] impl's home, so the loop's io-side contract reads
//! are the paced choreography fns.

//! ## Sync bridging

//! [`TickWordSource::fetch_tick_word`] is synchronous and the io
//! primitives are `async`. The read is a rare, multi-second RPC on a miss path
//! that already released every state lock, so it bridges through the shared
//! runtime: `block_in_place` when the calling thread is already inside a Tokio
//! runtime (the pump's threads are), a plain `block_on` otherwise (construction,
//! the `PyO3` `detach` path).

use std::sync::Arc;

use alloy::primitives::{U128, U256};
use degenbot_core::errors::ProviderError;
use degenbot_pools::sim::tick_fetch::{
    FetchedTickWord, TickWordFetchError, TickWordKey, TickWordSource,
};
use degenbot_rpc::tick_bootstrap::{
    fetch_tick_word_via, map_provider_err, TickWordTransport, UnreadableBitmapPolicy,
};

use super::choreography::{self};

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
        // The Solidity bitmap selector takes an int16; a word outside that
        // range cannot name a real bitmap slot.
        let word_i16 = i16::try_from(word).map_err(|_| TickWordFetchError::OutOfRange)?;
        // The miss path REFUSES an unreadable bitmap: an unfetchable word is a
        // fetch error, never a silently empty region (the registration arm's
        // graceful degrade lives in the loop policy, not here).
        let read = fetch_tick_word_via(
            &self.io,
            key,
            word_i16,
            block,
            UnreadableBitmapPolicy::Refuse,
        );
        bridge(read).map_err(map_provider_err)
    }
}

/// The construction-I/O transport: the paced contract reads the builder's
/// bootstrap used to drive inline, now the [`TickWordTransport`] impl the
/// shared loop drives (the V3/V4 ABI shapes live in the choreography fns).
impl TickWordTransport for ConstructionIo {
    async fn read_bitmap(
        &self,
        key: &TickWordKey,
        word: i16,
        block: u64,
    ) -> Result<U256, ProviderError> {
        match key {
            TickWordKey::V3 { address, .. } => {
                choreography::fetch_tick_bitmap(self, *address, word, Some(block)).await
            }
            TickWordKey::V4 {
                state_view,
                pool_id,
                ..
            } => {
                choreography::fetch_v4_tick_bitmap(self, *state_view, *pool_id, word, Some(block))
                    .await
            }
        }
    }

    async fn read_tick(
        &self,
        key: &TickWordKey,
        tick: i32,
        block: u64,
    ) -> Result<(U128, i128), ProviderError> {
        match key {
            TickWordKey::V3 { address, .. } => {
                choreography::fetch_tick_data(self, *address, tick, Some(block)).await
            }
            TickWordKey::V4 {
                state_view,
                pool_id,
                ..
            } => {
                choreography::fetch_v4_tick_data(self, *state_view, *pool_id, tick, Some(block))
                    .await
            }
        }
    }
}
