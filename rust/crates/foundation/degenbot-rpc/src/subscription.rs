//! Subscription support for the Alloy provider.
//!
//! # Design
//!
//! One lifecycle, one authority: [`SubscriptionHandle`]. Every pump kind and
//! every read path (fast `__anext__`, slow `__anext__`, bulk `drain`) changes
//! subscription state only through [`SubscriptionHandle::apply`] and reads the
//! same latched [`SubscriptionPhase`], so a terminal reached through one read
//! path is the terminal every other read path reports.
//!
//! # The three planes
//!
//! - **The lifecycle** ([`SubscriptionPhase`] plus the latched startup result)
//!   is published on a retained `tokio::sync::watch`, so waiting never consumes
//!   it: an early, late, or repeated `started()` waiter all observe the one
//!   resolved startup outcome.
//! - **The data plane** is a double buffer (the pump writes one side, a drain
//!   takes the other) followed by a pending queue holding drained-but-unread
//!   items. Its atomics are flags — the active buffer index and the coalesced
//!   wake-up signal — never lifecycle authority.
//! - **The read side** is claimed by one reader at a time
//!   ([`SubscriptionHandle::claim_read`]), so a concurrent read receives the
//!   typed [`ReadInProgress`] refusal instead of mistaking a checked-out
//!   receiver for a closed subscription.
//!
//! # Reconnect is not terminal
//!
//! A watchdog stall moves [`SubscriptionPhase::Streaming`] to
//! [`SubscriptionPhase::Reconnecting`] and a successful resubscribe moves back;
//! a reader keeps awaiting data across that transition instead of surfacing a
//! terminal error. Only [`SubscriptionPhase::Ended`],
//! [`SubscriptionPhase::Failed`] and [`SubscriptionPhase::Unsubscribed`] end
//! iteration, and they are absorbing: a late pump write cannot reopen them, and
//! a reader that observed one keeps observing it.
//!
//! # Lock order
//!
//! Critical sections acquire `lifecycle` then `pending` then `buffers[*]`, and
//! never the reverse. No lock is held across an `.await`: a reader's wait uses
//! [`Notify`], which stores a permit rather than a guard.
//!
//! # One publication order
//!
//! [`SubscriptionHandle::apply`] writes the state AND publishes it inside one
//! critical section (a `watch` send is synchronous and cannot block), so the
//! retained publication has exactly the authority's total order. Two concurrent
//! applies can never publish in the opposite order from their state writes, and a
//! subscriber can never read a phase older than the last transition.

use crate::provider::AlloyProvider;
use degenbot_core::op_warn;

use alloy::network::Ethereum;
use alloy::primitives::B256;
use alloy::providers::Provider;
use alloy::rpc::types::Filter;
use futures_util::StreamExt;
use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use degenbot_core::runtime::get_runtime;
use tokio::sync::{watch, Notify};
use tokio::task::JoinHandle;
use tokio::time::timeout;

/// Maximum elapsed time with no header before the watchdog tears down + reconnects
/// a stalled `newHeads` subscription.
///
/// ~4× the ~12s Ethereum mainnet block time = ~48s, within the ≤~60s target. A silently half-open socket (NAT idle timeout, provider stall with no
/// close frame) makes `stream.next()` never resolve and never return `None`; this
/// watchdog bounds that window so the pump reconnects instead of hanging forever.
pub(crate) const HEADER_WATCHDOG_SECS: u64 = 48;

/// A boxed, `'static`, `Send` stream of Ethereum block headers.
///
/// Both the initial `subscribe_blocks().into_stream()` and every reconnect yield
/// this same concrete type so [`pump_header_stream`]'s generic `S` unifies across
/// the initial and re-established streams.
pub(crate) type HeaderStream = futures_util::stream::BoxStream<'static, alloy::rpc::types::Header>;

// ---------------------------------------------------------------------------
// Lifecycle: the subscription's sole state authority
// ---------------------------------------------------------------------------

/// The lifecycle phase a [`SubscriptionHandle`] publishes.
///
/// [`Self::Ended`], [`Self::Failed`] and [`Self::Unsubscribed`] are terminal and
/// absorbing; [`Self::Reconnecting`] is transient on the way back to
/// [`Self::Streaming`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubscriptionPhase {
    /// Spawned; the pump has not yet confirmed the subscription.
    Starting,
    /// Subscribed and delivering items.
    Streaming,
    /// The stream stalled or dropped and the pump is re-subscribing. Transient:
    /// a successful resubscribe returns to [`Self::Streaming`].
    Reconnecting { message: String },
    /// The stream delivered its terminal end.
    Ended,
    /// The subscription failed unrecoverably.
    Failed { message: String },
    /// The consumer unsubscribed.
    Unsubscribed,
}

impl SubscriptionPhase {
    /// Whether this phase ends the subscription for good.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Ended | Self::Failed { .. } | Self::Unsubscribed)
    }

    /// The phase name, as `__repr__` and telemetry report it.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Streaming => "streaming",
            Self::Reconnecting { .. } => "reconnecting",
            Self::Ended => "ended",
            Self::Failed { .. } => "failed",
            Self::Unsubscribed => "unsubscribed",
        }
    }

    /// The phase's diagnostic message, when it carries one.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Reconnecting { message } | Self::Failed { message } => Some(message),
            Self::Starting | Self::Streaming | Self::Ended | Self::Unsubscribed => None,
        }
    }
}

impl std::fmt::Display for SubscriptionPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.message() {
            Some(message) => write!(f, "{} ({message})", self.name()),
            None => f.write_str(self.name()),
        }
    }
}

/// An event that drives the lifecycle. [`SubscriptionHandle::apply`] is the only
/// writer, so every pump kind reports the same vocabulary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LifecycleEvent {
    /// The pump established the subscription (or re-established it).
    Started,
    /// The initial subscribe failed unrecoverably.
    StartupFailed { message: String },
    /// A watchdog stall tore the stream down; a resubscribe is in flight.
    Stall { message: String },
    /// A resubscribe succeeded after a [`Self::Stall`].
    Resumed,
    /// The stream delivered its terminal end.
    End,
    /// An unrecoverable stream or transport failure.
    Fail { message: String },
    /// The consumer unsubscribed.
    Unsubscribe,
}

/// How startup resolved. Latched exactly once, and never revised: a subscription
/// that streamed and later failed still reports [`Self::Started`] to a waiter
/// that asked later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartupResult {
    /// The subscription reached [`SubscriptionPhase::Streaming`].
    Started,
    /// The subscription failed before it ever streamed.
    Failed { message: String },
    /// The subscription was torn down before startup resolved.
    Closed { phase: SubscriptionPhase },
}

/// Why a lifecycle event was refused.
///
/// A refusal is data, not a falsy return: the caller can branch on the variant,
/// and both variants carry the phase the event was refused in plus the event.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LifecycleDecline {
    /// The subscription is already over. Terminal phases are absorbing, so no
    /// later event reopens them.
    #[error("subscription is already over ({phase}), so {event:?} cannot reopen it")]
    Terminal {
        /// The terminal phase that absorbed the event.
        phase: SubscriptionPhase,
        /// The refused event.
        event: LifecycleEvent,
    },
    /// The event is admissible in principle but means nothing in this phase (a
    /// duplicate start ack, a resume with nothing to resume).
    #[error("{event:?} is not admissible while {phase}")]
    Inadmissible {
        /// The phase the event could not apply in.
        phase: SubscriptionPhase,
        /// The refused event.
        event: LifecycleEvent,
    },
}

impl LifecycleDecline {
    /// The phase the event was refused in.
    #[must_use]
    pub const fn phase(&self) -> &SubscriptionPhase {
        match self {
            Self::Terminal { phase, .. } | Self::Inadmissible { phase, .. } => phase,
        }
    }

    /// The refused event.
    #[must_use]
    pub const fn event(&self) -> &LifecycleEvent {
        match self {
            Self::Terminal { event, .. } | Self::Inadmissible { event, .. } => event,
        }
    }
}

/// One published lifecycle snapshot: the current phase plus the latched startup
/// result (`None` while startup is still pending).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscriptionLifecycle {
    phase: SubscriptionPhase,
    startup: Option<StartupResult>,
}

impl SubscriptionLifecycle {
    /// A freshly spawned subscription: starting, with startup unresolved.
    #[must_use]
    pub const fn starting() -> Self {
        Self {
            phase: SubscriptionPhase::Starting,
            startup: None,
        }
    }

    /// The current phase.
    #[must_use]
    pub const fn phase(&self) -> &SubscriptionPhase {
        &self.phase
    }

    /// The latched startup result, or `None` while startup is pending.
    #[must_use]
    pub const fn startup(&self) -> Option<&StartupResult> {
        self.startup.as_ref()
    }

    /// Whether the current phase ends the subscription for good.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        self.phase.is_terminal()
    }

    /// The typed refusal for an event [`Self::next`] returns `None` for.
    #[must_use]
    pub fn decline(&self, event: &LifecycleEvent) -> LifecycleDecline {
        if self.phase.is_terminal() {
            LifecycleDecline::Terminal {
                phase: self.phase.clone(),
                event: event.clone(),
            }
        } else {
            LifecycleDecline::Inadmissible {
                phase: self.phase.clone(),
                event: event.clone(),
            }
        }
    }

    /// The lifecycle after `event`, or `None` when the event is not admissible
    /// (terminal phases absorb every event, and a duplicate ack changes nothing).
    ///
    /// The startup result is derived here rather than stored by the pump: it
    /// latches to the first non-pending phase and never changes again.
    #[expect(
        clippy::match_same_arms,
        reason = "each no-op arm names a distinct rule of the lifecycle table"
    )]
    #[must_use]
    pub fn next(&self, event: &LifecycleEvent) -> Option<Self> {
        use LifecycleEvent as E;
        use SubscriptionPhase as P;

        let phase = match (&self.phase, event) {
            // Terminal phases are absorbing: no event reopens them.
            (P::Ended | P::Failed { .. } | P::Unsubscribed, _) => return None,

            // The pump's start ack, from a fresh or a re-subscribing handle.
            (P::Starting, E::Started) | (P::Reconnecting { .. }, E::Started | E::Resumed) => {
                P::Streaming
            }
            (P::Streaming, E::Started | E::Resumed) | (P::Starting, E::Resumed) => return None,

            // Startup failure: only admissible while the handle is starting.
            (P::Starting, E::StartupFailed { message } | E::Fail { message }) => P::Failed {
                message: message.clone(),
            },
            (P::Streaming | P::Reconnecting { .. }, E::StartupFailed { .. }) => return None,

            // Unrecoverable stream failure.
            (P::Streaming | P::Reconnecting { .. }, E::Fail { message }) => P::Failed {
                message: message.clone(),
            },

            // Transient stall: streaming stalls, and a re-stall only updates the
            // diagnostic message.
            (P::Starting | P::Streaming, E::Stall { message }) => P::Reconnecting {
                message: message.clone(),
            },
            (P::Reconnecting { message: current }, E::Stall { message }) if current != message => {
                P::Reconnecting {
                    message: message.clone(),
                }
            }
            (P::Reconnecting { .. }, E::Stall { .. }) => return None,

            // Terminal stream end and consumer unsubscribe, from any live phase.
            (P::Starting | P::Streaming | P::Reconnecting { .. }, E::End) => P::Ended,
            (P::Starting | P::Streaming | P::Reconnecting { .. }, E::Unsubscribe) => {
                P::Unsubscribed
            }
        };

        let mut next = Self {
            phase,
            startup: self.startup.clone(),
        };
        if next.startup.is_none() {
            next.startup = match &next.phase {
                P::Starting | P::Reconnecting { .. } => None,
                P::Streaming => Some(StartupResult::Started),
                P::Failed { message } => Some(StartupResult::Failed {
                    message: message.clone(),
                }),
                P::Ended | P::Unsubscribed => Some(StartupResult::Closed {
                    phase: next.phase.clone(),
                }),
            };
        }
        Some(next)
    }
}

/// A transient lifecycle notice a read path reports without ending iteration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DrainNotice {
    /// The stream stalled and a resubscribe is in flight: data may resume.
    Reconnecting { message: String },
    /// The subscription is over.
    Terminal(TerminalNotice),
}

impl DrainNotice {
    /// The notice the phase carries, if any.
    #[must_use]
    pub fn from_phase(phase: &SubscriptionPhase) -> Option<Self> {
        match phase {
            SubscriptionPhase::Starting | SubscriptionPhase::Streaming => None,
            SubscriptionPhase::Reconnecting { message } => Some(Self::Reconnecting {
                message: message.clone(),
            }),
            SubscriptionPhase::Ended => Some(Self::Terminal(TerminalNotice::Ended)),
            SubscriptionPhase::Unsubscribed => Some(Self::Terminal(TerminalNotice::Unsubscribed)),
            SubscriptionPhase::Failed { message } => Some(Self::Terminal(TerminalNotice::Failed {
                message: message.clone(),
            })),
        }
    }
}

/// A terminal lifecycle notice: the subscription is over for good.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalNotice {
    /// The stream delivered its terminal end.
    Ended,
    /// The consumer unsubscribed.
    Unsubscribed,
    /// The subscription failed unrecoverably.
    Failed { message: String },
}

/// What one read of the subscription produced.
// `RawSubItem` is large; boxing it would allocate once per item read, so the
// variant stays inline and the outcome is returned by move.
#[expect(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum ReadOutcome {
    /// The next pre-terminal item.
    Item(RawSubItem),
    /// The subscription is over; iteration does not resume.
    Terminal(TerminalNotice),
}

/// The typed refusal a concurrent read receives while another read owns the
/// subscription.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("another read of this subscription is in progress")]
pub struct ReadInProgress;

// ---------------------------------------------------------------------------
// Raw subscription items (stored in buffers, no Python conversion yet)
// ---------------------------------------------------------------------------

/// Raw items from subscription streams, before Python conversion.
#[derive(Debug)]
pub enum RawSubItem {
    /// A block header.
    Header(alloy::rpc::types::Header),
    /// A full block.
    Block(alloy::rpc::types::Block),
    /// A pending transaction hash.
    PendingTxHash(B256),
    /// A full pending transaction.
    FullPendingTx(parking_lot::Mutex<Option<serde_json::Value>>),
    /// A log.
    Log(alloy::rpc::types::Log),
}

/// One drain: the items buffered before any lifecycle notice, plus the notice
/// that applies to them.
#[derive(Debug)]
pub struct RawDrain {
    /// Pre-terminal items, in arrival order.
    pub items: Vec<RawSubItem>,
    /// The lifecycle notice observed with this drain, if the phase carries one.
    pub notice: Option<DrainNotice>,
}

// ---------------------------------------------------------------------------
// The subscription handle
// ---------------------------------------------------------------------------

/// Test-only hook, fired by [`SubscriptionHandle::apply`] immediately before the
/// transition is published.
///
/// It exists so a regression test can deterministically interleave a second
/// `apply` into the state-write/publication window: if publication can be
/// displaced, that second apply completes before this one publishes and the
/// retained snapshot lands behind the authoritative state.
#[cfg(test)]
#[derive(Default)]
struct PublishHook(parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>);

#[cfg(test)]
impl std::fmt::Debug for PublishHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PublishHook(..)")
    }
}

#[cfg(test)]
impl PublishHook {
    /// Run the installed hook, if any.
    ///
    /// The closure is cloned out of the hook mutex before it runs, so a hook that
    /// installs or clears itself cannot deadlock.
    fn run(&self) {
        let hook = self.0.lock().clone();
        if let Some(hook) = hook {
            hook();
        }
    }
}

/// Manages a subscription's lifecycle, double buffer, and background pump task.
///
/// See the module docs for the plane split, the lock order, and the single
/// publication order. The pump writes items and lifecycle events; a reader claims
/// the read side, awaits items, and observes the latched phase.
#[derive(Debug)]
pub struct SubscriptionHandle {
    /// The lifecycle authority. Mutated only through [`Self::apply`].
    lifecycle: parking_lot::Mutex<SubscriptionLifecycle>,
    /// Retained publication of the lifecycle: every waiter, early or late, sees
    /// the same snapshot.
    lifecycle_tx: watch::Sender<SubscriptionLifecycle>,
    /// Items drained from the buffers but not yet handed to the reader.
    pending: parking_lot::Mutex<VecDeque<RawSubItem>>,
    /// Double buffer: the pump writes `buffers[active]`, a drain takes the other.
    buffers: [parking_lot::Mutex<Vec<RawSubItem>>; 2],
    /// Index of the active buffer (0 or 1).
    active: AtomicUsize,
    /// Coalesced data-plane signal: set on write, cleared on drain.
    signaled: AtomicBool,
    /// Wake-up for readers: the pump signals both items and lifecycle changes.
    /// `Notify` stores a permit, so a wake-up that lands between a drain and a
    /// wait is never lost.
    wake: Notify,
    /// The pump task — aborted on unsubscribe.
    task: parking_lot::Mutex<Option<JoinHandle<()>>>,
    /// The current read owner (see [`Self::claim_read`]).
    read_owner: AtomicBool,
    /// Test-only publication hook (see [`PublishHook`]).
    #[cfg(test)]
    publish_hook: PublishHook,
}

impl Default for SubscriptionHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl SubscriptionHandle {
    /// A handle with no pump task attached: the lifecycle starts at
    /// [`SubscriptionPhase::Starting`].
    ///
    /// [`spawn_subscription_arc`] is the production constructor; tests drive the
    /// handle through the public pump, lifecycle, and drain seams.
    #[must_use]
    pub fn new() -> Self {
        let lifecycle = SubscriptionLifecycle::starting();
        let (lifecycle_tx, _initial_receiver) = watch::channel(lifecycle);
        Self {
            lifecycle: parking_lot::Mutex::new(SubscriptionLifecycle::starting()),
            lifecycle_tx,
            pending: parking_lot::Mutex::new(VecDeque::new()),
            buffers: [
                parking_lot::Mutex::new(Vec::new()),
                parking_lot::Mutex::new(Vec::new()),
            ],
            active: AtomicUsize::new(0),
            signaled: AtomicBool::new(false),
            wake: Notify::new(),
            task: parking_lot::Mutex::new(None),
            read_owner: AtomicBool::new(false),
            #[cfg(test)]
            publish_hook: PublishHook::default(),
        }
    }

    /// The current lifecycle snapshot.
    #[must_use]
    pub fn lifecycle(&self) -> SubscriptionLifecycle {
        self.lifecycle.lock().clone()
    }

    /// The current lifecycle phase.
    #[must_use]
    pub fn phase(&self) -> SubscriptionPhase {
        self.lifecycle.lock().phase().clone()
    }

    /// Whether the subscription has reached an absorbing terminal phase.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.lifecycle.lock().is_terminal()
    }

    /// A retained subscription to lifecycle changes — the multi-waiter seam.
    #[must_use]
    pub fn subscribe_lifecycle(&self) -> watch::Receiver<SubscriptionLifecycle> {
        self.lifecycle_tx.subscribe()
    }

    /// Install a hook fired between a transition and its publication (tests).
    #[cfg(test)]
    fn set_publish_hook(&self, hook: impl Fn() + Send + Sync + 'static) {
        *self.publish_hook.0.lock() = Some(Arc::new(hook));
    }

    /// Apply one lifecycle event. The ONLY writer of the lifecycle.
    ///
    /// The state write and its publication happen in ONE critical section: a
    /// `watch` send is synchronous and cannot block, so publishing under the
    /// authority lock makes the publication's total order exactly the authority's.
    /// A subscriber therefore can never read a phase older than the authority's
    /// last transition, even when two applies run concurrently.
    ///
    /// A refused event changes nothing, which is what makes a late pump write —
    /// or a start ack that arrives after [`Self::unsubscribe`] — unable to report
    /// the subscription active again.
    ///
    /// # Errors
    ///
    /// [`LifecycleDecline::Terminal`] when the subscription is already over, and
    /// [`LifecycleDecline::Inadmissible`] when the event means nothing in the
    /// current phase. Both carry the phase and the event.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "call sites pass an event literal; borrowing every one is noise"
    )]
    pub fn apply(&self, event: LifecycleEvent) -> Result<(), LifecycleDecline> {
        // Lock order: lifecycle -> publish hook (test only) -> pending ->
        // buffers -> the value publication.
        let mut current = self.lifecycle.lock();
        let Some(next) = current.next(&event) else {
            return Err(current.decline(&event));
        };
        *current = next.clone();
        if next.is_terminal() {
            // Terminal is absorbing: flush whatever the pump already wrote into
            // the pending queue so pre-terminal items are still delivered ahead
            // of the terminal notice. A later write is refused by `buffer_item`,
            // which reads the phase under this same lock.
            let mut pending = self.pending.lock();
            for buffer in &self.buffers {
                let mut buffer = buffer.lock();
                pending.extend(buffer.drain(..));
            }
        }

        #[cfg(test)]
        self.publish_hook.run();

        // Publish while the authority lock is held: a later apply cannot slip its
        // state in behind this one.
        let _previous = self.lifecycle_tx.send_replace(next);
        drop(current);

        // A lifecycle change is work for one reader (data may follow a
        // reconnect, and a terminal ends the wait).
        self.wake.notify_one();
        Ok(())
    }

    /// Claim the read side of the subscription.
    ///
    /// One reader at a time: a second concurrent read gets the typed
    /// [`ReadInProgress`] refusal instead of racing the buffer or mistaking a
    /// checked-out reader for a closed subscription. The claim is released when
    /// the returned guard drops.
    ///
    /// # Errors
    ///
    /// [`ReadInProgress`] when another read already owns the read side.
    pub fn claim_read(self: &Arc<Self>) -> Result<ReadClaim, ReadInProgress> {
        if self
            .read_owner
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            Ok(ReadClaim {
                handle: Arc::clone(self),
            })
        } else {
            Err(ReadInProgress)
        }
    }

    /// Await the next wake-up: a buffered item or a lifecycle change.
    pub async fn wait_for_work(&self) {
        self.wake.notified().await;
    }

    /// The next item, or the terminal notice that ends iteration.
    ///
    /// This is the ONE read loop every read path funnels through: buffered items
    /// first (so a batch that arrived before the terminal is never dropped), then
    /// the latched phase. [`SubscriptionPhase::Reconnecting`] deliberately does
    /// not end iteration — the loop keeps awaiting data, which is the intentional
    /// difference from surfacing a transient stall as a terminal error.
    pub async fn read_next(&self) -> ReadOutcome {
        loop {
            if let Some(item) = self.pop_item() {
                return ReadOutcome::Item(item);
            }
            match DrainNotice::from_phase(&self.phase()) {
                Some(DrainNotice::Terminal(terminal)) => return ReadOutcome::Terminal(terminal),
                Some(DrainNotice::Reconnecting { .. }) | None => self.wait_for_work().await,
            }
        }
    }

    /// Drain everything buffered, with the typed lifecycle notice that applies to
    /// it.
    ///
    /// The notice comes from the same authority the pump writes, so the terminal a
    /// slow `__anext__` observed is the terminal this bulk drain reports.
    #[must_use]
    pub fn drain_raw(&self) -> RawDrain {
        let items = {
            let mut pending = self.refill_pending();
            pending.drain(..).collect::<Vec<_>>()
        };

        // The phase read acquires `lifecycle` only after `pending` and the
        // buffers are released, so no critical section inverts the lock order.
        RawDrain {
            items,
            notice: DrainNotice::from_phase(&self.phase()),
        }
    }

    /// Wait for startup to resolve — safe for early, late, and repeated waiters.
    ///
    /// Every waiter observes the same latched outcome (the publication is
    /// retained, not consumed), so there is no "notification consumed" ambiguity
    /// and no take-once channel.
    pub async fn await_startup(&self) -> StartupResult {
        let mut receiver = self.subscribe_lifecycle();
        loop {
            {
                let snapshot = receiver.borrow_and_update();
                if let Some(resolved) = snapshot.startup() {
                    return resolved.clone();
                }
            }
            if receiver.changed().await.is_err() {
                // The publishing sender is gone: report the last latched state
                // rather than waiting forever.
                let snapshot = self.lifecycle();
                return snapshot
                    .startup()
                    .cloned()
                    .unwrap_or_else(|| StartupResult::Closed {
                        phase: snapshot.phase().clone(),
                    });
            }
        }
    }

    /// Unsubscribe: latch [`SubscriptionPhase::Unsubscribed`] and abort the pump.
    ///
    /// Idempotent, and terminal against a late pump event: the abort stops the
    /// task, and the absorbing phase refuses whatever the pump managed to
    /// publish before it died.
    pub fn unsubscribe(&self) {
        // A refused report means the subscription is already over: the absorbing
        // phase is the authority, and the abort below is still the right cleanup.
        let _ = self.apply(LifecycleEvent::Unsubscribe);
        let task = self.task.lock().take();
        if let Some(task) = task {
            task.abort();
        }
    }

    /// Publish one item without a transport (feature `test-utils`).
    ///
    /// Goes through the same write path a pump uses, so a downstream crate's test
    /// can drive a read path with a real item without standing up a node.
    #[cfg(feature = "test-utils")]
    pub fn push_item(&self, item: RawSubItem) {
        buffer_item(self, item);
    }

    /// The next buffered item, refilling from the double buffer first.
    fn pop_item(&self) -> Option<RawSubItem> {
        self.refill_pending().pop_front()
    }

    /// Clear the coalesced signal and merge both buffers into the pending queue.
    ///
    /// Clearing the signal BEFORE taking means a write that lands during this
    /// take sets it again and sends a fresh wake-up, so a reader that parks after
    /// a completed read can never sleep on an item it never saw. Lock order:
    /// `pending` -> `buffers[*]`.
    fn refill_pending(&self) -> parking_lot::MutexGuard<'_, VecDeque<RawSubItem>> {
        self.signaled.store(false, Ordering::Relaxed);
        let mut pending = self.pending.lock();
        for buffer in &self.buffers {
            let mut buffer = buffer.lock();
            pending.extend(buffer.drain(..));
        }
        pending
    }
}

/// Exclusive ownership of a subscription's read side, released on drop.
#[derive(Debug)]
pub struct ReadClaim {
    handle: Arc<SubscriptionHandle>,
}

impl ReadClaim {
    /// The handle this claim owns.
    #[must_use]
    pub fn handle(&self) -> &Arc<SubscriptionHandle> {
        &self.handle
    }
}

impl Drop for ReadClaim {
    fn drop(&mut self) {
        self.handle.read_owner.store(false, Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// Pump task helpers
// ---------------------------------------------------------------------------

/// Write one item to the active buffer.
///
/// Refuses once the lifecycle is terminal. The phase is read under the lifecycle
/// lock the pump holds for the whole push, so a write either lands before the
/// terminal transition — where [`SubscriptionHandle::apply`]'s flush captures it
/// — or is dropped: post-terminal data can never appear behind a terminal notice.
fn buffer_item(handle: &SubscriptionHandle, item: RawSubItem) {
    {
        let lifecycle = handle.lifecycle.lock();
        if lifecycle.is_terminal() {
            return;
        }
        let active = handle.active.load(Ordering::Relaxed);
        handle.buffers[active].lock().push(item);
    }

    // Coalesced signal: wake a reader only on the false -> true transition.
    if !handle.signaled.swap(true, Ordering::Relaxed) {
        handle.wake.notify_one();
    }
}

/// Report an event the pump expects to be admissible, tolerating a refusal.
///
/// A refusal means the handle is already terminal — a late pump event against an
/// absorbing phase — which is not an error for a pump: the terminal is the
/// authority and the report is stale.
fn report(handle: &SubscriptionHandle, event: LifecycleEvent) {
    let _ = handle.apply(event);
}

/// Signal that the pump established the subscription.
fn signal_started(handle: &SubscriptionHandle) {
    report(handle, LifecycleEvent::Started);
}

/// Signal that the chain-side subscribe failed.
fn signal_start_failed(handle: &SubscriptionHandle, message: String) {
    report(handle, LifecycleEvent::StartupFailed { message });
}

/// Drive one normalized item stream into the handle.
///
/// The shared body of every pump kind: an item is buffered, a clean stream end is
/// the terminal [`LifecycleEvent::End`], and a stream error is the terminal
/// [`LifecycleEvent::Fail`]. The loop also stops as soon as the lifecycle is
/// terminal, so an unsubscribe ends the pump without waiting for the next item.
pub async fn drive_items<S>(handle: &Arc<SubscriptionHandle>, mut items: S)
where
    S: futures_util::Stream<Item = Result<RawSubItem, String>> + Unpin + Send,
{
    loop {
        if handle.is_terminal() {
            return;
        }
        let Some(item) = items.next().await else {
            report(handle, LifecycleEvent::End);
            return;
        };
        match item {
            Ok(item) => buffer_item(handle, item),
            Err(message) => {
                report(handle, LifecycleEvent::Fail { message });
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Pump tasks for each subscription type
// ---------------------------------------------------------------------------

/// Pump task for block header subscriptions.
///
/// Drives a [`HeaderStream`] through [`pump_header_stream`] with a header
/// watchdog: a `newHeads` subscription that delivers no header within
/// [`HEADER_WATCHDOG_SECS`] is torn down and reconnected, reusing the provider's
/// existing backend reconnect path (`is_backend_gone` /
/// `is_pubsub_unavailable`) rather than a duplicate WS backoff. A stall is
/// transient — [`LifecycleEvent::Stall`] then [`LifecycleEvent::Resumed`] — so a
/// reader sees a reconnect, not a terminal close.
pub async fn pump_blocks(provider: Arc<dyn Provider<Ethereum>>, handle: Arc<SubscriptionHandle>) {
    let stream: HeaderStream = match provider.subscribe_blocks().await {
        Ok(s) => {
            signal_started(&handle);
            s.into_stream().boxed()
        }
        Err(e) => {
            signal_start_failed(&handle, format!("Failed to subscribe to blocks: {e}"));
            return;
        }
    };

    // `provider` is reused (not duplicated) by the reconnect closure: each
    // watchdog fire re-issues `subscribe_blocks` on the same provider, leaning
    // on alloy-pubsub's backend reconnect for a dead socket.
    let reconnect_provider = Arc::clone(&provider);
    pump_header_stream(
        handle,
        stream,
        Duration::from_secs(HEADER_WATCHDOG_SECS),
        move || {
            let provider = Arc::clone(&reconnect_provider);
            async move { reconnect_new_heads_stream(provider).await }
        },
    )
    .await;
}

/// Re-establish a `newHeads` [`HeaderStream`] after the watchdog tore down a
/// stalled subscription.
///
/// Reuses the provider's existing backend reconnect path: `subscribe_blocks`
/// re-issues `eth_subscribe`; if the WS backend is dead, alloy-pubsub's
/// `is_backend_gone` / `is_pubsub_unavailable` retry path reconnects the
/// transport (the same path that fires on request errors during normal RPC).
/// Each subscribe attempt is bounded by [`HEADER_WATCHDOG_SECS`] so a hung dead
/// socket (one that accepts the `eth_subscribe` write but never responds)
/// cannot re-stall the pump. Retries indefinitely with the shared backoff curve
/// ([`crate::provider::rpc_retry_policy`]) so a transiently-downed provider
/// recovers without an operator restart — the bot process stays alive.
pub(crate) async fn reconnect_new_heads_stream(
    provider: Arc<dyn Provider<Ethereum>>,
) -> Option<HeaderStream> {
    use crate::retry_policy::rpc_retry_policy;
    // This loop retries indefinitely, so the policy's attempt bound is unused;
    // only its shared curve (base/cap) is consumed.
    let policy = rpc_retry_policy(u32::MAX);
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        let delay = policy.capped_backoff(attempt);
        let delay_ms = delay.as_millis();
        match timeout(
            Duration::from_secs(HEADER_WATCHDOG_SECS),
            provider.subscribe_blocks(),
        )
        .await
        {
            Ok(Ok(s)) => return Some(s.into_stream().boxed()),
            Ok(Err(e)) => {
                op_warn!(domain = rpc, %e,
                    delay_ms,
                    "newHeads reconnect subscribe failed — retrying"
                );
            }
            Err(_) => {
                op_warn!(
                    domain = rpc,
                    delay_ms,
                    "newHeads reconnect subscribe hung — retrying"
                );
            }
        }
        tokio::time::sleep(delay).await;
    }
}

/// Outcome of [`drive_new_heads`].
pub(crate) enum HeaderDriveExit {
    /// The stream closed cleanly (`Ok(None)`) and `on_end` asked to stop
    /// rather than reconnect.
    StreamEnded,
    /// A callback asked to stop (consumer unsubscribed, or the reconnect
    /// helper gave up).
    Aborted,
}

/// Drive a `newHeads` header stream with a header watchdog, delegating what
/// each event MEANS to the caller's callbacks.
///
/// This is the single implementation of the watchdog + reconnect policy shared
/// by the FFI subscription pump ([`pump_header_stream`], which buffers raw
/// items for Python) and [`crate::head_watch::HeadWatch`] (which publishes the
/// head block number on a `tokio::sync::watch` channel). Each callback returns
/// `false` to stop; a `true` from `on_stall`/`on_end` routes through
/// `next_stream` to re-establish the subscription.
///
/// The watchdog bounds `stream.next()` so a silently-stalled socket (no close
/// frame, never resolves) is torn down within a bounded window instead of
/// hanging the driver forever.
///
/// # Cancel safety
///
/// **Mostly cancel-safe, with an honest remainder.**
///
/// *Dropping the inner `timeout(.., stream.next())` future*: `StreamExt::next`
/// is the documented-cancel-safe read — dropping it without producing an item
/// consumes nothing from the stream, so the next `next()` observes the same
/// pending/next item. The watchdog firing (`Err(_)`) drops only that `next()`
/// future; the following `next_stream()` rebuilds a fresh subscription and
/// assigns it to the same `stream` binding.
///
/// *The unsafe remainder — the buffered-items question*: the loop OWNS `stream`
/// by value, and a stream is not a resumable cursor. So when the WHOLE
/// `drive_new_heads` future is cancelled (its owning task is dropped/aborted —
/// `pump_header_stream` runs as a spawned task), `stream` is dropped with it,
/// and **any headers the underlying subscription had already buffered but this
/// loop had not yet consumed are lost** — not replayed, not re-buffered. The
/// reconnect path re-subscribes (`eth_subscribe`) and anchors on the CURRENT
/// head, so it does not backfill the delivered-but-unconsumed window either.
/// The two cancellation scopes therefore differ: cancelling the inner
/// `next()` (watchdog) is lossless, but cancelling the outer future discards
/// everything buffered past the last header handed to `on_header`.
///
/// **Caller's obligation / the fence**: `on_header` is the ONLY durable sink
/// for a header — once it returns the item is the caller's. A caller that
/// needs the delivered-but-unconsumed window must not rely on it surviving a
/// cancellation of this loop; it must persist each header at `on_header` time.
/// A clean stop is signalled by returning `false` from a callback (which yields
/// [`HeaderDriveExit`]) — not by dropping the task, which would silently
/// discard the buffered tail.
pub(crate) async fn drive_new_heads<S, F, Fut, H, D, E>(
    mut stream: S,
    watchdog: Duration,
    mut next_stream: F,
    mut on_header: H,
    mut on_stall: D,
    mut on_end: E,
) -> HeaderDriveExit
where
    S: futures_util::Stream<Item = alloy::rpc::types::Header> + Unpin + Send + 'static,
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Option<S>> + Send + 'static,
    H: FnMut(alloy::rpc::types::Header) -> bool + Send,
    D: FnMut() -> bool + Send,
    E: FnMut() -> bool + Send,
{
    loop {
        match timeout(watchdog, stream.next()).await {
            Ok(Some(header)) => {
                if !on_header(header) {
                    return HeaderDriveExit::Aborted;
                }
            }
            Ok(None) => {
                // Clean subscription end (real close frame): the caller
                // decides whether that is terminal (buffer `End`, stop) or a
                // reason to re-subscribe.
                if !on_end() {
                    return HeaderDriveExit::StreamEnded;
                }
                match next_stream().await {
                    Some(s) => stream = s,
                    None => return HeaderDriveExit::Aborted,
                }
            }
            Err(_) => {
                // Watchdog fired: no header within `watchdog`. The caller
                // decides the meaning of the gap, then we re-subscribe.
                if !on_stall() {
                    return HeaderDriveExit::Aborted;
                }
                match next_stream().await {
                    Some(s) => stream = s,
                    None => return HeaderDriveExit::Aborted,
                }
            }
        }
    }
}

/// Buffer `newHeads` headers into a [`SubscriptionHandle`] via
/// [`drive_new_heads`]: a header becomes a [`RawSubItem::Header`], a watchdog fire
/// a transient [`LifecycleEvent::Stall`] (a reconnect, not a disconnect), a
/// successful resubscribe a [`LifecycleEvent::Resumed`], and a clean stream end
/// the terminal [`LifecycleEvent::End`].
///
/// Separated from [`pump_blocks`] so the watchdog/reconnect lifecycle is unit-
/// testable with an injected never-yielding stream and a tiny threshold (no live
/// RPC — see `a_watchdog_stall_is_a_transient_reconnect_not_a_terminal_end`).
async fn pump_header_stream<S, F, Fut>(
    handle: Arc<SubscriptionHandle>,
    stream: S,
    watchdog: Duration,
    mut next_stream: F,
) where
    S: futures_util::Stream<Item = alloy::rpc::types::Header> + Unpin + Send + 'static,
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Option<S>> + Send + 'static,
{
    let header_handle = Arc::clone(&handle);
    let stall_handle = Arc::clone(&handle);
    let resume_handle = Arc::clone(&handle);
    let end_handle = Arc::clone(&handle);

    // Every re-established stream resumes the lifecycle. A reconnect that gives up
    // leaves the handle in `Reconnecting`, which the reader observes as a
    // transient phase rather than a terminal close.
    let reconnect = move || {
        let next = next_stream();
        let resume_handle = Arc::clone(&resume_handle);
        async move {
            let stream = next.await;
            if stream.is_some() {
                report(&resume_handle, LifecycleEvent::Resumed);
            }
            stream
        }
    };

    let _ = drive_new_heads(
        stream,
        watchdog,
        reconnect,
        move |header| {
            buffer_item(&header_handle, RawSubItem::Header(header));
            // Stop feeding once the handle is terminal (unsubscribe, or a terminal
            // notice the reader has already observed).
            !header_handle.is_terminal()
        },
        move || {
            report(
                &stall_handle,
                LifecycleEvent::Stall {
                    message:
                        "newHeads subscription stall: no header within watchdog window, reconnecting"
                            .to_string(),
                },
            );
            true
        },
        move || {
            report(&end_handle, LifecycleEvent::End);
            false
        },
    )
    .await;
}

/// Pump task for full block subscriptions.
pub async fn pump_full_blocks(
    provider: Arc<dyn Provider<Ethereum>>,
    handle: Arc<SubscriptionHandle>,
) {
    let stream = match provider.subscribe_full_blocks().full().into_stream().await {
        Ok(s) => {
            signal_started(&handle);
            s
        }
        Err(e) => {
            signal_start_failed(&handle, format!("Failed to subscribe to full blocks: {e}"));
            return;
        }
    };

    // Unlike a `newHeads` watchdog stall, a stream error here is terminal: the
    // subscription itself failed, so there is nothing to resubscribe.
    let items = stream.map(|item| {
        item.map(RawSubItem::Block)
            .map_err(|e| format!("Full block subscription error: {e}"))
    });
    drive_items(&handle, items).await;
}

/// Pump task for pending transaction hash subscriptions.
pub async fn pump_pending_transactions(
    provider: Arc<dyn Provider<Ethereum>>,
    handle: Arc<SubscriptionHandle>,
) {
    let stream = match provider.subscribe_pending_transactions().await {
        Ok(s) => {
            signal_started(&handle);
            s.into_stream()
        }
        Err(e) => {
            signal_start_failed(
                &handle,
                format!("Failed to subscribe to pending transactions: {e}"),
            );
            return;
        }
    };

    let items = stream.map(|hash| Ok(RawSubItem::PendingTxHash(hash)));
    drive_items(&handle, items).await;
}

/// Pump task for full pending transaction subscriptions.
pub async fn pump_full_pending_transactions(
    provider: Arc<dyn Provider<Ethereum>>,
    handle: Arc<SubscriptionHandle>,
) {
    let stream = match provider.subscribe_full_pending_transactions().await {
        Ok(s) => {
            signal_started(&handle);
            s.into_stream()
        }
        Err(e) => {
            signal_start_failed(
                &handle,
                format!("Failed to subscribe to full pending transactions: {e}"),
            );
            return;
        }
    };

    // Serialize the transaction to JSON in the pump task (no GIL); Python
    // conversion happens in the drain.
    let items = stream.map(|tx_data| {
        serde_json::to_value(&tx_data)
            .map(|value| RawSubItem::FullPendingTx(parking_lot::Mutex::new(Some(value))))
            .map_err(|e| format!("Transaction serialization error: {e}"))
    });
    drive_items(&handle, items).await;
}

/// Pump task for log subscriptions.
pub async fn pump_logs(
    provider: Arc<dyn Provider<Ethereum>>,
    filter: Filter,
    handle: Arc<SubscriptionHandle>,
) {
    let stream = match provider.subscribe_logs(&filter).await {
        Ok(s) => {
            signal_started(&handle);
            s.into_stream()
        }
        Err(e) => {
            signal_start_failed(&handle, format!("Failed to subscribe to logs: {e}"));
            return;
        }
    };

    let items = stream.map(|log| Ok(RawSubItem::Log(log)));
    drive_items(&handle, items).await;
}

// ---------------------------------------------------------------------------
// Provider-side subscription constructors
// ---------------------------------------------------------------------------

/// Create a [`SubscriptionHandle`] and spawn its pump task.
///
/// Returns the `Arc` shared between the pump task and the read side.
fn spawn_subscription_arc<F, Fut>(pump_fn: F) -> Arc<SubscriptionHandle>
where
    F: FnOnce(Arc<SubscriptionHandle>) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let handle = Arc::new(SubscriptionHandle::new());

    let task = get_runtime().spawn(pump_fn(Arc::clone(&handle)));

    *handle.task.lock() = Some(task);

    handle
}

impl AlloyProvider {
    /// Subscribe to new block headers.
    #[must_use]
    pub fn subscribe_blocks(&self) -> Arc<SubscriptionHandle> {
        let provider = self.provider_arc();
        spawn_subscription_arc(move |handle| pump_blocks(provider, handle))
    }

    /// Subscribe to full block bodies (with full transactions).
    #[must_use]
    pub fn subscribe_full_blocks(&self) -> Arc<SubscriptionHandle> {
        let provider = self.provider_arc();
        spawn_subscription_arc(move |handle| pump_full_blocks(provider, handle))
    }

    /// Subscribe to pending transaction hashes.
    #[must_use]
    pub fn subscribe_pending_transactions(&self) -> Arc<SubscriptionHandle> {
        let provider = self.provider_arc();
        spawn_subscription_arc(move |handle| pump_pending_transactions(provider, handle))
    }

    /// Subscribe to full pending transaction bodies.
    #[must_use]
    pub fn subscribe_full_pending_transactions(&self) -> Arc<SubscriptionHandle> {
        let provider = self.provider_arc();
        spawn_subscription_arc(move |handle| pump_full_pending_transactions(provider, handle))
    }

    /// Subscribe to logs matching the given filter.
    #[must_use]
    pub fn subscribe_logs(&self, filter: Filter) -> Arc<SubscriptionHandle> {
        let provider = self.provider_arc();
        spawn_subscription_arc(move |handle| pump_logs(provider, filter, handle))
    }
}

#[cfg(test)]
mod tests;
