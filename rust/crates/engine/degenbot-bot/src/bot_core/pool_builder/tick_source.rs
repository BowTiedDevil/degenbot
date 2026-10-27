//! The live-miss tick-word source over [`ConstructionIo`] — the **Tick-word
//! source** capability's miss-path call site (GLOSSARY).
//!
//! The sparse-map miss path (`swap_simulation` -> `MissingTickWord`) reaches the
//! pool state's stored [`TickWordSource`] when a walk enters an unfetched bitmap
//! word. This impl is the core's own source: UNBOUND — one instance reads any
//! pool the key names, because the key carries the pool's read descriptor
//! (V3 address + spacing, or V4 `StateView` + pool id + spacing). The legacy
//! Python path stores a `PyTickWordSource` callback instead; a pure-Rust
//! consumer wires this impl and still gets sparse-map backfill.
//!
//! The source is the SHARED adapter
//! ([`degenbot_rpc::tick_bootstrap::TransportTickWordSource`]) over this
//! module's [`ConstructionIo`] transport: the word-range check, the miss path's
//! REFUSE unreadable-bitmap policy, the error bucketing, and the sync bridge
//! live there once. The registration Chain arm
//! (`bootstrap_{v3,v4}_tick_map`) drives the same transport-generic loop, and
//! differs only in policy — it degrades an unreadable bitmap to an empty word
//! (the legacy graceful fallback). This module therefore owns exactly the
//! io-side contract reads: the paced choreography fns.

use std::sync::Arc;

use alloy::primitives::{U128, U256};
use degenbot_core::errors::ProviderError;
use degenbot_pools::sim::tick_fetch::{TickWordKey, TickWordSource};
use degenbot_rpc::tick_bootstrap::{TickWordTransport, TransportTickWordSource};

use super::choreography::{self};

use crate::bot_core::construction_io::{ConstructionIo, NoDb};

/// The core's own live-miss tick-word source over the session's construction
/// I/O — unbound, so ONE instance serves every pool the session constructs.
///
/// The DB half is unused by a tick-word read, so the handle is rebuilt from the
/// RPC half alone ([`NoDb`] supplies the no-database arm) — the source stays
/// independent of whether the session opened a database.
#[must_use]
pub fn attached(io: &ConstructionIo) -> Arc<dyn TickWordSource> {
    let rpc = Arc::clone(&io.rpc);
    Arc::new(TransportTickWordSource::from_transport(
        ConstructionIo::new(Arc::new(NoDb), rpc),
    ))
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
