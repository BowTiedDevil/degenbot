//! `degenbot-eventhub` — the per-process event hub.
//!
//! Phase B's fan-out vocabulary: one [`Hub`] per host process, sources
//! register once with a named [`OverflowPolicy`], and strategies/drivers
//! [`Hub::subscribe`] to a policy-typed receiver. The hub is transport-pure
//! — it knows nothing about `BotState`, the stage machine, Python, or
//! pyo3 — mirroring the `degenbot-ingestion` boundary lesson: the hub
//! carries events, the runtime decides what they mean.
//!
//! # The contract
//!
//! - **Vocabulary, never raw substrate.** [`HubEvent`] names the classes a
//!   registered source can emit ([`HubClass::PoolEvent`],
//!   [`HubClass::PendingTx`] keyed by [`PendingTxSource`]) and carries
//!   exactly the fields today's sources carry; it is not a raw WS/log
//!   passthrough and no field is invented.
//! - **The head clock lives here.** The hub owns the one head source
//!   ([`Hub::claim_head_source`] is the permanent claim, [`Hub::subscribe_head`]
//!   the infallible subscription) and the staleness clock ([`HeadSubscription::stale`]).
//!   A claimed clock's subscriptions close when every publisher drops.
//! - **Registration declares strictness.** Every registered source names its
//!   [`OverflowPolicy`] when it registers (once per class per process). A
//!   second registration of the same class is refused; subscribing to an
//!   unregistered class is refused.
//! - **Single-process only.** Processes cannot share sockets, so the hub is
//!   per-process; cross-process co-location is later-phase work.
//!
//! Deferred to later slices (not yet vocabulary): a `Lifecycle`
//! (`Resumed|Backfilling|Drained`) event and an `EnterDegraded` policy. No
//! source emits either today, so adding them here would invent state.
//!
//! # Overflow policies
//!
//! [`OverflowPolicy::DropOldestCounted`] is the explicit bounded ring (the
//! `MEVBlocker` feed's move in this slice); [`OverflowPolicy::UnboundedFlagged`]
//! is a deliberate, named audit flag rather than a convenience. The declared
//! policy is visible via [`Hub::policy_of`] and [`Hub::unbounded_flagged_count`].

pub mod event;
pub mod head;
pub mod hub;
pub mod named;
pub mod policy;

pub use event::{HubClass, HubEvent, PendingTx, PendingTxSource};
pub use head::{Head, HeadMetadata, HeadSender, HeadSubscription};
pub use hub::{
    AlreadyClaimed, DropOldestReceiver, DropOldestSender, Hub, SourceHandle, Subscription,
    UnboundedReceiver, UnboundedSender,
};
pub use named::{NamedReceiver, NamedSender};
pub use policy::{HubError, OverflowPolicy};
