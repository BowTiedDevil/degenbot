//! `degenbot-eventhub` — the per-process event hub.
//!
//! Phase B's fan-out vocabulary: one [`Hub`] per host process. A feed opens its
//! source kind's typed pending-transaction ring once
//! ([`Hub::open_pending_tx_ring`]) and publishes through the returned
//! [`PendingTxPublisher`]; the ring's one [`PendingTxView`] is the consumer's
//! drain. The head clock is the hub's own. The hub is transport-pure — it knows
//! nothing about `BotState`, the stage machine, Python, or pyo3 — mirroring the
//! `degenbot-ingestion` boundary lesson: the hub carries events, the runtime
//! decides what they mean.
//!
//! # The contract
//!
//! - **Typed payloads, never a union.** [`PendingTx`] is the ring's frame
//!   type, keyed by [`PendingTxSource`]; a consumer cannot receive another
//!   source's payload and there is no wrong-variant arm to warn about.
//! - **One ring per source kind, one drainer per ring.** The hub proves one
//!   ring per kind ([`Hub::open_pending_tx_ring`] refuses a second), and
//!   [`PendingTxView`] is deliberately not `Clone`, so one ring has exactly one
//!   consumer end.
//! - **Non-zero capacity.** A ring's capacity is a
//!   [`std::num::NonZeroUsize`]: zero is unrepresentable, not clamped.
//! - **Drop telemetry on the publisher.** Evictions are counted where the feed
//!   can read them, so feed status needs no consumer view to exist.
//! - **The head clock lives here.** The hub owns the one head source
//!   ([`Hub::claim_head_source`] is the permanent claim, [`Hub::subscribe_head`]
//!   the infallible subscription) and the staleness clock
//!   ([`HeadSubscription::stale`]). A claimed clock's subscriptions close when
//!   every publisher drops.
//! - **Single-process only.** Processes cannot share sockets, so the hub is
//!   per-process; cross-process co-location is later-phase work.
//!
//! Deferred to later slices (not yet vocabulary): a `Lifecycle`
//! (`Resumed|Backfilling|Drained`) event and an `EnterDegraded` policy. No
//! source emits either today, so adding them here would invent state.

pub mod event;
pub mod head;
pub mod hub;
pub mod pending;

pub use degenbot_core::canonical_head::ChainAnchor;
pub use event::{PendingTx, PendingTxSource};
pub use head::{Head, HeadMetadata, HeadSender, HeadSubscription};
pub use hub::{AlreadyClaimed, Hub, HubError};
pub use pending::{detached_pending_ring, PendingTxPublisher, PendingTxView};
