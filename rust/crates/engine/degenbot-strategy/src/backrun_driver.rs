//! The backrun strategy driver: the loop half of the standalone sidecar,
//! lifted out of the binary so a host can register it.
//!
//! A driver owns everything the loop touches: the frame-replay runtime, the
//! quarantine FSM, the per-frame funnel, and the loop that services them.
//! The pending-tx streams and the head clock are the host's **Hosted
//! sources** (one typed stream per source kind, one head watch + fallback
//! poller per process); the driver drains its arm's stream and reads the
//! hub's latest head. The process boot (config load, tracing, the shared
//! node join, the connector registry) stays with the host; this module is
//! handed the already-minted hub, registry, node join, and its arm's stream
//! and does not own them.
//!
//! This module is the seam map, not the home. The internals live in three
//! partitions, each carrying its own invariant header:
//!
//! - `driver_boot` - the boot handoff: the resolved node join, the route
//!   registry resolvers, the boot recipe, and the host spawn factory. It
//!   turns host-minted artifacts into a runner without owning them.
//! - `driver_loop` - the loop and everything it touches: the hosted
//!   pending-tx stream's drain, the head-clock reader, the frame-replay
//!   runtime, the quarantine FSM, the per-frame funnel, and the lifecycle
//!   the handle drives.
//! - `driver_policy` - the bid's economics: the pricing reads, the
//!   broadcast relay fan-out, and the bundle target a decided frame is
//!   submitted against.
//!
//! [`BackrunDriver::start`] performs the driver boot, then returns a
//! [`DriverHandle`] whose [`wait`](DriverHandle::wait) drives the loop. The
//! bin polls the returned future inline, so a driver panic still unwinds the
//! process exactly as a bin panic did; the handle's stop flag is the seam a
//! multi-strategy host will drive the same loop through.

mod driver_boot;
mod driver_loop;
mod driver_policy;
mod node_capability;

#[cfg(test)]
mod tests;

pub use driver_boot::{
    backrun_boot, backrun_spawn_factory, resolve_backrun_boot, resolve_backrun_node_join,
    BackrunBoot, BackrunBootError, BackrunBootResources, BackrunEcosystem, BackrunNodeJoin,
    BackrunStrategyBoot,
};
pub use driver_loop::{BackrunDriver, DriverHandle, LoopDecline, LoopPhase};
pub use node_capability::{AnyRequestTransport, NodeCapability, RequestProviderFuture};
