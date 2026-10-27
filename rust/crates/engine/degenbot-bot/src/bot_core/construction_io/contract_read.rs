//! The construction transports' [`ContractRead`] impls and the tick-paced view.
//!
//! The ABI read family (`degenbot_rpc::abi`) is generic over [`ContractRead`] —
//! the one call primitive — so a construction-side read runs the same
//! encode→call→decode code the standalone consumer runs over an
//! `AlloyProvider`. These impls are the construction side of that seam: the
//! handle's own (unpaced) read, the `dyn RpcConstruction` trait object the
//! handle holds (and so its `Arc`), and [`PacedRead`], the tick-RPC-paced view
//! of the handle.
//!
//! # Why the pace lives here
//!
//! `builders.tick_fetch_min_interval_ms` bounds a constrained archive source's
//! tick-RPC rate ([`TickFetchPace`](super::TickFetchPace)), so pacing is a
//! property of the construction transport rather than of whichever fetcher
//! remembered to sleep: every tick-word read reaches the chain through
//! [`PacedRead`], while the token-metadata, reserves and slot0 reads the
//! interval is not meant to delay drive the unpaced impl.

use alloy::primitives::{Address, Bytes};
use degenbot_core::errors::ProviderResult;
use degenbot_rpc::abi::ContractRead;

use super::handle::ConstructionIo;
use super::RpcConstruction;

/// A construction read: the handle's generic `eth_call`, unpaced.
impl ContractRead for ConstructionIo {
    async fn call(&self, to: Address, data: Bytes, block: Option<u64>) -> ProviderResult<Bytes> {
        self.rpc.call(to, data, block).await
    }
}

/// The shared RPC half reads as the trait object it is, so a consumer holding
/// `Arc<dyn RpcConstruction>` (rather than a whole handle) reaches the ABI read
/// family. The `Arc` impl the read primitive already carries covers the shared
/// handle itself; this is the impl it lands on.
impl ContractRead for dyn RpcConstruction + Send + Sync + '_ {
    async fn call(&self, to: Address, data: Bytes, block: Option<u64>) -> ProviderResult<Bytes> {
        RpcConstruction::call(self, to, data, block).await
    }
}

/// The tick-scoped paced view of one construction handle: every read waits for
/// the next tick-fetch slot before the underlying `eth_call`.
///
/// A wrapper rather than a mode on the handle, because only the tick-word reads
/// are what the operator's [`TickFetchPace`](super::TickFetchPace) bounds.
pub struct PacedRead<'a>(&'a ConstructionIo);

impl<'a> PacedRead<'a> {
    /// The paced view over `io`.
    #[must_use]
    pub const fn new(io: &'a ConstructionIo) -> Self {
        Self(io)
    }
}

impl ContractRead for PacedRead<'_> {
    async fn call(&self, to: Address, data: Bytes, block: Option<u64>) -> ProviderResult<Bytes> {
        self.0.tick_fetch_pace().wait().await;
        // The pace belongs to this view; the call itself is the handle's own.
        ConstructionIo::call(self.0, to, data, block).await
    }
}
