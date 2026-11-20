//! Hosted sources: the ENGINE-lifetime owner of the strategies' pending-tx
//! source provisioning and the head clock.
//!
//! One typed stream per source kind, minted once at boot: each active arm's
//! pump opens its own [`degenbot_eventhub::Hub::open_pending_tx_ring`] ring for
//! its [`PendingTxSource`], so two backrun arms can run in one process and each
//! arm's frame corpus is exactly its own feed's. The head clock is also
//! engine-lifetime and, in every composition root that drives an engine, owned
//! by the BLOCK PUMP: the pump registers the hub's
//! one head source and publishes each accepted header into it, so there is ONE
//! `newHeads` subscription and no polled duplicate. A boot that hosts its own
//! watch instead adds ONE stale-fallback `eth_blockNumber` poller. Either way
//! there is ONE feed-telemetry sampler — drivers never spawn feeds, watch heads,
//! or scrape status; they drain their arm's typed stream and read the hub's
//! latest head ([`Hub::subscribe_head`] is multi-subscriber).
//!
//! # Deferred start
//!
//! [`HostedSources::mint`] PROVISIONS (the hub rings, the driver streams, the
//! dedicated head-watch runtime, the clock and trigger) and dials nothing;
//! [`HostedSources::start`] launches the live I/O at the engine's start
//! boundary — each arm's feed pump, the feed-telemetry sampler, and the
//! head-watch WS connect. A process that mints an engine without resuming
//! performs ZERO network dials.
//!
//! # One lifecycle
//!
//! Provisioning, startup, partial failure, running, and teardown are ONE
//! authoritative lifecycle — `HostedLifecycle`, private to this module and
//! owned only by [`HostedSources`]: `Provisioned` → `Starting` →
//! `Running` | `HalfStarted` → `TornDown`. Each phase carries exactly the
//! resources legal in it, so no reader infers lifecycle truth from a resource's
//! presence and there is no separate started latch. Start is legal from
//! `Provisioned` and an idempotent no-op from `Running`; a concurrent start gets
//! [`HostedStartError::StartInFlight`], and a start after a launch already
//! failed gets [`HostedStartError::StartAfterFailure`] — never `Ok`. The
//! payload-free [`HostedPhase`] is the public projection.
//!
//! The head edge carries its own mode machine ([`HeadEdgeMode`]) instead of
//! inferring a mode from which resource happens to exist, and owns it — with the
//! once-only `Stopped` exit fact — in one transition value ([`HeadEdge`]): every
//! transition publishes the mode it entered, so [`HostedSources::head_edge`]
//! hands out a projection with one writer, not independent lifecycle truth.
//!
//! The head edge drives the once-per-head reconciliation through an injected
//! trigger: this crate cannot depend on `degenbot-submission` (the
//! composition root owns `HeadReconciliation`), so the boot wires
//! `HeadReconciliation::reconcile_head` in as the [`HeadTrigger`] closure and
//! this module only knows "a head was observed at the source edge, fire the
//! trigger" — once per observed head; the reconciliation's once-per-head
//! dedupe makes redundant triggers free.
//!
//! # Who owns the head source
//!
//! The hub head source has exactly one owner, named by the boot's
//! [`HostedClockBinding`]. The session boot CLAIMS the hub's one head source on
//! its freshly minted hub, so a session can never mint a clock nobody owns. The
//! session's block pump owns it in every composition root that drives an engine
//! ([`HostedClockBinding::PumpOwned`]): it publishes each ACCEPTED header, so the
//! edge, the drivers' head subscriptions, and the per-head reconciliation all
//! read the one `newHeads` subscription the pump already holds. A boot that hosts
//! its own watch or poller instead ([`HostedClockBinding::Watch`] /
//! [`HostedClockBinding::Poll`]) owns the publisher and is the only shape that
//! dials for heads.
//!
//! # Lifetime & teardown
//!
//! The boot mints one `HostedSources` and the engine holds it for its whole
//! life, so `Drop for HostedSources` runs when the ENGINE drops — not at
//! boot-host end. Drop executes ONE ordered teardown plan from EVERY phase
//! (never-started, half-started, running, already torn down): it stops every
//! pump's transport, fires a `watch` shutdown signal the head edge and the feed
//! sampler select on, drops the head-watch WS transport, and then tears down a
//! DEDICATED single-worker runtime (the shared `OwnedPubsubRuntime`), the same
//! type `AnvilFork` uses, that
//! hosts the head-watch WS transport and its alloy pubsub service. Tearing that
//! runtime down CANCELS the pubsub service and the head watch's driving task
//! deterministically, with no dependence on a contended shared-runtime worker —
//! the head edge also drops its WS transport Arc while the endpoint may still
//! be alive (the `AnvilFork` ordering discipline).

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy::network::Ethereum;
use alloy::providers::Provider;
use degenbot_core::pubsub_runtime::{OwnedPubsubRuntime, OwnedPubsubRuntimeSpec};
use degenbot_eventhub::{Head, HeadSender, HeadSubscription, Hub, HubError, PendingTxSource};
use degenbot_rpc::backrun_feed::BackrunFeedStatus;
use degenbot_rpc::head_watch::{HeadWatch, HeadWatchConfig};
use degenbot_rpc::pending_tx_stream::{PendingTxFeedConfig, PendingTxPump, PendingTxStream};
use degenbot_rpc::provider::{AlloyProvider, DEFAULT_MAX_RETRIES};
use futures_util::future::BoxFuture;
use tokio::sync::watch;

/// How long a head await may run before the edge re-checks staleness. The
/// watch resolves the instant a header arrives (~12s apart), so a healthy
/// watch leaves this to time out most iterations.
const HEAD_WAIT: Duration = Duration::from_secs(2);

/// A watch silent this long is treated as dead: the edge polls for that
/// iteration while the watch's watchdog reconnects in the background. Must
/// exceed the chain's block interval — mainnet blocks arrive ~12s apart, so
/// a threshold at or below that would poll on every block and defeat the
/// point of the subscription. Kept below the watch's 48s watchdog so the
/// fallback poll covers the reconnect window.
const HEAD_STALE: Duration = Duration::from_secs(30);

/// The fallback head-poll cadence.
const HEAD_POLL_TICK: Duration = Duration::from_millis(200);

/// The feed-telemetry sample cadence (the per-driver `<=2s` scrape it
/// replaces).
const FEED_SAMPLE_TICK: Duration = Duration::from_secs(2);

/// The per-head trigger the source head edge fires. The composition root
/// wires `HeadReconciliation::reconcile_head` in here; the module never
/// names the reconciliation type.
pub type HeadTrigger = Arc<dyn Fn(u64) -> BoxFuture<'static, ()> + Send + Sync>;

/// The head clock a boot DECLARES. Pre-claim: it names the shape and the
/// node facts, never a publisher.
///
/// [`crate::bot_core::session_boot::EngineSession::boot`] claims the session
/// hub's one head source and converts a declaration into the post-claim
/// [`HostedClockBinding`] the hosted sources actually consume.
pub enum HostedHeadClock {
    /// Watch the chain node's `newHeads` subscription, falling back to the
    /// poller when the subscribe fails or the source goes stale.
    Watch {
        /// The chain node's subscription-scope endpoint.
        ws_url: String,
        /// The chain-node provider the fallback poller reads.
        provider: Arc<AlloyProvider>,
    },
    /// No WS feed: the poller is the primary clock.
    Poll {
        /// The chain-node provider the poller reads.
        provider: Arc<AlloyProvider>,
    },
    /// The engine's block pump owns the hub head source: it publishes every
    /// accepted header into the source the session boot claimed. This module's
    /// edge only fires the trigger per observed head, so the process keeps ONE
    /// `newHeads` subscription (the pump's) and no polled fallback — whatever
    /// strategies the boot admits.
    ///
    /// The ownership is declared, not inferred: a directly minted owner whose
    /// hub's head source is unclaimed is refused by name
    /// ([`HostedStartError::HeadSourceUnclaimed`]) instead of registering a
    /// source of its own.
    PumpOwned,
}

/// The post-claim clock binding the hosted sources consume.
///
/// The public [`HostedHeadClock`] declaration cannot be constructed with a
/// publisher, and this binding cannot be constructed without one where a
/// publisher is required: a hosted watch with no publisher and a pump-owned
/// clock carrying one are both unrepresentable.
pub enum HostedClockBinding {
    /// The engine's block pump owns the claimed source; the edge consumes it
    /// and publishes nothing of its own.
    PumpOwned,
    /// A hosted `newHeads` watch over the claimed source, with the poller's
    /// provider for the fallback read.
    Watch {
        /// The chain node's subscription-scope endpoint.
        ws_url: String,
        /// The chain-node provider the fallback poller reads.
        provider: Arc<AlloyProvider>,
        /// The claimed publisher the edge and the watch publish through.
        sender: HeadSender,
    },
    /// No WS feed: the poller is the primary clock, over the claimed source.
    Poll {
        /// The chain-node provider the poller reads.
        provider: Arc<AlloyProvider>,
        /// The claimed publisher the polled reads publish through.
        sender: HeadSender,
    },
}

/// The clock shape a head edge boots into — the mode decision, without the
/// node handles a binding carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClockShape {
    /// A hosted `newHeads` watch (connecting at boot).
    Watching,
    /// The poll clock is the primary clock.
    Polling,
    /// The engine pump owns the source; the edge only fires the trigger.
    PumpOwned,
}

impl HostedClockBinding {
    fn shape(&self) -> ClockShape {
        match self {
            Self::PumpOwned => ClockShape::PumpOwned,
            Self::Watch { .. } => ClockShape::Watching,
            Self::Poll { .. } => ClockShape::Polling,
        }
    }
}

/// What a spawned head edge observes heads through.
///
/// The pump-owned arm carries only a subscription (the engine driver owns the
/// publisher); the hosted arm carries the owner's publisher beside its
/// subscription, so a polled observation — which exists nowhere else — always
/// has somewhere to be published.
enum HeadEdgeSource {
    /// The pump owns the source: consume it, publish nothing.
    PumpOwned { subscription: HeadSubscription },
    /// This owner claimed the source: consume it and publish its own polled
    /// reads back into it.
    Hosted {
        sender: HeadSender,
        subscription: HeadSubscription,
    },
}

impl HeadEdgeSource {
    /// The publisher this edge may write through, when it owns one.
    fn publisher(&self) -> Option<HeadSender> {
        match self {
            Self::PumpOwned { .. } => None,
            Self::Hosted { sender, .. } => Some(sender.clone()),
        }
    }

    fn subscription_mut(&mut self) -> &mut HeadSubscription {
        match self {
            Self::PumpOwned { subscription } | Self::Hosted { subscription, .. } => subscription,
        }
    }
}

/// The clock, the per-head trigger, and the dedicated pubsub runtime a mint
/// provisioned but no start has consumed yet.
///
/// Owned by `HostedLifecycle::Provisioned` and moved out exactly once by the
/// `start_requested` transition, so nothing but a live start can dial.
struct Deferred {
    /// The post-claim head clock the start hosts. `None` hosts no head edge at
    /// all.
    clock: Option<HostedClockBinding>,
    /// The per-head trigger the start hands the head edge.
    trigger: Option<HeadTrigger>,
    /// The dedicated single-worker runtime a WS watch is hosted on (the
    /// [`OwnedPubsubRuntime`] discipline). `Some` exactly when `clock` is a
    /// [`HostedClockBinding::Watch`], and built EMPTY at mint: the start is the
    /// only thing that spawns onto it.
    pubsub_runtime: Option<OwnedPubsubRuntime>,
}

/// The resources a start launched that require ordered teardown.
///
/// Carried by `HostedLifecycle::Running` and `HostedLifecycle::HalfStarted` —
/// the two phases a launch can reach — so a partial start holds exactly what it
/// created instead of leaving teardown to resource-presence inference.
struct Live {
    /// The claimed head publisher this owner holds, when the boot's clock
    /// claimed the source (`Watch`/`Poll`). Dropping it at teardown is what
    /// closes the clock's subscriptions deterministically.
    head_publisher: Option<HeadSender>,
    /// The head-watch WS transport frontend, held so it is never moved away
    /// unaccounted. Teardown drops it WHILE the endpoint may still be alive, so
    /// no frontend clone keeps the pubsub request channel open.
    head_ws_provider: Option<Arc<dyn Provider<Ethereum>>>,
    /// The dedicated single-worker runtime the WS transport (and the alloy
    /// pubsub service it spawns) live on. Torn down LAST, which cancels the
    /// pubsub service and the head watch's driving task deterministically.
    pubsub_runtime: Option<OwnedPubsubRuntime>,
}

/// The owner's ONE authoritative lifecycle.
///
/// Provisioning, startup, partial failure, running, and teardown are
/// transitions of this value, and it is the only home of the resources each
/// phase owns. Private to this module: a reader gets the payload-free
/// [`HostedPhase`], so no caller can infer lifecycle truth from a resource's
/// presence — and there is no second latch to disagree with it.
enum HostedLifecycle {
    /// Minted: the ring pumps and driver streams exist, the deferred I/O has
    /// not launched, and nothing has dialed.
    Provisioned { deferred: Deferred },
    /// A start is in flight. The deferred resources are owned by that start,
    /// and nothing may observe this phase as running.
    Starting,
    /// The start launched every resource it provisions.
    Running { live: Live },
    /// Startup failed AFTER some resources launched. Terminal: never success,
    /// never projected as running, and recovered only by drop + recreate.
    HalfStarted { live: Live },
    /// The ordered teardown ran. Total, and the end of the line.
    TornDown,
}

/// The payload-free phase projection of the owner's lifecycle.
///
/// [`HostedSources::phase`] serves it; a caller reads the phase and never the
/// resources behind it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostedPhase {
    /// Provisioned and unstarted: nothing has dialed.
    Provisioned,
    /// A start is between its admission and its completion.
    Starting,
    /// Every deferred resource is live.
    Running,
    /// Startup failed after launching some resources: a terminal failure.
    HalfStarted,
    /// The ordered teardown ran.
    TornDown,
}

/// Why a [`HostedSources::start`] was refused.
///
/// Every refusal is a named lifecycle outcome, never a silent `Ok`: a partial
/// start cannot be mistaken for a running one, and no refusal hides a retry.
#[derive(Debug, thiserror::Error)]
pub enum HostedStartError {
    /// The boot names the engine's block pump as the hub head source's owner
    /// ([`HostedClockBinding::PumpOwned`]) and the hub's one head source is
    /// still unclaimed: no pump ever claimed it. A miswired boot fails closed
    /// here rather than claiming a source and double-reading the chain. The
    /// check runs AFTER the pumps launched and before the edge spawned, so the
    /// owner lands half-started with everything the partial launch created.
    #[error("the block pump owns no hub head source: this hub's head source is unclaimed")]
    HeadSourceUnclaimed,
    /// Another start is in flight: the phase is `Starting`, which is never
    /// reported as running.
    #[error("a hosted-sources start is already in flight")]
    StartInFlight,
    /// An earlier start already failed and left the owner half-started.
    /// Recovery is drop and recreate, not a retry.
    #[error("this hosted-sources owner's start already failed; drop and recreate it")]
    StartAfterFailure,
    /// The owner has been torn down.
    #[error("this hosted-sources owner is torn down")]
    TornDown,
}

/// Why a [`HostedSources::take_stream`] was refused: the `stream_taken`
/// transition is once-only, and the refusal is typed.
#[derive(Debug, thiserror::Error)]
pub enum StreamRefusal {
    /// No pump was minted for the kind (the arm was not active at boot).
    #[error("no pending-tx pump was minted for {0:?}")]
    NoPump(PendingTxSource),
    /// The kind's stream was already handed out.
    #[error("{0:?}'s driver stream was already handed out")]
    AlreadyTaken(PendingTxSource),
}

/// What a `start_requested` transition admitted.
enum StartAdmission {
    /// The deferred I/O this start must launch. The phase is now
    /// [`HostedPhase::Starting`]. Boxed so the admission stays small beside its
    /// payload-free arms (the deferred set is dominated by the runtime handle).
    Launch(Box<Deferred>),
    /// The owner is already [`HostedPhase::Running`]: an idempotent no-op.
    AlreadyRunning,
}

/// The one ordered teardown a `stop` transition yields, from whichever phase
/// owns the resources.
struct TeardownPlan {
    /// Dropped first: the claimed head publisher, so the clock's subscriptions
    /// close with the owner that published them.
    head_publisher: Option<HeadSender>,
    /// Dropped next: the WS transport frontend, while the endpoint may still be
    /// alive, so the pubsub service exits through its clean closed-channel path.
    head_ws_provider: Option<Arc<dyn Provider<Ethereum>>>,
    /// Torn down LAST: the dedicated runtime, which cancels the pubsub service
    /// and the head watch's driving task.
    pubsub_runtime: Option<OwnedPubsubRuntime>,
}

impl TeardownPlan {
    fn empty() -> Self {
        Self {
            head_publisher: None,
            head_ws_provider: None,
            pubsub_runtime: None,
        }
    }

    fn from_live(live: Live) -> Self {
        Self {
            head_publisher: live.head_publisher,
            head_ws_provider: live.head_ws_provider,
            pubsub_runtime: live.pubsub_runtime,
        }
    }
}

impl HostedLifecycle {
    fn phase(&self) -> HostedPhase {
        match self {
            Self::Provisioned { .. } => HostedPhase::Provisioned,
            Self::Starting => HostedPhase::Starting,
            Self::Running { .. } => HostedPhase::Running,
            Self::HalfStarted { .. } => HostedPhase::HalfStarted,
            Self::TornDown => HostedPhase::TornDown,
        }
    }

    /// The named transition `start_requested`.
    ///
    /// Legal from `Provisioned` (which moves to `Starting` and hands the start
    /// the deferred resources) and from `Running` (an idempotent no-op). Every
    /// other phase is a typed refusal: a concurrent start is `StartInFlight`, a
    /// start after a failed launch is `StartAfterFailure` (never `Ok`), and a
    /// torn-down owner is `TornDown`. Every outcome other than the admitted
    /// `Provisioned` arm restores the phase it found: a refusal (or the no-op)
    /// is never itself a transition.
    fn request_start(&mut self) -> Result<StartAdmission, HostedStartError> {
        match std::mem::replace(self, Self::Starting) {
            Self::Provisioned { deferred } => Ok(StartAdmission::Launch(Box::new(deferred))),
            Self::Running { live } => {
                *self = Self::Running { live };
                Ok(StartAdmission::AlreadyRunning)
            }
            Self::Starting => {
                *self = Self::Starting;
                Err(HostedStartError::StartInFlight)
            }
            Self::HalfStarted { live } => {
                *self = Self::HalfStarted { live };
                Err(HostedStartError::StartAfterFailure)
            }
            Self::TornDown => {
                *self = Self::TornDown;
                Err(HostedStartError::TornDown)
            }
        }
    }

    /// The named transition `resources_launched`: `Starting` -> `Running`,
    /// carrying the resources the launch created.
    fn resources_launched(&mut self, live: Live) {
        debug_assert!(
            matches!(self, Self::Starting),
            "only an in-flight start reports a launch"
        );
        *self = Self::Running { live };
    }

    /// The named transition `launch_failed`: `Starting` -> `HalfStarted`,
    /// carrying whatever the failed launch actually created (so its teardown is
    /// still ordered and total).
    fn launch_failed(&mut self, live: Live) {
        debug_assert!(
            matches!(self, Self::Starting),
            "only an in-flight start reports a failure"
        );
        *self = Self::HalfStarted { live };
    }

    /// The named transition `stop`: every phase yields the ordered teardown plan
    /// its resources need, and the lifecycle ends `TornDown`. Total — a
    /// torn-down owner yields an empty plan.
    ///
    /// A `Starting` phase holds no resources by construction: the in-flight
    /// start owns the deferred set until it reports, and a drop cannot run
    /// beside it (both need the owner). The only way to drop from `Starting` is
    /// a panic inside the launch, whose local `Deferred` tears its runtime down
    /// through [`OwnedPubsubRuntime`]'s own drop.
    fn teardown(&mut self) -> TeardownPlan {
        match std::mem::replace(self, Self::TornDown) {
            Self::Provisioned { deferred } => TeardownPlan {
                head_publisher: deferred.clock.as_ref().and_then(binding_publisher),
                head_ws_provider: None,
                pubsub_runtime: deferred.pubsub_runtime,
            },
            Self::Starting | Self::TornDown => TeardownPlan::empty(),
            Self::Running { live } | Self::HalfStarted { live } => TeardownPlan::from_live(live),
        }
    }
}

/// The head edge's mode: what it is observing heads through right now.
///
/// Pure data, owned by [`HeadEdge`] — the ONE value that advances it and
/// publishes it, so nothing infers a mode from which resource happens to exist.
/// The externally hosted modes are data too: [`PollCause::PrimaryPoll`] is a boot
/// that hosts no watch, and [`Self::PumpHosted`] is the pump-owned hub head
/// source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeadEdgeMode {
    /// No edge task has been spawned (a mint, or a boot hosting no clock).
    NotSpawned,
    /// The edge is connecting the hosted WS transport.
    WatchConnecting,
    /// The hosted `newHeads` watch is live. A STALE watch polls for the current
    /// iteration while its watchdog reconnects and STAYS `WatchLive`: the mode
    /// is the policy, and the poll is one iteration's decision.
    WatchLive,
    /// The poll clock is in force for the whole run, with the cause as data.
    PollFallback(PollCause),
    /// The pump owns the hub head source; the edge only fires the trigger per
    /// observed head.
    PumpHosted,
    /// The edge exited, after dropping its WS transport. Published once.
    Stopped,
}

/// Why the poll is the head clock — the fallback policy in force.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollCause {
    /// The boot hosts no watch, so the poll is the primary clock
    /// ([`HostedHeadClock::Poll`]).
    PrimaryPoll,
    /// The WS connect or the subscribe failed at this owner's start boundary.
    WatchConnectFailed,
}

/// What the hub head subscription did inside one iteration — the fact a fallback
/// poll is chosen from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceSignal {
    /// No head arrived inside [`HEAD_WAIT`]; `stale` records whether the source
    /// is past its staleness bound.
    Quiet { stale: bool },
    /// The subscription closed: no transport will deliver another head.
    Closed,
}

/// Why the edge read a head for itself, because the hub delivered none.
///
/// Distinct from [`PollCause`], which says why the poll IS the clock for the whole
/// run: a polled read also happens inside a watch edge, and then this reason
/// records the actual I/O — which is not what the published mode says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PollReason {
    /// The poll is this edge's clock, with the mode's own cause.
    Clock(PollCause),
    /// A live watch was silent past [`HEAD_STALE`]: one iteration's read while
    /// the watch's watchdog reconnects. The mode STAYS
    /// [`HeadEdgeMode::WatchLive`], so a reconnecting watch resumes watching.
    StaleWatch,
    /// The hub head subscription closed: the feed can deliver no more heads, so
    /// the edge reads one from its own provider instead.
    SourceClosed,
}

/// What one head-edge iteration observed, with the provenance of the read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeadObservation {
    /// A head arrived on the hub head subscription (the hosted watch, or the
    /// pump-owned source). Its owner published it, so the edge only fires the
    /// trigger.
    Watched(u64),
    /// A head this edge read from its own provider, which the edge must publish
    /// into the hub so every driver sees it.
    Polled { number: u64, reason: PollReason },
}

impl HeadObservation {
    /// The observed block number.
    fn number(self) -> u64 {
        match self {
            Self::Watched(number) | Self::Polled { number, .. } => number,
        }
    }
}

/// One head-edge iteration's decision, derived from the owned mode alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeadIteration {
    /// Await the hub head subscription (a live watch or an externally hosted
    /// source).
    AwaitHead,
    /// Sleep one poll tick, then read the poll provider's block number: the poll
    /// IS this edge's clock, with the cause as data.
    Poll(PollCause),
    /// Leave the edge loop.
    Exit,
}

/// A head-edge transition's outcome: the mode the edge is in afterwards, or the
/// refusal a stopped edge answers every later transition with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EdgeTransition {
    /// The edge reports the mode it is in after the transition.
    Applied(HeadEdgeMode),
    /// The edge had exited, so the transition was refused.
    RefusedStopped,
}

/// The head edge's ONE transition owner: the mode it observes heads through, the
/// once-only `Stopped` exit fact, and the publication of both.
///
/// One value, shared by the edge task that advances it and by readers
/// ([`HostedSources::head_edge`]). Every transition method moves the mode AND
/// publishes it under the same lock, so there is no task-local copy of the mode
/// and no second publication path to disagree with the projection. A clone of the
/// `Arc` lets a caller observe the clean shutdown across the owner's drop.
pub struct HeadEdge {
    /// Leaf-scoped: no lock on this value is ever held across an `.await`.
    owned: parking_lot::Mutex<EdgeState>,
}

/// The state the head edge's transitions move.
struct EdgeState {
    mode: HeadEdgeMode,
    stop_publications: u32,
}

impl HeadEdge {
    /// The mint's projection: no edge has been spawned and nothing has exited.
    fn minted() -> Self {
        Self {
            owned: parking_lot::Mutex::new(EdgeState {
                mode: HeadEdgeMode::NotSpawned,
                stop_publications: 0,
            }),
        }
    }

    /// The named transition `head_edge_spawned`: the edge boots into the mode its
    /// clock shape names, published BEFORE the connect so
    /// [`HeadEdgeMode::WatchConnecting`] is observable.
    fn boot(&self, shape: ClockShape) -> EdgeTransition {
        let boot_mode = match shape {
            ClockShape::Watching => HeadEdgeMode::WatchConnecting,
            ClockShape::Polling => HeadEdgeMode::PollFallback(PollCause::PrimaryPoll),
            ClockShape::PumpOwned => HeadEdgeMode::PumpHosted,
        };
        self.transition_from(HeadEdgeMode::NotSpawned, boot_mode)
    }

    /// The named transition `watch_connected`: the hosted watch went live.
    fn watch_connected(&self) -> EdgeTransition {
        self.transition_from(HeadEdgeMode::WatchConnecting, HeadEdgeMode::WatchLive)
    }

    /// The named transition `watch_connect_failed`: the poll becomes the clock for
    /// the whole run, with the cause recorded.
    fn watch_connect_failed(&self) -> EdgeTransition {
        self.transition_from(
            HeadEdgeMode::WatchConnecting,
            HeadEdgeMode::PollFallback(PollCause::WatchConnectFailed),
        )
    }

    /// The named transition `head_edge_exited`: publish `Stopped` exactly once. A
    /// second exit is refused, which is what makes the fact once-only.
    fn exited(&self) -> EdgeTransition {
        let mut owned = self.owned.lock();
        if owned.mode == HeadEdgeMode::Stopped {
            return EdgeTransition::RefusedStopped;
        }
        owned.mode = HeadEdgeMode::Stopped;
        owned.stop_publications += 1;
        EdgeTransition::Applied(HeadEdgeMode::Stopped)
    }

    /// Move AND publish the mode `from` -> `to`. A stopped edge refuses (its exit
    /// is final, so no late event revives the projection), and a mode other than
    /// `from` has no such successor, so the transition leaves it where it is.
    fn transition_from(&self, from: HeadEdgeMode, to: HeadEdgeMode) -> EdgeTransition {
        let mut owned = self.owned.lock();
        if owned.mode == HeadEdgeMode::Stopped {
            return EdgeTransition::RefusedStopped;
        }
        if owned.mode == from {
            owned.mode = to;
        }
        EdgeTransition::Applied(owned.mode)
    }

    /// This iteration's decision, from the ONE owned mode.
    #[must_use]
    fn iteration(&self) -> HeadIteration {
        match self.mode() {
            HeadEdgeMode::PollFallback(cause) => HeadIteration::Poll(cause),
            HeadEdgeMode::WatchLive | HeadEdgeMode::PumpHosted | HeadEdgeMode::WatchConnecting => {
                HeadIteration::AwaitHead
            }
            HeadEdgeMode::NotSpawned | HeadEdgeMode::Stopped => HeadIteration::Exit,
        }
    }

    /// The fallback poll this edge may run for a source that delivered no head, or
    /// `None` when it may not poll at all.
    ///
    /// This is where the mode bounds the actual I/O: the pump-hosted edge consumes
    /// the pump's own source and owns no provider, so it NEVER polls; a LIVE watch
    /// polls a quiet source only through its staleness policy; a connecting edge
    /// polls only for a source that actually closed; and either one polls a closed
    /// source with its own cause.
    #[must_use]
    fn fallback_poll(&self, signal: SourceSignal) -> Option<PollReason> {
        match self.mode() {
            // The pump-hosted edge has no provider of its own, a never-spawned or
            // stopped edge has no run left, and the poll clock reads the provider
            // directly instead of through a fallback.
            HeadEdgeMode::PumpHosted
            | HeadEdgeMode::NotSpawned
            | HeadEdgeMode::Stopped
            | HeadEdgeMode::PollFallback(_) => None,
            // A connecting edge has not gone live: it polls only for a source
            // that actually closed, never for mere quiet.
            HeadEdgeMode::WatchConnecting => match signal {
                SourceSignal::Closed => Some(PollReason::SourceClosed),
                SourceSignal::Quiet { .. } => None,
            },
            // A live watch polls only through its staleness policy: one
            // iteration's read while its watchdog reconnects, and the mode STAYS
            // `WatchLive`.
            HeadEdgeMode::WatchLive => match signal {
                SourceSignal::Closed => Some(PollReason::SourceClosed),
                SourceSignal::Quiet { stale: true } => Some(PollReason::StaleWatch),
                SourceSignal::Quiet { stale: false } => None,
            },
        }
    }

    /// The edge's current mode.
    #[must_use]
    pub fn mode(&self) -> HeadEdgeMode {
        self.owned.lock().mode
    }

    /// Whether the edge has exited — the positive half of the clean-shutdown
    /// guard.
    #[must_use]
    pub fn stopped(&self) -> bool {
        self.mode() == HeadEdgeMode::Stopped
    }

    /// How many times the exit published `Stopped`: exactly one after an edge
    /// exit, which is what makes the exit once-only.
    #[must_use]
    pub fn stop_publications(&self) -> u32 {
        self.owned.lock().stop_publications
    }
}

/// The engine-lifetime pending-tx pumps and head clock.
///
/// Constructed once at the boot ([`Self::mint`]), which PROVISIONS everything
/// (the hub rings, the driver streams, the dedicated head-watch runtime, the
/// clock and trigger) but starts nothing and dials nothing. [`Self::start`] —
/// the engine's start boundary — launches the deferred I/O: each arm's feed
/// pump, the feed-telemetry sampler, and the head-watch WS connect. Each arm's
/// driver stream is handed out once via [`Self::take_stream`]. The boot hands
/// the owner to the engine, which holds it for its whole life, so [`Drop`] runs
/// when the ENGINE drops and executes the one ordered teardown.
///
/// The owner is the SOLE writer of its lifecycle: callers request transitions
/// ([`Self::start`]) and read projections ([`Self::phase`],
/// [`Self::pump_status`], [`Self::head_edge`]).
pub struct HostedSources {
    hub: Arc<Hub>,
    pumps: Arc<parking_lot::Mutex<Vec<PendingTxPump>>>,
    /// The per-arm driver streams. Each slot is handed out once by
    /// [`Self::take_stream`]; a taken slot stays a slot, so the refusal is a
    /// typed transition outcome rather than a missing vector entry.
    streams: Vec<(PendingTxSource, StreamSlot)>,
    /// The shutdown signal fired by [`Drop`]: `true` stops the head edge and
    /// the feed sampler (the graceful stop for the plain loops on the shared
    /// runtime; the dedicated runtime teardown is the deterministic cancel).
    /// [`Self::start`] subscribes the receivers; a never-started owner has
    /// none, so its drop send is a plain no-op.
    shutdown: watch::Sender<bool>,
    /// The head edge's ONE transition owner. The edge task advances it and the
    /// transition itself publishes the mode; a caller reads the projection
    /// ([`Self::head_edge`]) and never drives it.
    head_edge: Arc<HeadEdge>,
    /// The ONE lifecycle. Leaf-scoped: no lock on this value is ever held
    /// across an `.await`, and the pumps lock is never held under it.
    lifecycle: parking_lot::Mutex<HostedLifecycle>,
}

/// One arm's driver-stream slot: the `stream_taken` transition is
/// `Available` -> `Taken`, once and never back.
enum StreamSlot {
    Available(PendingTxStream),
    Taken,
}

impl HostedSources {
    /// Mint the hosted sources: one pump per arm config (provisioned, not
    /// started), the dedicated head-watch runtime, and the deferred head clock.
    ///
    /// Each config NAMES its source kind, so a pump cannot be minted for one
    /// kind over another kind's feed. An arm the boot could not resolve gets no
    /// config and therefore no pump and no stream (its driver halts at its
    /// live-mode gate, the same refusal a boot with no resolvable feed
    /// produced).
    ///
    /// Nothing here dials: the feed pumps and the head-watch WS connect are
    /// launched by [`Self::start`] at the engine's start boundary, so an
    /// engine that is constructed and never started performs zero network
    /// I/O. The rings, the driver streams, the dedicated runtime, and the
    /// clock are all provisioned here, so `take_stream` and the live-gate
    /// semantics are unchanged.
    ///
    /// # Panics
    ///
    /// When a WS `Watch` clock is hosted and the dedicated single-worker
    /// runtime cannot be built (OS resource exhaustion): the boot fails closed
    /// rather than hosting a half-live head watch.
    ///
    /// # Errors
    ///
    /// [`HubError`] when a ring cannot be opened — a miswired boot (two arms
    /// naming one source kind over one hub) fails closed rather than
    /// half-hosting.
    pub fn mint(
        hub: Arc<Hub>,
        arms: impl IntoIterator<Item = PendingTxFeedConfig>,
        clock: Option<HostedClockBinding>,
        trigger: Option<HeadTrigger>,
    ) -> Result<Self, HubError> {
        let mut pumps = Vec::new();
        let mut streams = Vec::new();
        for cfg in arms {
            let source = cfg.source();
            let (pump, stream) = PendingTxPump::mint(&hub, cfg)?;
            pumps.push(pump);
            streams.push((source, StreamSlot::Available(stream)));
        }
        let pumps = Arc::new(parking_lot::Mutex::new(pumps));
        // The one shutdown channel the head edge and the feed sampler select
        // on; `Drop` fires it and `start` subscribes the receivers. No receiver
        // exists until `start`, so a never-started owner's drop send is a plain
        // no-op.
        let (shutdown, _shutdown_rx) = watch::channel(false);
        let head_edge = Arc::new(HeadEdge::minted());
        // A WS watch is hosted on a DEDICATED single-worker runtime, so the
        // alloy pubsub service it spawns (and the head watch's driving task) are
        // cancellable by tearing that runtime down — the deterministic teardown
        // `Drop` performs (the shared `OwnedPubsubRuntime` discipline). Built
        // here (empty); `start` is the only thing that spawns onto it.
        let pubsub_runtime = if matches!(&clock, Some(HostedClockBinding::Watch { .. })) {
            #[expect(
                clippy::expect_used,
                reason = "a fresh single-worker runtime build fails only on OS resource exhaustion; the boot fails closed rather than hosting a half-live head watch"
            )]
            let runtime = OwnedPubsubRuntime::build(OwnedPubsubRuntimeSpec {
                resource: "hosted_pubsub_workers",
                kind: "tokio single-worker runtime (hosted head-watch WS pubsub — deterministic teardown)",
                thread_name: "degenbot-hosted-pubsub",
                sizing: "exactly one per HostedSources owner (one hosted owner per engine); fixed single worker",
                binding: "pinned",
            })
            .expect("build the hosted head-watch pubsub runtime");
            Some(runtime)
        } else {
            None
        };
        Ok(Self {
            hub,
            pumps,
            streams,
            shutdown,
            head_edge,
            lifecycle: parking_lot::Mutex::new(HostedLifecycle::Provisioned {
                deferred: Deferred {
                    clock,
                    trigger,
                    pubsub_runtime,
                },
            }),
        })
    }

    /// Launch the deferred hosted I/O — the engine's start boundary.
    ///
    /// The transition is `start_requested`, one named edge of the owner's
    /// lifecycle: legal from [`HostedPhase::Provisioned`] (which it moves to
    /// `Starting`), an idempotent no-op from `Running`, and a typed refusal from
    /// every other phase. A start that runs launches every arm's feed pump (its
    /// first dial), spawns the feed-telemetry sampler, resolves the hub head
    /// source the boot's clock names (a hosted WS watch dials and connects here;
    /// a pump-owned clock resolves the source the pump registered), and spawns
    /// the head edge. [`Self::mint`] performed none of this, so an engine that
    /// is minted and never started dials nothing.
    ///
    /// No lock is held across an await: the admission takes the lifecycle lock
    /// for the transition only, the pumps lock for the launch call only, and the
    /// completion takes the lifecycle lock again to publish the phase.
    ///
    /// # Errors
    ///
    /// - [`HostedStartError::HeadSourceUnclaimed`] when the boot's
    ///   [`HostedClockBinding::PumpOwned`] names the pump as the owner but the
    ///   hub's one head source is unclaimed: a directly minted miswired owner
    ///   fails closed rather than claiming a source and double-reading the
    ///   chain. The check is post-pump-launch and pre-edge-spawn, so the owner
    ///   lands in [`HostedPhase::HalfStarted`] holding exactly what it launched
    ///   — a terminal failure, never success. The caller MUST drop the owner;
    ///   [`Drop`] stops and tears down everything the partial launch created.
    /// - [`HostedStartError::StartInFlight`] when another start is between its
    ///   admission and its completion. `Starting` is never reported as running.
    /// - [`HostedStartError::StartAfterFailure`] from `HalfStarted`: recovery is
    ///   drop and recreate, never a hidden retry.
    /// - [`HostedStartError::TornDown`] after a teardown.
    pub async fn start(&self) -> Result<(), HostedStartError> {
        let admission = self.lifecycle.lock().request_start();
        let deferred = match admission? {
            StartAdmission::AlreadyRunning => return Ok(()),
            StartAdmission::Launch(deferred) => *deferred,
        };
        let Deferred {
            clock,
            trigger,
            pubsub_runtime,
        } = deferred;
        // 1. Each arm's feed pump: the deferred dial.
        for pump in self.pumps.lock().iter() {
            pump.start();
        }
        // 2. The one feed-telemetry sampler.
        spawn_feed_sampler(Arc::clone(&self.pumps), self.shutdown.subscribe());
        // 3. The head edge: resolve the hub head source the clock names (a
        //    hosted watch dials and connects on the owned runtime; a pump-owned
        //    clock resolves the pump's own source) and spawn the edge loop. The
        //    deferred set is owned by this call, never by the lifecycle lock, so
        //    nothing holds a lock across the await.
        let launch = match clock {
            Some(clock) => {
                spawn_head_task(
                    Arc::clone(&self.hub),
                    clock,
                    trigger,
                    self.shutdown.subscribe(),
                    Arc::clone(&self.head_edge),
                    pubsub_runtime.as_ref(),
                )
                .await
            }
            None => HeadEdgeLaunch::nothing_launched(),
        };
        let live = Live {
            head_publisher: launch.publisher,
            head_ws_provider: launch.transport,
            pubsub_runtime,
        };
        let mut lifecycle = self.lifecycle.lock();
        match launch.failure {
            None => {
                lifecycle.resources_launched(live);
                Ok(())
            }
            Some(error) => {
                lifecycle.launch_failed(live);
                Err(error)
            }
        }
    }

    /// The owner's lifecycle phase — a payload-free projection of the one
    /// authoritative lifecycle.
    #[must_use]
    pub fn phase(&self) -> HostedPhase {
        self.lifecycle.lock().phase()
    }

    /// The source kinds this owner minted a pump for, in mint order.
    ///
    /// A boot probe reads the roster to prove one pump per hosted arm; the
    /// pumps themselves stay the owner's so telemetry and teardown cannot be
    /// duplicated by a reader.
    #[must_use]
    pub fn pump_sources(&self) -> Vec<PendingTxSource> {
        self.pumps
            .lock()
            .iter()
            .map(PendingTxPump::source)
            .collect()
    }

    /// One pump's feed status by source kind, or `None` when no pump was minted
    /// for that kind. A never-started owner reports every counter at zero —
    /// which is what proves the mint dialed nothing.
    #[must_use]
    pub fn pump_status(&self, source: PendingTxSource) -> Option<BackrunFeedStatus> {
        self.pumps
            .lock()
            .iter()
            .find(|pump| pump.source() == source)
            .map(PendingTxPump::status)
    }

    /// Hand out one arm's typed stream through the named `stream_taken`
    /// transition: once per arm, and a typed refusal afterwards.
    ///
    /// # Errors
    ///
    /// [`StreamRefusal::NoPump`] when no pump was minted for the kind (the arm
    /// was not active at boot), and [`StreamRefusal::AlreadyTaken`] when the
    /// kind's stream was already handed out.
    pub fn take_stream(
        &mut self,
        source: PendingTxSource,
    ) -> Result<PendingTxStream, StreamRefusal> {
        let Some(index) = self.streams.iter().position(|(kind, _)| *kind == source) else {
            return Err(StreamRefusal::NoPump(source));
        };
        let slot = &mut self.streams[index].1;
        let StreamSlot::Available(stream) = std::mem::replace(slot, StreamSlot::Taken) else {
            return Err(StreamRefusal::AlreadyTaken(source));
        };
        Ok(stream)
    }

    /// Subscribe a reader of the hub's latest head. Multi-subscriber and
    /// infallible: every driver reads the same clock the session claimed, at
    /// the clock's current value.
    #[must_use]
    pub fn subscribe_head(&self) -> HeadSubscription {
        self.hub.subscribe_head()
    }

    /// The head edge's transition owner — the projection a caller reads, never
    /// an independent lifecycle fact. A clone lets a caller observe the edge's
    /// clean shutdown across the owner's drop (the positive half of the teardown
    /// guard); the owner itself never drives it.
    #[must_use]
    pub fn head_edge(&self) -> Arc<HeadEdge> {
        Arc::clone(&self.head_edge)
    }
}

impl HostedSources {
    /// The named transition `stop`: the ONE ordered teardown, total from every
    /// phase.
    ///
    /// Order is load-bearing (the `AnvilFork` discipline):
    ///
    /// 1. stop every pump's transport,
    /// 2. fire the graceful shutdown the head edge and the feed sampler select
    ///    on,
    /// 3. drop the claimed head publisher, so the clock's subscriptions close
    ///    with the owner that published them,
    /// 4. drop the WS transport frontend while the endpoint may still be alive,
    /// 5. tear the dedicated runtime down — the deterministic cancellation of
    ///    the pubsub service and the head watch's driving task.
    ///
    /// The resources come from ONE plan the lifecycle yields, so a
    /// never-started, half-started, running, and already torn-down owner all
    /// tear down through the same path.
    fn teardown(&mut self) {
        for pump in self.pumps.lock().iter() {
            pump.stop();
        }
        // The receiver side may already be gone (a launch that failed after the
        // sampler spawned, or a task that aborted): a send error is expected and
        // harmless — a dropped sender ends both actors too.
        let _ = self.shutdown.send(true);
        let plan = self.lifecycle.lock().teardown();
        drop(plan.head_publisher);
        drop(plan.head_ws_provider);
        drop(plan.pubsub_runtime);
    }
}

impl Drop for HostedSources {
    fn drop(&mut self) {
        self.teardown();
    }
}

/// Connect the head-watch WS transport on the owner's dedicated runtime and
/// subscribe, returning the transport Arc when the watch went live (`None` when
/// the connect or subscribe failed, so the clock falls back to the poller).
///
/// Both the connect and the subscribe run on `runtime`, so the alloy pubsub
/// service and the head watch's driving task land there and are cancelled with
/// it. The initial work runs in a task on that runtime (a nested `block_on`
/// would panic off the async caller) and reports back over a oneshot.
async fn connect_head_watch(
    sender: HeadSender,
    ws_url: &str,
    runtime: &OwnedPubsubRuntime,
) -> Option<Arc<dyn Provider<Ethereum>>> {
    let handle = runtime.handle();
    let ws_url = ws_url.to_string();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    handle.spawn(async move {
        let transport = match AlloyProvider::new(&ws_url, DEFAULT_MAX_RETRIES).await {
            Ok(ws_provider) => {
                let transport = ws_provider.provider_arc();
                match HeadWatch::subscribe(
                    sender,
                    Arc::clone(&transport),
                    HeadWatchConfig::default(),
                    tokio::runtime::Handle::current(),
                )
                .await
                {
                    Ok(_) => Some(transport),
                    Err(error) => {
                        tracing::error!(
                            %error,
                            "head watch subscribe failed - falling back to 200ms head poll"
                        );
                        None
                    }
                }
            }
            Err(error) => {
                tracing::error!(
                    %error,
                    "head watch WS connect failed - falling back to 200ms head poll"
                );
                None
            }
        };
        let _ = result_tx.send(transport);
    });
    result_rx.await.unwrap_or_else(|_| {
        tracing::error!("head watch connect task aborted - falling back to 200ms head poll");
        None
    })
}

/// The publisher a clock binding carries, when it carries one.
fn binding_publisher(binding: &HostedClockBinding) -> Option<HeadSender> {
    match binding {
        HostedClockBinding::PumpOwned => None,
        HostedClockBinding::Watch { sender, .. } | HostedClockBinding::Poll { sender, .. } => {
            Some(sender.clone())
        }
    }
}

/// The head edge's launch product.
///
/// `publisher` is the claimed head publisher the OWNER keeps (the edge keeps a
/// clone and drops it on exit); `transport` is the WS transport Arc the owner
/// must hold. `failure` is set when the head-source step failed: the resources
/// the partial launch created are still carried back, so a half-started
/// owner's teardown drops them in order instead of leaving them to a local
/// drop.
struct HeadEdgeLaunch {
    publisher: Option<HeadSender>,
    transport: Option<Arc<dyn Provider<Ethereum>>>,
    failure: Option<HostedStartError>,
}

impl HeadEdgeLaunch {
    /// A clockless owner launches no edge at all.
    fn nothing_launched() -> Self {
        Self {
            publisher: None,
            transport: None,
            failure: None,
        }
    }
}

/// Subscribe the transport and spawn the head edge task.
///
/// The task owns the head edge while its [`HostedSources`] owner lives: it
/// fires the trigger once per observed head (from the watch, or from the
/// fallback poller, which also publishes polled heads into the hub so every
/// driver sees them), and on a stale or closed watch it polls for that
/// iteration. It exits when the owner's shutdown signal fires (or the owner's
/// sender drops), dropping the WS transport it holds WHILE the endpoint may
/// still be alive — the `AnvilFork` teardown discipline, so the alloy pubsub
/// service exits through the clean "request channel closed" path rather than a
/// reconnect loop against a dead endpoint.
///
/// The WS connect and the head watch's driving task run on `pubsub_runtime`
/// (the owner's dedicated runtime), so the owner's teardown cancels them
/// deterministically by tearing that runtime down. The launch reports both the
/// WS transport Arc the owner must hold and, when the head-source step failed,
/// the error alongside the resources the failed launch created — a half-started
/// owner's teardown then drops them in order instead of leaving them to a local
/// drop.
///
/// The edge's ONE transition owner advances here: `boot` publishes
/// `WatchConnecting`/`PollFallback(PrimaryPoll)`/`PumpHosted`, the connect
/// outcome publishes `WatchLive` or `PollFallback(WatchConnectFailed)`, and the
/// exit publishes `Stopped` exactly once — by this launch on a pre-spawn failure
/// (no task exists to publish it), and by the edge task on exit.
///
async fn spawn_head_task(
    hub: Arc<Hub>,
    clock: HostedClockBinding,
    trigger: Option<HeadTrigger>,
    mut shutdown: watch::Receiver<bool>,
    head_edge: Arc<HeadEdge>,
    pubsub_runtime: Option<&OwnedPubsubRuntime>,
) -> HeadEdgeLaunch {
    // `head_edge_spawned`: the mode the edge boots into, published before the
    // connect so `WatchConnecting` is observable.
    head_edge.boot(clock.shape());
    // The WS transport the edge owns for its lifetime; the owner keeps its own
    // clone and this one is dropped on exit (see this function's doc). `None`
    // when the boot hosts no WS watch (a poll or pump-owned clock, or a WS connect
    // that failed at boot).
    let mut head_transport: Option<Arc<dyn Provider<Ethereum>>> = None;
    if let HostedClockBinding::Watch { ws_url, sender, .. } = &clock {
        // The WS connect runs on the DEDICATED runtime, so the alloy pubsub
        // service it spawns lives there too — and is cancelled when teardown
        // drops that runtime; the head watch's driving task spawns on the same
        // runtime for the same reason. The runtime is `Some` here by
        // construction: `mint` builds it exactly when the clock is a WS watch.
        let connected = match pubsub_runtime {
            Some(runtime) => connect_head_watch(sender.clone(), ws_url, runtime).await,
            None => None,
        };
        if let Some(transport) = connected {
            head_transport = Some(transport);
            head_edge.watch_connected();
        } else {
            head_edge.watch_connect_failed();
        }
    }
    // The claimed publisher this owner keeps, when the binding carries one.
    let publisher = binding_publisher(&clock);
    // The pump-owned arm consumes the source the pump claimed; a hub whose
    // source is still unclaimed means no pump ever claimed it, which is a
    // PRE-SPAWN failure: the edge task will never exist, so this launch
    // publishes the once-only terminal `Stopped` fact itself — without it the
    // projection would stay live past a half-started owner's teardown. The
    // resources still ride the failure back into `HalfStarted`, so the ordered
    // teardown drops them in order.
    let mut source = match &clock {
        HostedClockBinding::PumpOwned => {
            if !hub.head_source_is_claimed() {
                head_edge.exited();
                return HeadEdgeLaunch {
                    publisher,
                    transport: head_transport,
                    failure: Some(HostedStartError::HeadSourceUnclaimed),
                };
            }
            HeadEdgeSource::PumpOwned {
                subscription: hub.subscribe_head(),
            }
        }
        HostedClockBinding::Watch { sender, .. } | HostedClockBinding::Poll { sender, .. } => {
            HeadEdgeSource::Hosted {
                sender: sender.clone(),
                subscription: hub.subscribe_head(),
            }
        }
    };
    let poll_provider: Option<Arc<AlloyProvider>> = match &clock {
        HostedClockBinding::Watch { provider, .. } | HostedClockBinding::Poll { provider, .. } => {
            Some(Arc::clone(provider))
        }
        HostedClockBinding::PumpOwned => None,
    };
    let mut last_published = source
        .subscription_mut()
        .head()
        .map_or(0, |head| head.number);
    // The owner keeps its own publisher and transport clones; the edge keeps
    // these and drops them on exit.
    let owned_publisher = publisher.clone();
    let owned_transport = head_transport.clone();
    degenbot_core::runtime::get_runtime().spawn(async move {
        loop {
            if head_edge.iteration() == HeadIteration::Exit {
                break;
            }
            // Select the stop signal against ONE head iteration so the edge
            // leaves promptly on drop instead of waiting out a poll/watch
            // bound — and drops its WS transport right after.
            let observed = tokio::select! {
                biased;
                _ = shutdown.changed() => break,
                observed = head_iteration(
                    &mut source,
                    &head_edge,
                    poll_provider.as_ref(),
                    last_published,
                ) => observed,
            };
            let Some(observation) = observed else {
                continue;
            };
            last_published =
                publish_observed(source.publisher().as_ref(), observation, last_published);
            if let Some(trigger) = trigger.as_ref() {
                trigger(observation.number()).await;
            }
        }
        // Teardown discipline: drop the WS transport WHILE the endpoint may
        // still be alive, so the pubsub service observes the closed request
        // channel and exits cleanly (never the reconnect loop). Then publish
        // the once-only `Stopped` fact the teardown guard asserts.
        drop(head_transport);
        head_edge.exited();
        degenbot_core::op_info!(
            domain = pump,
            "hosted head edge stopped; WS transport dropped"
        );
    });
    HeadEdgeLaunch {
        publisher: owned_publisher,
        transport: owned_transport,
        failure: None,
    }
}

/// One head-edge iteration: resolve the next observed head, or `None` when this
/// iteration observed none. Split out so the spawned loop can `select!` the stop
/// signal against it.
///
/// Every observation carries its provenance ([`HeadObservation`]): a head the hub
/// delivered was published by its owner, while a head the edge polled for — the
/// poll clock, a stale watch, or a closed subscription — is the edge's own read
/// and carries the reason it read.
async fn head_iteration(
    source: &mut HeadEdgeSource,
    edge: &HeadEdge,
    poll_provider: Option<&Arc<AlloyProvider>>,
    last_published: u64,
) -> Option<HeadObservation> {
    match edge.iteration() {
        // The poll is this edge's clock: one polled read per tick, and only a NEW
        // head is an observation — the tick would otherwise re-fire the trigger
        // for a head it already reported.
        HeadIteration::Poll(cause) => {
            let observed = poll_head(poll_provider, PollReason::Clock(cause)).await?;
            (observed.number() > last_published).then_some(observed)
        }
        // Watch (or external-hub) mode: the head edge resolves on a header's
        // arrival; the 2s bound keeps the edge live while the head is quiet.
        HeadIteration::AwaitHead => {
            match tokio::time::timeout(HEAD_WAIT, source.subscription_mut().changed()).await {
                Ok(Ok(())) => source
                    .subscription_mut()
                    .borrow_and_update()
                    .map(|head| HeadObservation::Watched(head.number)),
                Ok(Err(_)) => {
                    // The feed can deliver no more heads, so an edge with a provider
                    // of its own reads one instead.
                    if let Some(reason) = edge.fallback_poll(SourceSignal::Closed) {
                        tracing::warn!("head source closed - polling");
                        poll_head(poll_provider, reason).await
                    } else {
                        // No provider of its own: nothing can be read, but the
                        // iteration still paces so a closed feed cannot spin the loop.
                        tracing::warn!("head source closed - no poll fallback for this edge");
                        tokio::time::sleep(HEAD_POLL_TICK).await;
                        None
                    }
                }
                Err(_) => {
                    let stale = source.subscription_mut().stale(HEAD_STALE);
                    match edge.fallback_poll(SourceSignal::Quiet { stale }) {
                        // Only a live watch falls back to a polled read, and only
                        // through its staleness policy: the mode STAYS
                        // `WatchLive`, so a reconnecting watch resumes watching
                        // instead of being reported as a permanently polled edge.
                        Some(reason) => {
                            tracing::warn!("head source stale - polling");
                            poll_head(poll_provider, reason).await
                        }
                        None => None,
                    }
                }
            }
        }
        HeadIteration::Exit => None,
    }
}

/// One polled read of this edge's own provider, `reason` recording why the edge
/// read a head instead of being told about one.
async fn poll_head(
    provider: Option<&Arc<AlloyProvider>>,
    reason: PollReason,
) -> Option<HeadObservation> {
    tokio::time::sleep(HEAD_POLL_TICK).await;
    poll(provider)
        .await
        .map(|number| HeadObservation::Polled { number, reason })
}

async fn poll(provider: Option<&Arc<AlloyProvider>>) -> Option<u64> {
    let provider = provider?;
    provider.get_block_number().await.ok()
}

/// Publish a polled head into the hub head clock. A polled read carries the
/// block number only, so the metadata is absent and a consumer that needs the
/// header's fee falls back to its own read.
fn publish_polled(sender: &HeadSender, number: u64) {
    sender.publish(Head {
        number,
        metadata: None,
    });
}

/// Publish an observation into the hub head clock when it is the edge's OWN
/// read, returning the new high-water mark.
///
/// A watched head was published by the clock's owner, so re-publishing it would
/// be a second write of the same fact; a polled head exists nowhere else, so the
/// edge writes it for every driver to read. `sender` is absent only for the
/// pump-owned edge, which never polls.
fn publish_observed(
    sender: Option<&HeadSender>,
    observation: HeadObservation,
    last_published: u64,
) -> u64 {
    match observation {
        HeadObservation::Watched(_) => last_published,
        HeadObservation::Polled { number, .. } => {
            if number > last_published {
                if let Some(sender) = sender {
                    publish_polled(sender, number);
                }
                number
            } else {
                last_published
            }
        }
    }
}

/// Spawn the one feed-telemetry sampler: scrape every hosted pump's status
/// into the engine instruments on a `<=2s` tick, pushing counters as deltas.
fn spawn_feed_sampler(
    pumps: Arc<parking_lot::Mutex<Vec<PendingTxPump>>>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut last: Vec<BackrunFeedStatus> = pumps.lock().iter().map(PendingTxPump::status).collect();
    degenbot_core::runtime::get_runtime().spawn(async move {
        loop {
            // A stop signal ends the sampler promptly; otherwise it ticks.
            tokio::select! {
                biased;
                _ = shutdown.changed() => break,
                () = tokio::time::sleep(FEED_SAMPLE_TICK) => {}
            }
            let Some(pipeline) = degenbot_substrate::telemetry_port::pipeline() else {
                continue;
            };
            let guard = pumps.lock();
            if guard.is_empty() {
                continue;
            }
            let current: Vec<BackrunFeedStatus> = guard.iter().map(PendingTxPump::status).collect();
            drop(guard);
            let pairs: Vec<(BackrunFeedStatus, BackrunFeedStatus)> = current
                .iter()
                .zip(
                    last.iter()
                        .chain(std::iter::repeat(&BackrunFeedStatus {
                            connected: false,
                            accepted: 0,
                            dropped_ring: 0,
                            rejected_chain_id: 0,
                            rejected_parse: 0,
                            reconnects: 0,
                            last_event_unix_ms: 0,
                        }))
                        .take(current.len()),
                )
                .map(|(cur, prev)| (*cur, *prev))
                .collect();
            if let Some((
                connected,
                seconds_since_event,
                frames,
                dropped,
                parse,
                chain_id,
                reconnects,
            )) = aggregate_feed_sample(&pairs)
            {
                pipeline.record_backrun_feed(
                    connected,
                    seconds_since_event,
                    frames,
                    dropped,
                    parse,
                    chain_id,
                    reconnects,
                );
            }
            last = current;
        }
    });
}

/// One sampler record: `(connected, seconds_since_event, frames, dropped,
/// parse, chain-id rejects, reconnects)` — the positional shape the engine
/// instruments' `record_backrun_feed` consumes.
type FeedSample = (bool, Option<f64>, u64, u64, u64, u64, u64);

/// One sampler record from every pump's (current, previous) status pair:
/// `connected` is ANY arm live, `seconds_since_event` the most recent event
/// across arms, and the counters the summed per-arm deltas. A single arm
/// records exactly what the per-driver scrape it replaced recorded.
fn aggregate_feed_sample(samples: &[(BackrunFeedStatus, BackrunFeedStatus)]) -> Option<FeedSample> {
    if samples.is_empty() {
        return None;
    }
    let now_ms = now_unix_ms();
    let mut connected = false;
    let mut seconds_since_event: Option<f64> = None;
    let mut frames = 0;
    let mut dropped = 0;
    let mut parse = 0;
    let mut chain_id = 0;
    let mut reconnects = 0;
    #[expect(clippy::cast_precision_loss)]
    for (current, previous) in samples {
        connected |= current.connected;
        if current.last_event_unix_ms != 0 {
            let secs = (now_ms.saturating_sub(current.last_event_unix_ms)) as f64 / 1_000.0;
            seconds_since_event = Some(match seconds_since_event {
                Some(shortest) if shortest < secs => shortest,
                _ => secs,
            });
        }
        frames += current.accepted.saturating_sub(previous.accepted);
        dropped += current.dropped_ring.saturating_sub(previous.dropped_ring);
        parse += current
            .rejected_parse
            .saturating_sub(previous.rejected_parse);
        chain_id += current
            .rejected_chain_id
            .saturating_sub(previous.rejected_chain_id);
        reconnects += current.reconnects.saturating_sub(previous.reconnects);
    }
    Some((
        connected,
        seconds_since_event,
        frames,
        dropped,
        parse,
        chain_id,
        reconnects,
    ))
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "hosted-source tests assert mint and head-edge outcomes"
)]
mod tests {
    use super::*;
    use degenbot_rpc::backrun_feed::BackrunFeedConfig;
    use degenbot_rpc::txpool_feed::TxpoolFeedConfig;
    use futures_util::SinkExt as _;
    use futures_util::StreamExt as _;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    /// Both arms' kinds mint on ONE hub, and each arm's stream is handed out
    /// exactly its own feed's frames: no second-arm tombstone, per-kind
    /// corpus split.
    #[tokio::test]
    async fn both_arms_mint_and_drain_their_own_corpus() {
        let hub = Arc::new(Hub::new());
        let mut hosted = HostedSources::mint(
            Arc::clone(&hub),
            [
                PendingTxFeedConfig::Mevblocker(BackrunFeedConfig::for_mainnet()),
                PendingTxFeedConfig::Txpool(TxpoolFeedConfig::defaults()),
            ],
            None,
            None,
        )
        .expect("both kinds mint without a tombstone");

        let mev = hosted
            .take_stream(PendingTxSource::Mevblocker)
            .expect("the mevblocker arm's stream");
        let tx = hosted
            .take_stream(PendingTxSource::Txpool)
            .expect("the txpool arm's stream");
        assert_eq!(mev.source(), PendingTxSource::Mevblocker);
        assert_eq!(tx.source(), PendingTxSource::Txpool);
        assert!(
            matches!(
                hosted.take_stream(PendingTxSource::Mevblocker),
                Err(StreamRefusal::AlreadyTaken(PendingTxSource::Mevblocker))
            ),
            "a stream is handed out exactly once, then refused with a typed refusal"
        );

        // Push a frame per kind (test support) and drain each stream: each
        // arm's corpus is exactly its own feed's.
        {
            let pumps = hosted.pumps.lock();
            for pump in pumps.iter() {
                pump.push(frame(pump.source(), 7));
            }
        }
        let mev_frames = mev.drain();
        let tx_frames = tx.drain();
        assert_eq!(
            mev_frames.len(),
            1,
            "the mevblocker stream is its feed's corpus"
        );
        assert_eq!(
            mev_frames[0].hash,
            mev_hash(),
            "the mevblocker frame is the mevblocker feed's"
        );
        assert_eq!(tx_frames.len(), 1, "the txpool stream is its feed's corpus");
        assert_eq!(
            tx_frames[0].hash,
            txpool_hash(),
            "the txpool frame is the txpool feed's"
        );
    }

    fn mev_hash() -> alloy::primitives::B256 {
        alloy::primitives::B256::repeat_byte(0x11)
    }

    fn txpool_hash() -> alloy::primitives::B256 {
        alloy::primitives::B256::repeat_byte(0x22)
    }

    fn frame(source: PendingTxSource, nonce: u64) -> degenbot_eventhub::PendingTx {
        degenbot_eventhub::PendingTx {
            raw_signed_tx: None,
            chain_id: 1,
            from: alloy::primitives::Address::ZERO,
            to: None,
            value: alloy::primitives::U256::ZERO,
            data: alloy::primitives::Bytes::new(),
            gas: 0,
            max_fee_per_gas: 0,
            max_priority_fee_per_gas: 0,
            nonce,
            hash: match source {
                PendingTxSource::Mevblocker => mev_hash(),
                PendingTxSource::Txpool => txpool_hash(),
            },
            access_list: serde_json::Value::Null,
            tx_type: 2,
            received_unix_ms: 0,
        }
    }

    /// The head edge fires the trigger once per observed head — the source
    /// edge, not a driver loop, owns the reconciliation trigger.
    #[tokio::test]
    async fn the_head_edge_fires_the_trigger_per_observed_head() {
        let (hub, sender) = claimed_hub();
        let (fire_tx, mut fire_rx) = tokio::sync::mpsc::unbounded_channel::<u64>();
        let trigger: HeadTrigger = Arc::new(move |head| {
            let fire_tx = fire_tx.clone();
            Box::pin(async move {
                fire_tx.send(head).expect("the recorder lives");
            })
        });
        let hosted = HostedSources::mint(
            Arc::clone(&hub),
            [],
            Some(HostedClockBinding::PumpOwned),
            Some(trigger),
        )
        .expect("the pump-owned clock mints");
        // The head edge is deferred: it must be started before it observes a
        // head (mint alone spawns nothing).
        hosted.start().await.expect("the pump-owned clock starts");

        sender.publish(Head {
            number: 42,
            metadata: None,
        });
        let fired = fire_rx.recv().await.expect("the edge fired");
        assert_eq!(fired, 42, "the trigger carries the observed head");
    }

    /// One head source per process: the PUMP owns it, so every hosted owner
    /// over that hub consumes the same source rather than registering a second
    /// one — the pump publishes a head and both edges fire on it.
    #[tokio::test]
    async fn a_second_mint_consumes_the_pumps_single_head_source() {
        let (hub, pump) = claimed_hub();
        let recorder = |tx: tokio::sync::mpsc::UnboundedSender<u64>| -> HeadTrigger {
            Arc::new(move |head| {
                let tx = tx.clone();
                Box::pin(async move {
                    let _ = tx.send(head);
                })
            })
        };
        let (first_tx, mut first_rx) = tokio::sync::mpsc::unbounded_channel::<u64>();
        let (second_tx, mut second_rx) = tokio::sync::mpsc::unbounded_channel::<u64>();
        let first = HostedSources::mint(
            Arc::clone(&hub),
            [],
            Some(HostedClockBinding::PumpOwned),
            Some(recorder(first_tx)),
        )
        .expect("first mint");
        first.start().await.expect("first start");
        let second = HostedSources::mint(
            Arc::clone(&hub),
            [],
            Some(HostedClockBinding::PumpOwned),
            Some(recorder(second_tx)),
        )
        .expect("second mint consumes the same head source");
        second
            .start()
            .await
            .expect("second start consumes the same head source");

        pump.publish(Head {
            number: 7,
            metadata: None,
        });
        assert_eq!(first_rx.recv().await, Some(7), "the first edge fires");
        assert_eq!(
            second_rx.recv().await,
            Some(7),
            "the second edge reads the same pump-owned source"
        );
        assert!(
            hub.head_source_is_claimed(),
            "the claim is permanent: both owners consume one claimed source"
        );
    }

    /// Ownership is DECLARED, not inferred: a directly minted pump-owned clock
    /// whose hub's head source is unclaimed refuses the start by name instead of
    /// claiming a source of its own.
    #[tokio::test]
    async fn an_unclaimed_pump_clock_is_refused_by_name() {
        let hub = unclaimed_head_hub();
        let hosted = HostedSources::mint(
            Arc::clone(&hub),
            [],
            Some(HostedClockBinding::PumpOwned),
            None,
        )
        .expect("the pump-owned clock mints");
        let refused = hosted.start().await;
        assert!(
            matches!(refused, Err(HostedStartError::HeadSourceUnclaimed)),
            "a pump-owned clock over an unclaimed source fails closed: {refused:?}"
        );
        assert_eq!(
            hosted.phase(),
            HostedPhase::HalfStarted,
            "the refusal is a terminal launch failure, never running"
        );
        assert!(
            !hub.head_source_is_claimed(),
            "the edge claimed no source of its own"
        );
    }

    /// The sampler record: one arm aggregates to exactly what the per-driver
    /// scrape recorded; two arms sum their deltas and keep the freshest
    /// event age.
    #[test]
    fn the_sampler_aggregates_arms() {
        let status = |connected, accepted, last_ms| BackrunFeedStatus {
            connected,
            accepted,
            dropped_ring: 0,
            rejected_chain_id: 0,
            rejected_parse: 0,
            reconnects: 0,
            last_event_unix_ms: last_ms,
        };
        let prev = status(false, 10, 0);
        let one = status(true, 15, 1_000);
        let (connected, secs, frames, dropped, parse, chain, reconnects) =
            aggregate_feed_sample(&[(one, prev)]).expect("one arm");
        assert!(connected);
        assert_eq!((frames, dropped, parse, chain, reconnects), (5, 0, 0, 0, 0));
        assert!(secs.is_some(), "the event age is carried");

        let two_prev = status(false, 100, 0);
        let two = status(false, 103, 2_000);
        let (connected, secs, frames, ..) =
            aggregate_feed_sample(&[(one, prev), (two, two_prev)]).expect("two arms");
        assert!(connected, "any arm live reads connected");
        assert_eq!(frames, 8, "the counters sum across arms");
        assert!(
            secs.is_some_and(|s| s
                <= aggregate_feed_sample(&[(status(true, 15, 1_000), status(false, 10, 0))])
                    .expect("one arm")
                    .1
                    .expect("age")),
            "the freshest event age wins"
        );
    }

    // ---- Engine-lifetime ownership + clean head-edge shutdown --------------

    /// A bounded poll over the head edge's published mode: the edge publishes
    /// `Stopped` on a detached task on the shared runtime, so the test waits a
    /// window instead of assuming one scheduler turn.
    async fn await_head_edge_stopped(edge: &HeadEdge) -> bool {
        for _ in 0..1_000 {
            if edge.stopped() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        edge.stopped()
    }

    /// A bounded poll for a published head-edge mode (the connect outcome is
    /// published from the shared runtime, not synchronously with `start`).
    async fn await_head_edge_mode(edge: &HeadEdge, want: HeadEdgeMode) -> HeadEdgeMode {
        for _ in 0..1_000 {
            if edge.mode() == want {
                return want;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        edge.mode()
    }

    /// The WS-transport slot's type, as a lazy HTTP provider handle (built
    /// without connecting): the teardown tests only need an `Arc` to release.
    async fn detached_transport() -> Arc<dyn Provider<Ethereum>> {
        AlloyProvider::new("http://127.0.0.1:9", DEFAULT_MAX_RETRIES)
            .await
            .expect("a lazy HTTP provider builds without connecting")
            .provider_arc()
    }

    /// A test hub whose one head source is claimed, plus the claim: the
    /// publisher every hosted clock binding must carry.
    fn claimed_hub() -> (Arc<Hub>, HeadSender) {
        let hub = Arc::new(Hub::new());
        let sender = hub
            .claim_head_source()
            .expect("a fresh test hub's head source is unclaimed");
        (hub, sender)
    }

    /// The `Watch` clock's fallback poll provider: a lazy HTTP endpoint (built
    /// without connecting), so the edge holds a provider handle with no live
    /// node behind it.
    async fn dead_loopback_poll_provider() -> Arc<AlloyProvider> {
        Arc::new(
            AlloyProvider::new("http://127.0.0.1:9", DEFAULT_MAX_RETRIES)
                .await
                .expect("a lazy HTTP provider builds without connecting"),
        )
    }

    /// A connection-recording listener: a blocking `std::net::TcpListener` on
    /// an ephemeral loopback port plus a thread that accepts and counts. A
    /// dial that reaches the port increments the counter, so a test can assert
    /// the counter stayed zero (no dial) or grew (a dial landed). `std`, not
    /// `tokio::net`, so the probe does not depend on a tokio `net` feature.
    pub(crate) fn counting_listener() -> (std::net::SocketAddr, Arc<std::sync::atomic::AtomicU64>) {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral loopback port");
        let addr = listener.local_addr().expect("the bound loopback address");
        let accepted = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let counter = Arc::clone(&accepted);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stream.is_err() {
                    break;
                }
                counter.fetch_add(1, Ordering::Relaxed);
            }
        });
        (addr, accepted)
    }

    /// A minimal WS server that serves the head watch's INITIAL connect
    /// (handshake + `eth_subscribe` reply) and then FAILS every later
    /// connection, so the alloy pubsub service spawns, the watch goes live, and
    /// the backend death drives the service's reconnect loop (the storm). This is
    /// the non-vacuous counterpart to [`counting_listener`]: a dead endpoint
    /// (`ws://127.0.0.1:9`) fails the eager connect BEFORE the service spawns
    /// (`alloy-pubsub service.rs`: `connector.connect()?` runs before
    /// `this.spawn()`), so no service -- and no storm -- can exist to observe.
    ///
    /// The accepted-connection COUNT is the observable. The first connection
    /// completes the handshake and answers the `eth_subscribe` (so
    /// `HeadWatch::subscribe` resolves and the watch goes live); every later
    /// connection is accepted then dropped WITHOUT a handshake, so the service's
    /// reconnect attempt errors and it stays in `reconnect_with_retries` (never
    /// returning to the request-channel `select!` arm). Counting accepts is
    /// independent of the best-effort global tracing subscriber (see the sibling
    /// `dropping_the_owner_leaves_no_pubsub_reconnect_storm` note), so the guard
    /// built on this server is non-vacuous even when another test owns the
    /// process-wide subscriber slot.
    ///
    /// Returns the `ws://host:port` URL, the accepted-connection counter, and the
    /// accept task's handle. Promoted `pub(crate)` beside [`counting_listener`]
    /// (the crate-local test-helper convention; see `arb_engine::tests::test_keys`).
    pub(crate) async fn accept_then_die_ws_server() -> (
        String,
        Arc<std::sync::atomic::AtomicU64>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind an ephemeral loopback port");
        let addr = listener.local_addr().expect("the bound loopback address");
        let connections = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let counter = Arc::clone(&connections);
        let handle = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let attempt = counter.fetch_add(1, Ordering::Relaxed) + 1;
                if attempt == 1 {
                    // The head watch's initial connect: complete the handshake,
                    // answer the `eth_subscribe` so the watch goes live, then die.
                    tokio::spawn(serve_initial_connect(stream));
                } else {
                    // A reconnect attempt: fail the handshake so the service's
                    // `reconnect()` errors and it re-enters the retry loop.
                    drop(stream);
                }
            }
        });
        (format!("ws://{addr}"), connections, handle)
    }

    /// Serve the head watch's initial WS connect, then close so the backend dies.
    async fn serve_initial_connect(stream: tokio::net::TcpStream) {
        let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
            return;
        };
        // Answer every request the client sends until the `eth_subscribe`
        // (provider construction may issue a preliminary request first), then
        // give the client time to process the subscription-id reply BEFORE the
        // backend dies -- otherwise `connect_head_watch` falls back to the poller
        // and no storm exists to observe.
        while let Some(Ok(frame)) = ws.next().await {
            let Ok(text) = frame.to_text() else {
                continue;
            };
            let Ok(request) = serde_json::from_str::<serde_json::Value>(text) else {
                continue;
            };
            let id = request
                .get("id")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let method = request
                .get("method")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let result = if method == "eth_subscribe" {
                serde_json::Value::String("0x1".to_string())
            } else {
                serde_json::Value::Null
            };
            let reply = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result,
            })
            .to_string();
            let _ = ws.send(WsMessage::Text(reply.into())).await;
            if method == "eth_subscribe" {
                tokio::time::sleep(Duration::from_millis(500)).await;
                break;
            }
        }
        drop(ws);
    }

    /// The dial-free construction probe: minting with a `Watch` clock AND a
    /// feed arm performs zero connection attempts. Both the head watch and the
    /// feed point at a live listener that counts acceptances, so a dial at mint
    /// would land here — the test is red if either one connects before start.
    #[tokio::test]
    async fn minting_performs_zero_connection_attempts() {
        let (addr, accepted) = counting_listener();
        let (hub, sender) = claimed_hub();
        let hosted = HostedSources::mint(
            Arc::clone(&hub),
            [mev_feed(format!("ws://{addr}"))],
            Some(HostedClockBinding::Watch {
                ws_url: format!("ws://{addr}"),
                provider: dead_loopback_poll_provider().await,
                sender,
            }),
            None,
        )
        .expect("the watch clock and the feed arm mint");

        // A window long enough for any (wrong) dial to land.
        tokio::time::sleep(Duration::from_millis(750)).await;
        assert_eq!(
            accepted.load(Ordering::Relaxed),
            0,
            "mint must perform zero connection attempts"
        );
        drop(hosted);
    }

    /// The deferred dials launch at the start boundary: after `start`, the
    /// head-watch WS connect and the feed pump both reach the listener.
    #[tokio::test]
    async fn starting_performs_the_deferred_dials() {
        let (addr, accepted) = counting_listener();
        let (hub, sender) = claimed_hub();
        let hosted = HostedSources::mint(
            Arc::clone(&hub),
            [mev_feed(format!("ws://{addr}"))],
            Some(HostedClockBinding::Watch {
                ws_url: format!("ws://{addr}"),
                provider: dead_loopback_poll_provider().await,
                sender,
            }),
            None,
        )
        .expect("the watch clock and the feed arm mint");
        assert_eq!(
            accepted.load(Ordering::Relaxed),
            0,
            "no dial before the start boundary"
        );

        hosted.start().await.expect("the deferred I/O starts");

        for _ in 0..500 {
            if accepted.load(Ordering::Relaxed) >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            accepted.load(Ordering::Relaxed) >= 2,
            "start must dial both the head watch and the feed pump"
        );
        drop(hosted);
    }

    /// Dropping a never-started owner is clean: nothing dialed, no actor ever
    /// ran on the owned runtime, and the head edge's mode stays `NotSpawned`
    /// (there was no edge to stop).
    #[tokio::test]
    async fn dropping_a_never_started_owner_tears_down_cleanly() {
        let (addr, accepted) = counting_listener();
        let (hub, sender) = claimed_hub();
        let hosted = HostedSources::mint(
            Arc::clone(&hub),
            [mev_feed(format!("ws://{addr}"))],
            Some(HostedClockBinding::Watch {
                ws_url: format!("ws://{addr}"),
                provider: dead_loopback_poll_provider().await,
                sender,
            }),
            None,
        )
        .expect("the watch clock and the feed arm mint");
        // The mint is the `Provisioned` phase: no dial, no edge, no launch.
        assert_eq!(
            hosted.phase(),
            HostedPhase::Provisioned,
            "a fresh mint is provisioned and unstarted"
        );
        let head_edge = hosted.head_edge();

        drop(hosted);

        // The owned runtime dropped empty: no pump, sampler, or head edge ever
        // spawned, so nothing dials after the drop either.
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(
            accepted.load(Ordering::Relaxed),
            0,
            "a never-started owner must dial nothing, before or after its drop"
        );
        assert_eq!(
            head_edge.mode(),
            HeadEdgeMode::NotSpawned,
            "the head edge never ran, so its mode must not leave NotSpawned"
        );
        assert_eq!(
            head_edge.stop_publications(),
            0,
            "there was no edge to publish Stopped"
        );
    }

    /// Dropping the owner stops the head edge: the edge publishes `Stopped`
    /// exactly once, proving the clean-shutdown path ran (the edge dropped its
    /// WS transport and published the fact) rather than merely being
    /// unscheduled.
    ///
    /// The WS endpoint is the suite's dead loopback: the WS connect fails at
    /// start, the watch falls back to the poller, and the guard asserts the
    /// shutdown path itself — the dead endpoint is never required to connect.
    /// The head edge is deferred to `start`, so the guard starts the owner
    /// before it drops it.
    #[tokio::test]
    async fn dropping_the_owner_stops_the_head_edge() {
        let (hub, sender) = claimed_hub();
        let hosted = HostedSources::mint(
            Arc::clone(&hub),
            [],
            Some(HostedClockBinding::Watch {
                ws_url: "ws://127.0.0.1:9".to_string(),
                provider: dead_loopback_poll_provider().await,
                sender,
            }),
            None,
        )
        .expect("the watch clock mints (the dead WS falls back to the poller)");
        hosted
            .start()
            .await
            .expect("the deferred head edge starts (the dead WS falls back to the poller)");

        let head_edge = hosted.head_edge();
        assert_eq!(
            await_head_edge_mode(
                &head_edge,
                HeadEdgeMode::PollFallback(PollCause::WatchConnectFailed)
            )
            .await,
            HeadEdgeMode::PollFallback(PollCause::WatchConnectFailed),
            "the dead WS connect chooses the poll fallback"
        );

        drop(hosted);

        assert!(
            await_head_edge_stopped(&head_edge).await,
            "dropping the owner must stop the head edge (the clean-shutdown path ran)"
        );
        assert_eq!(
            head_edge.stop_publications(),
            1,
            "the exit publishes Stopped exactly once"
        );
        // Monotone: every later transition is refused, so a late connect outcome
        // cannot revive a stopped edge.
        assert_eq!(
            head_edge.watch_connected(),
            EdgeTransition::RefusedStopped,
            "a stopped edge refuses a late connect outcome"
        );
        assert_eq!(
            head_edge.mode(),
            HeadEdgeMode::Stopped,
            "a stopped edge's publication is final"
        );
    }

    /// The teardown guard, mirroring `degenbot-fork` node.rs's
    /// `leaked_provider_clone_emits_no_reconnect_storm` /
    /// `drop_shuts_down_pubsub_cleanly`: a capture subscriber records the alloy
    /// pubsub service's own events, the owner is dropped, and the observation
    /// window must hold no `Reconnection attempt` /
    /// `Pubsub service backend error` event.
    ///
    /// The global subscriber slot is best-effort (another test in this binary
    /// may own it, and under `--features otel` this test is excluded so it
    /// never competes for it); the positive half — the head edge's clean
    /// shutdown flag — is independent of the subscriber, so the assertion that
    /// the shutdown path ran is never vacuous.
    #[cfg(not(feature = "otel"))]
    #[tokio::test]
    async fn dropping_the_owner_leaves_no_pubsub_reconnect_storm() {
        let _guard = capture::lock().await;
        capture::install();
        let (hub, sender) = claimed_hub();
        let hosted = HostedSources::mint(
            Arc::clone(&hub),
            [],
            Some(HostedClockBinding::Watch {
                ws_url: "ws://127.0.0.1:9".to_string(),
                provider: dead_loopback_poll_provider().await,
                sender,
            }),
            None,
        )
        .expect("the watch clock mints (the dead WS falls back to the poller)");
        hosted
            .start()
            .await
            .expect("the deferred head edge starts (the dead WS falls back to the poller)");
        let head_edge = hosted.head_edge();
        capture::clear();
        drop(hosted);
        assert!(
            await_head_edge_stopped(&head_edge).await,
            "the clean-shutdown path must run"
        );
        // A short window past the drop: a reconnect loop's first attempt
        // precedes any backoff, so it would surface almost immediately.
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        let bad: Vec<String> = capture::events()
            .into_iter()
            .filter(|event| {
                event.contains("Reconnection attempt")
                    || event.contains("Pubsub service backend error")
            })
            .collect();
        assert!(
            bad.is_empty(),
            "dropping the owner left a pubsub reconnect storm: {bad:?}"
        );
    }

    /// Bounded poll for the accepted-connection counter to reach `want`.
    /// `false` on timeout: the caller's non-vacuity assertion then trips instead
    /// of the guard passing vacuously.
    async fn wait_for_connections(
        connections: &Arc<std::sync::atomic::AtomicU64>,
        want: u64,
        window: Duration,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + window;
        while tokio::time::Instant::now() < deadline {
            if connections.load(Ordering::Relaxed) >= want {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        connections.load(Ordering::Relaxed) >= want
    }

    /// The NON-VACUOUS teardown guard. Its sibling
    /// `dropping_the_owner_leaves_no_pubsub_reconnect_storm` runs against a dead
    /// endpoint (`ws://127.0.0.1:9`), so the eager WS connect FAILS before
    /// `PubSubService::connect` spawns its service (`alloy-pubsub service.rs`:
    /// `connector.connect()?` runs first, then `this.spawn()`) -- no service
    /// exists and the no-storm half is vacuously true. This guard points the
    /// head watch at `accept_then_die_ws_server`: the initial connect +
    /// `eth_subscribe` SUCCEED (the service spawns on the owner's dedicated
    /// runtime and the watch goes live), the backend dies, and the service's
    /// reconnect loop is OBSERVED RUNNING before the drop (the non-vacuity
    /// assertion: a SECOND accepted connection proves a reconnect attempt). The
    /// owner is then dropped mid-storm and the guard asserts NO further accepted
    /// connection lands.
    ///
    /// The storm is observed through the server's accepted-connection COUNT, not
    /// the process-wide tracing subscriber: that slot is best-effort (the sibling
    /// guard documents the same), so a capture-based assertion would be silently
    /// vacuous whenever another test in the binary owns the subscriber -- exactly
    /// the defect this guard exists to remove.
    ///
    /// RED-capability: neutralizing the `OwnedPubsubRuntime` teardown in
    /// `Drop for HostedSources` (leaking the runtime instead of dropping it)
    /// leaves the service's reconnect loop alive past the drop, so another
    /// reconnect attempt lands on the server and the connection count grows.
    #[tokio::test]
    async fn dropping_the_owner_mid_storm_leaves_no_new_pubsub_reconnect() {
        let (ws_url, connections, server) = accept_then_die_ws_server().await;
        let (hub, sender) = claimed_hub();
        let hosted = HostedSources::mint(
            Arc::clone(&hub),
            [],
            Some(HostedClockBinding::Watch {
                ws_url,
                provider: dead_loopback_poll_provider().await,
                sender,
            }),
            None,
        )
        .expect("the watch clock mints");
        hosted
            .start()
            .await
            .expect("the head watch connects and subscribes (the server accepts then dies)");

        // Non-vacuity: the initial backend died, so the pubsub service's
        // reconnect loop must land a SECOND connection before the drop.
        let reconnected = wait_for_connections(&connections, 2, Duration::from_secs(10)).await;
        assert!(
            reconnected,
            "vacuous guard: the service never reconnected before the drop (it never spawned, or its backend never died)"
        );

        let head_edge = hosted.head_edge();
        assert_eq!(
            head_edge.mode(),
            HeadEdgeMode::WatchLive,
            "a connected watch reports WatchLive, not a mode inferred from a resource"
        );
        drop(hosted);
        assert!(
            await_head_edge_stopped(&head_edge).await,
            "the clean-shutdown path must run"
        );
        // Let any reconnect accepted at the drop instant land and the dedicated
        // runtime teardown settle, then take the baseline.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let before = connections.load(Ordering::Relaxed);
        // A live reconnect loop (3s base backoff, then exponential) lands
        // another accepted connection inside this window; a torn-down one lands
        // none.
        tokio::time::sleep(Duration::from_secs(5)).await;
        let after = connections.load(Ordering::Relaxed);
        assert_eq!(
            after,
            before,
            "dropping the owner mid-storm left {} new reconnect connections",
            after.saturating_sub(before)
        );
        server.abort();
    }

    /// Criterion 2e: the JOIN provider (`AnyRequestTransport::request_provider`
    /// over the request-scoped URI) takes the SAME `build_provider` WS path as
    /// the head-watch provider. Empirically a WS connect to a dead endpoint
    /// FAILS at `WsConnect::connect` BEFORE `PubSubService::connect` spawns its
    /// service task (alloy-pubsub 2.5.0 `service.rs`: `connector.connect()?`
    /// runs first, then `this.spawn()`), so a dead endpoint leaks NOTHING —
    /// there is no service to retry. This pins that: the connect fails and the
    /// capture subscriber records no reconnect/backend-error event, so no
    /// dedicated-runtime reroute is needed for the join provider.
    ///
    /// (It is green here because the leak does not exist; it would go red if the
    /// connect path ever started spawning a service before a successful
    /// connect.)
    #[cfg(not(feature = "otel"))]
    #[tokio::test]
    async fn a_dead_endpoint_ws_connect_leaks_no_pubsub_service() {
        let _guard = capture::lock().await;
        capture::install();
        capture::clear();
        let outcome = AlloyProvider::new("ws://127.0.0.1:9", DEFAULT_MAX_RETRIES).await;
        assert!(
            outcome.is_err(),
            "a dead loopback WS connect must fail, not connect"
        );
        tokio::time::sleep(Duration::from_millis(1_000)).await;
        let bad: Vec<String> = capture::events()
            .into_iter()
            .filter(|event| {
                event.contains("Reconnection attempt")
                    || event.contains("Pubsub service backend error")
            })
            .collect();
        assert!(
            bad.is_empty(),
            "a failed WS connect leaked a pubsub reconnect storm: {bad:?}"
        );
    }

    // ---- The one authoritative lifecycle -----------------------------------

    /// A feed endpoint whose every dial is refused immediately (a closed local
    /// port), so a pump's reconnect counter witnesses its launch without a live
    /// endpoint.
    const CLOSED_FEED: &str = "ws://127.0.0.1:1";

    /// A `MEVBlocker` arm config pointing at a local endpoint.
    fn mev_feed(url: String) -> PendingTxFeedConfig {
        PendingTxFeedConfig::Mevblocker(BackrunFeedConfig {
            url,
            ..BackrunFeedConfig::for_mainnet()
        })
    }

    /// A hub nobody claimed: the pump-owned clock's post-launch head step fails
    /// on it — the partial-start seam this lifecycle must never project as
    /// success.
    fn unclaimed_head_hub() -> Arc<Hub> {
        Arc::new(Hub::new())
    }

    /// A pump arm over [`CLOSED_FEED`], for a boot that must launch a pump and
    /// then fail later in its start.
    fn closed_feed_arm() -> PendingTxFeedConfig {
        mev_feed(CLOSED_FEED.to_string())
    }

    /// A start that fails AFTER the pumps launched is TERMINAL: the owner is
    /// `HalfStarted` (never running, never success), a second start is a typed
    /// `StartAfterFailure`, and the drop stops everything the partial launch
    /// created. The direct-owner recovery is a WHOLE fresh session and hub, not
    /// a re-mint over this one.
    #[tokio::test]
    async fn an_unclaimed_pump_clock_half_starts_after_pumps_launch() {
        // A live loopback endpoint that counts dials, so the launch is
        // observable and the drop's effect on it is too.
        let (addr, dialed) = counting_listener();
        let hub = unclaimed_head_hub();
        let hosted = HostedSources::mint(
            Arc::clone(&hub),
            [mev_feed(format!("ws://{addr}"))],
            Some(HostedClockBinding::PumpOwned),
            None,
        )
        .expect("the pump and the pump-owned clock mint");
        assert_eq!(
            hosted.phase(),
            HostedPhase::Provisioned,
            "the mint is provisioned and has launched nothing"
        );

        let first = hosted.start().await;
        assert!(
            matches!(first, Err(HostedStartError::HeadSourceUnclaimed)),
            "the unclaimed head source fails the launch: {first:?}"
        );
        assert_eq!(
            hosted.phase(),
            HostedPhase::HalfStarted,
            "a launch that failed after the pumps launched is half-started, never running"
        );

        let second = hosted.start().await;
        assert!(
            matches!(second, Err(HostedStartError::StartAfterFailure)),
            "a second start after a failed launch is a typed refusal, not success: {second:?}"
        );
        assert_eq!(
            hosted.phase(),
            HostedPhase::HalfStarted,
            "the refusal leaves the phase half-started"
        );

        assert!(
            wait_for_connections(&dialed, 1, Duration::from_secs(5)).await,
            "the partial launch's pump dialed its feed"
        );

        // The partial owner's drop is total: the launched pump's reconnect loop
        // ends. Its initial backoff is 250ms, so a still-live loop would land
        // another dial well inside this observation window — nothing leaks a
        // dial or a reconnect.
        drop(hosted);
        let after_drop = dialed.load(Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(900)).await;

        assert_eq!(
            dialed.load(Ordering::Relaxed),
            after_drop,
            "a half-started owner's drop must stop the launched pump (no further dials)"
        );
    }

    /// Every refused start leaves the phase EXACTLY as it found it: a refusal
    /// is never itself a transition. In particular a torn-down owner keeps
    /// answering `TornDown` — a refusal must not park the lifecycle in
    /// `Starting` and hand the NEXT start a bogus `StartInFlight`.
    #[tokio::test]
    async fn every_refused_start_leaves_the_phase_unchanged() {
        // TornDown: repeated post-teardown starts all answer `TornDown`.
        let (hub, _sender) = claimed_hub();
        let mut hosted = HostedSources::mint(
            Arc::clone(&hub),
            [],
            Some(HostedClockBinding::PumpOwned),
            None,
        )
        .expect("the pump-owned clock mints");
        hosted.teardown();
        assert_eq!(hosted.phase(), HostedPhase::TornDown);
        for attempt in 1..=3 {
            let refused = hosted.start().await;
            assert!(
                matches!(refused, Err(HostedStartError::TornDown)),
                "post-teardown start #{attempt} is a typed TornDown refusal: {refused:?}"
            );
            assert_eq!(
                hosted.phase(),
                HostedPhase::TornDown,
                "the refusal leaves the torn-down phase unchanged (attempt #{attempt})"
            );
        }

        // HalfStarted: repeated refusals answer `StartAfterFailure` and the
        // terminal failure never moves.
        let hub = unclaimed_head_hub();
        let hosted = HostedSources::mint(
            Arc::clone(&hub),
            [closed_feed_arm()],
            Some(HostedClockBinding::PumpOwned),
            None,
        )
        .expect("the pump and the pump-owned clock mint");
        let first = hosted.start().await;
        assert!(
            matches!(first, Err(HostedStartError::HeadSourceUnclaimed)),
            "the unclaimed head source fails the launch: {first:?}"
        );
        for attempt in 1..=2 {
            let refused = hosted.start().await;
            assert!(
                matches!(refused, Err(HostedStartError::StartAfterFailure)),
                "half-started start #{attempt} is a typed refusal: {refused:?}"
            );
            assert_eq!(
                hosted.phase(),
                HostedPhase::HalfStarted,
                "the refusal leaves the terminal failure unchanged (attempt #{attempt})"
            );
        }

        // Starting: a concurrent refusal answers `StartInFlight` and the
        // in-flight phase never moves.
        let (hub, _sender) = claimed_hub();
        let hosted = HostedSources::mint(
            Arc::clone(&hub),
            [],
            Some(HostedClockBinding::PumpOwned),
            None,
        )
        .expect("the pump-owned clock mints");
        *hosted.lifecycle.lock() = HostedLifecycle::Starting;
        for attempt in 1..=2 {
            let refused = hosted.start().await;
            assert!(
                matches!(refused, Err(HostedStartError::StartInFlight)),
                "in-flight start #{attempt} is a typed refusal: {refused:?}"
            );
            assert_eq!(
                hosted.phase(),
                HostedPhase::Starting,
                "the refusal leaves the in-flight phase unchanged (attempt #{attempt})"
            );
        }
    }

    /// A launch failure BEFORE the edge task exists still publishes the
    /// once-only terminal fact: the public head-edge projection can never stay
    /// live past a half-started owner's teardown, and a stopped edge is not
    /// revivable — the same single-writer, once-only exit a spawned edge's
    /// clean exit publishes. The pump-owned clock is the one shape with a
    /// fallible post-launch head step; the poll clock's former pre-spawn
    /// failure path no longer exists (its publisher is bound at mint).
    #[tokio::test]
    async fn an_unclaimed_pump_clock_pre_spawn_failure_stops_the_edge_once() {
        // The pump-owned clock publishes `PumpHosted` and THEN finds the hub's
        // head source unclaimed: the launch returns without ever spawning the
        // edge task, so no task exists to publish the exit.
        let (addr, dialed) = counting_listener();
        let hub = unclaimed_head_hub();
        let hosted = HostedSources::mint(
            Arc::clone(&hub),
            [mev_feed(format!("ws://{addr}"))],
            Some(HostedClockBinding::PumpOwned),
            None,
        )
        .expect("the pump and the pump-owned clock mint");
        let head_edge = hosted.head_edge();
        assert_eq!(
            head_edge.mode(),
            HeadEdgeMode::NotSpawned,
            "the mint has spawned no edge"
        );

        let first = hosted.start().await;
        assert!(
            matches!(first, Err(HostedStartError::HeadSourceUnclaimed)),
            "the unclaimed head source fails the launch before the edge task spawns: {first:?}"
        );
        assert_eq!(
            hosted.phase(),
            HostedPhase::HalfStarted,
            "the failed launch is half-started, never running"
        );
        assert_eq!(
            head_edge.mode(),
            HeadEdgeMode::Stopped,
            "a pre-spawn failure publishes the terminal fact; the projection cannot stay live"
        );
        assert_eq!(
            head_edge.stop_publications(),
            1,
            "the exit publishes Stopped exactly once, even with no edge task to publish it"
        );

        // Non-revivable and once-only: a stopped edge refuses every later
        // transition, so neither a late connect outcome nor a second exit moves
        // the projection.
        assert_eq!(
            head_edge.watch_connected(),
            EdgeTransition::RefusedStopped,
            "a stopped edge refuses a late connect outcome"
        );
        assert_eq!(
            head_edge.exited(),
            EdgeTransition::RefusedStopped,
            "a second exit is refused"
        );
        assert_eq!(
            head_edge.mode(),
            HeadEdgeMode::Stopped,
            "a stopped edge is not revivable"
        );
        assert_eq!(
            head_edge.stop_publications(),
            1,
            "the once-only exit cannot double-publish"
        );

        // The resources the partial launch created still transfer to the
        // HalfStarted teardown: the launched pump dialed and then stops with
        // the owner (same no-further-dials guard as the running teardown).
        assert!(
            wait_for_connections(&dialed, 1, Duration::from_secs(5)).await,
            "the partial launch's pump dialed its feed"
        );
        drop(hosted);
        let after_drop = dialed.load(Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(900)).await;
        assert_eq!(
            dialed.load(Ordering::Relaxed),
            after_drop,
            "a half-started owner's drop must stop the launched pump (no further dials)"
        );
        assert_eq!(
            head_edge.mode(),
            HeadEdgeMode::Stopped,
            "the drop neither revives nor re-publishes the projection"
        );
        assert_eq!(head_edge.stop_publications(), 1);
    }

    /// A start from `Running` is an idempotent no-op, and the phase is the one
    /// authoritative answer to "did this owner launch?".
    #[tokio::test]
    async fn a_running_owner_treats_a_second_start_as_an_idempotent_no_op() {
        let (hub, _sender) = claimed_hub();
        let hosted = HostedSources::mint(
            Arc::clone(&hub),
            [],
            Some(HostedClockBinding::PumpOwned),
            None,
        )
        .expect("the pump-owned clock mints");
        hosted.start().await.expect("the pump-owned clock starts");
        assert_eq!(hosted.phase(), HostedPhase::Running);
        hosted
            .start()
            .await
            .expect("a second start from Running is an idempotent no-op");
        assert_eq!(hosted.phase(), HostedPhase::Running);
    }

    /// A concurrent start is a typed `StartInFlight`, and an in-flight launch is
    /// never projected as running.
    ///
    /// The launch is frozen DETERMINISTICALLY — no network, no sleep race: the
    /// owner's dedicated runtime has one worker, and the test parks it on a
    /// never-resolving task, so the connect task the start spawns cannot run and
    /// the admission stays `Starting`.
    #[tokio::test]
    async fn a_concurrent_start_is_typed_in_flight_and_never_running() {
        let (hub, sender) = claimed_hub();
        let hosted = HostedSources::mint(
            Arc::clone(&hub),
            [],
            Some(HostedClockBinding::Watch {
                ws_url: "ws://127.0.0.1:9".to_string(),
                provider: dead_loopback_poll_provider().await,
                sender,
            }),
            None,
        )
        .expect("the watch clock mints");
        {
            let mut lifecycle = hosted.lifecycle.lock();
            let HostedLifecycle::Provisioned { deferred } = &mut *lifecycle else {
                panic!("a fresh mint is provisioned");
            };
            let runtime = deferred
                .pubsub_runtime
                .as_ref()
                .expect("a WS watch owns the dedicated runtime");
            runtime.spawn(std::future::pending::<()>());
        }
        let hosted = Arc::new(hosted);
        let in_flight = Arc::clone(&hosted);
        let first = tokio::spawn(async move { in_flight.start().await });

        let mut admitted = false;
        for _ in 0..500 {
            if hosted.phase() == HostedPhase::Starting {
                admitted = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(
            admitted,
            "the launch must be admitted into Starting (the wedged runtime parks the connect)"
        );

        let concurrent = hosted.start().await;
        assert!(
            matches!(concurrent, Err(HostedStartError::StartInFlight)),
            "a concurrent start is a typed in-flight refusal: {concurrent:?}"
        );
        assert_eq!(
            hosted.phase(),
            HostedPhase::Starting,
            "an in-flight start is never reported as running"
        );

        first.abort();
        let _ = first.await;
        drop(hosted);
    }

    /// A stream is handed out once; the second request is a typed refusal, and
    /// a kind with no minted pump is a DIFFERENT typed refusal. The pump status
    /// stays provisioned-but-not-started until the real launch edge.
    #[tokio::test]
    async fn a_stream_is_handed_out_once_then_refused_until_the_launch_edge() {
        let hub = Arc::new(Hub::new());
        let mut hosted = HostedSources::mint(Arc::clone(&hub), [closed_feed_arm()], None, None)
            .expect("the pump mints");
        let status = hosted
            .pump_status(PendingTxSource::Mevblocker)
            .expect("one pump per arm");
        assert!(
            !status.connected && status.accepted == 0 && status.reconnects == 0,
            "the pump is provisioned but unstarted: {status:?}"
        );
        assert_eq!(
            hosted.phase(),
            HostedPhase::Provisioned,
            "the stream is readable before the launch edge"
        );

        assert!(
            matches!(
                hosted.take_stream(PendingTxSource::Txpool),
                Err(StreamRefusal::NoPump(PendingTxSource::Txpool))
            ),
            "a kind with no minted pump is a typed NoPump refusal"
        );
        hosted
            .take_stream(PendingTxSource::Mevblocker)
            .expect("the arm's stream is handed out");
        assert!(
            matches!(
                hosted.take_stream(PendingTxSource::Mevblocker),
                Err(StreamRefusal::AlreadyTaken(PendingTxSource::Mevblocker))
            ),
            "the once-only stream is a typed AlreadyTaken refusal"
        );
    }

    /// The head edge's ONE transition owner: every boot, connect, and exit
    /// outcome is a named transition of the value that publishes the mode, so no
    /// task-local copy can disagree with the projection.
    #[tokio::test]
    async fn the_head_edge_owns_every_transition_it_publishes() {
        let edge = HeadEdge::minted();
        assert_eq!(
            edge.mode(),
            HeadEdgeMode::NotSpawned,
            "a mint spawns no edge"
        );
        assert_eq!(
            edge.iteration(),
            HeadIteration::Exit,
            "an unspawned edge has no loop"
        );
        assert_eq!(
            edge.boot(ClockShape::Watching),
            EdgeTransition::Applied(HeadEdgeMode::WatchConnecting),
            "a watch clock boots connecting"
        );
        assert_eq!(
            edge.mode(),
            HeadEdgeMode::WatchConnecting,
            "the boot publishes the mode it entered"
        );
        assert_eq!(edge.iteration(), HeadIteration::AwaitHead);
        assert_eq!(
            edge.watch_connected(),
            EdgeTransition::Applied(HeadEdgeMode::WatchLive),
            "watch_connected is a named transition"
        );
        assert_eq!(edge.mode(), HeadEdgeMode::WatchLive);

        let failed = HeadEdge::minted();
        failed.boot(ClockShape::Watching);
        assert_eq!(
            failed.watch_connect_failed(),
            EdgeTransition::Applied(HeadEdgeMode::PollFallback(PollCause::WatchConnectFailed)),
            "watch_connect_failed records the cause as data"
        );
        assert_eq!(
            failed.iteration(),
            HeadIteration::Poll(PollCause::WatchConnectFailed)
        );

        let primary = HeadEdge::minted();
        primary.boot(ClockShape::Polling);
        assert_eq!(
            primary.mode(),
            HeadEdgeMode::PollFallback(PollCause::PrimaryPoll),
            "a boot hosting no watch polls as its primary clock"
        );
        assert_eq!(
            primary.iteration(),
            HeadIteration::Poll(PollCause::PrimaryPoll)
        );

        let pump = HeadEdge::minted();
        pump.boot(ClockShape::PumpOwned);
        assert_eq!(
            pump.mode(),
            HeadEdgeMode::PumpHosted,
            "the pump-owned hub source is its own mode"
        );
        assert_eq!(
            pump.watch_connected(),
            EdgeTransition::Applied(HeadEdgeMode::PumpHosted),
            "a transition with no successor leaves the mode where it found it"
        );
        assert_eq!(pump.mode(), HeadEdgeMode::PumpHosted);
    }

    /// The exit is the end of the line: `Stopped` is published exactly once and
    /// every later transition is refused, so no late connect outcome revives the
    /// projection.
    #[tokio::test]
    async fn a_stopped_edge_refuses_every_later_transition() {
        let edge = HeadEdge::minted();
        edge.boot(ClockShape::Watching);
        edge.watch_connected();
        assert_eq!(
            edge.exited(),
            EdgeTransition::Applied(HeadEdgeMode::Stopped)
        );
        assert_eq!(
            edge.stop_publications(),
            1,
            "the exit publishes Stopped once"
        );

        let refusals = [
            edge.watch_connected(),
            edge.watch_connect_failed(),
            edge.boot(ClockShape::Polling),
            edge.exited(),
        ];
        for refused in refusals {
            assert_eq!(
                refused,
                EdgeTransition::RefusedStopped,
                "a stopped edge refuses every later transition"
            );
        }
        assert_eq!(
            edge.mode(),
            HeadEdgeMode::Stopped,
            "no refused transition revives the exit"
        );
        assert_eq!(
            edge.stop_publications(),
            1,
            "the once-only exit cannot double-publish"
        );
        assert_eq!(
            edge.iteration(),
            HeadIteration::Exit,
            "a stopped edge has no iteration left"
        );
    }

    /// The fallback poll is the edge's OWN decision: a quiet watch polls one
    /// iteration through its staleness policy and STAYS `WatchLive`, a closed
    /// subscription polls with that cause recorded, and a live watch inside its
    /// bound observes nothing.
    #[tokio::test]
    async fn a_watch_edge_polls_only_through_its_own_policy() {
        let edge = HeadEdge::minted();
        edge.boot(ClockShape::Watching);

        // Still connecting: the edge has not gone live, so it polls only for a
        // source that actually closed — never for mere quiet.
        assert_eq!(
            edge.fallback_poll(SourceSignal::Quiet { stale: false }),
            None,
            "a connecting edge does not poll a quiet source"
        );
        assert_eq!(
            edge.fallback_poll(SourceSignal::Quiet { stale: true }),
            None,
            "a connecting edge does not poll a stale-but-quiet source either"
        );
        assert_eq!(
            edge.fallback_poll(SourceSignal::Closed),
            Some(PollReason::SourceClosed),
            "a connecting edge polls only for a source that actually closed"
        );

        edge.watch_connected();
        assert_eq!(
            edge.fallback_poll(SourceSignal::Quiet { stale: false }),
            None,
            "a live watch inside its staleness bound observes nothing"
        );
        assert_eq!(
            edge.fallback_poll(SourceSignal::Quiet { stale: true }),
            Some(PollReason::StaleWatch),
            "a stale watch polls for this iteration while its watchdog reconnects"
        );
        assert_eq!(
            edge.mode(),
            HeadEdgeMode::WatchLive,
            "the stale poll is one iteration's read, not a mode: the reconnecting watch resumes watching"
        );
        assert_eq!(
            edge.fallback_poll(SourceSignal::Closed),
            Some(PollReason::SourceClosed),
            "a closed source polls with its own cause, not the staleness policy's"
        );
    }

    /// The pump-hosted edge consumes the pump's source and owns no provider, so
    /// it NEVER polls: not when the source is quiet, and not when it closes.
    #[tokio::test]
    async fn the_pump_hosted_edge_never_polls() {
        let edge = HeadEdge::minted();
        edge.boot(ClockShape::PumpOwned);
        assert_eq!(edge.mode(), HeadEdgeMode::PumpHosted);
        assert_eq!(edge.iteration(), HeadIteration::AwaitHead);
        for signal in [SourceSignal::Quiet { stale: true }, SourceSignal::Closed] {
            assert_eq!(
                edge.fallback_poll(signal),
                None,
                "the pump owns the clock, so this edge has no provider to poll ({signal:?})"
            );
        }
        assert_eq!(
            edge.mode(),
            HeadEdgeMode::PumpHosted,
            "no poll fallback moves the pump-hosted mode"
        );
    }

    /// A closed pump-hosted clock cannot be revived by a poll: the edge owns no
    /// provider, so a closed subscription paces one iteration and observes
    /// nothing — it never falls back to reading a head of its own.
    #[tokio::test]
    async fn a_closed_pump_hosted_clock_paces_without_polling() {
        let hub = Arc::new(Hub::new());
        let publisher = hub
            .claim_head_source()
            .expect("a fresh test hub's head source is unclaimed");
        drop(publisher);
        let mut head = hub.subscribe_head();
        assert!(
            head.changed().await.is_err(),
            "every publisher dropped, so the claimed clock is closed"
        );

        let edge = HeadEdge::minted();
        edge.boot(ClockShape::PumpOwned);
        assert_eq!(edge.fallback_poll(SourceSignal::Closed), None);

        let mut source = HeadEdgeSource::PumpOwned { subscription: head };
        let started = std::time::Instant::now();
        let observed = head_iteration(&mut source, &edge, None, 0).await;
        assert!(
            observed.is_none(),
            "a closed pump-hosted clock observes nothing"
        );
        assert!(
            started.elapsed() >= HEAD_POLL_TICK,
            "the closed-clock iteration paces instead of spinning"
        );
        assert_eq!(
            edge.mode(),
            HeadEdgeMode::PumpHosted,
            "a closed source does not move the pump-hosted mode"
        );
    }

    /// A closed hosted-watch clock polls with its own cause and stays
    /// `WatchLive`: the owner's provider is the fallback, and a reconnect can
    /// resume watching.
    #[tokio::test]
    async fn a_closed_hosted_watch_polls_with_source_closed() {
        let (hub, publisher) = claimed_hub();
        drop(publisher);
        let mut head = hub.subscribe_head();
        assert!(
            head.changed().await.is_err(),
            "every publisher dropped, so the claimed clock is closed"
        );

        let edge = HeadEdge::minted();
        edge.boot(ClockShape::Watching);
        edge.watch_connected();
        assert_eq!(
            edge.fallback_poll(SourceSignal::Closed),
            Some(PollReason::SourceClosed),
            "a live watch polls a closed source with that cause"
        );
        assert_eq!(
            edge.fallback_poll(SourceSignal::Quiet { stale: false }),
            None,
            "a closed source is not polled through the staleness policy"
        );

        let (_publisher_hub, poll_publisher) = claimed_hub();
        let mut source = HeadEdgeSource::Hosted {
            sender: poll_publisher,
            subscription: head,
        };
        let started = std::time::Instant::now();
        let observed = head_iteration(&mut source, &edge, None, 0).await;
        assert!(
            observed.is_none(),
            "the dead loopback provider answers no block"
        );
        assert!(
            started.elapsed() >= HEAD_POLL_TICK,
            "the closed subscription took the polled-read path"
        );
        assert_eq!(
            edge.mode(),
            HeadEdgeMode::WatchLive,
            "the poll is one iteration's decision, not a mode"
        );
    }

    /// Provenance publication: a head the edge polled for exists nowhere else, so
    /// the edge writes it into the hub for every driver; a head the hub delivered
    /// was already published by its owner, so the edge must not write it twice.
    #[tokio::test]
    async fn the_edge_publishes_only_its_own_polled_reads() {
        let (hub, sender) = claimed_hub();
        let mut head = hub.subscribe_head();

        let published = publish_observed(
            Some(&sender),
            HeadObservation::Polled {
                number: 9,
                reason: PollReason::SourceClosed,
            },
            0,
        );
        assert_eq!(published, 9, "a polled head advances the high-water mark");
        assert_eq!(
            head.borrow_and_update().map(|head| head.number),
            Some(9),
            "the polled head reaches the hub's readers"
        );
        assert_eq!(
            head.head().and_then(|head| head.metadata),
            None,
            "a polled read carries the block number only"
        );

        let published = publish_observed(Some(&sender), HeadObservation::Watched(12), published);
        assert_eq!(published, 9, "a watched head is not the edge's own read");
        assert_eq!(
            head.borrow_and_update().map(|head| head.number),
            Some(9),
            "the edge did not write the watched head"
        );

        let published = publish_observed(
            Some(&sender),
            HeadObservation::Polled {
                number: 9,
                reason: PollReason::StaleWatch,
            },
            published,
        );
        assert_eq!(published, 9, "a re-read head is deduped, not re-published");
    }

    /// The teardown is ONE ordered plan from EVERY phase: the deferred runtime
    /// from a never-started owner, the transport before the runtime from a
    /// running or half-started one, and nothing at all from a torn-down owner.
    #[tokio::test]
    async fn the_teardown_plan_is_total_and_ordered() {
        let transport = detached_transport().await;
        let (_hub, publisher) = claimed_hub();
        let plan_of = |lifecycle: HostedLifecycle| {
            let mut lifecycle = lifecycle;
            lifecycle.teardown()
        };

        let runtime = || {
            OwnedPubsubRuntime::build(OwnedPubsubRuntimeSpec {
                resource: "hosted_pubsub_workers_teardown_plan_test",
                kind: "test dedicated runtime (hosted teardown plan)",
                thread_name: "degenbot-hosted-teardown-plan",
                sizing: "test: exactly one worker",
                binding: "pinned",
            })
            .expect("build the test dedicated runtime")
        };

        let provisioned = plan_of(HostedLifecycle::Provisioned {
            deferred: Deferred {
                clock: None,
                trigger: None,
                pubsub_runtime: Some(runtime()),
            },
        });
        assert!(
            provisioned.head_ws_provider.is_none(),
            "nothing has connected, so a provisioned owner has no transport"
        );
        assert!(
            provisioned.head_publisher.is_none(),
            "a clockless deferred set holds no publisher"
        );
        assert!(
            provisioned.pubsub_runtime.is_some(),
            "a provisioned owner still tears its dedicated runtime down"
        );
        drop(provisioned);

        for lifecycle in [HostedLifecycle::Starting, HostedLifecycle::TornDown] {
            let plan = plan_of(lifecycle);
            assert!(
                plan.head_ws_provider.is_none()
                    && plan.pubsub_runtime.is_none()
                    && plan.head_publisher.is_none(),
                "a start-in-flight holds the deferred set itself, and a torn-down owner holds nothing"
            );
        }

        for lifecycle in [
            HostedLifecycle::Running {
                live: Live {
                    head_publisher: Some(publisher.clone()),
                    head_ws_provider: Some(Arc::clone(&transport)),
                    pubsub_runtime: Some(runtime()),
                },
            },
            HostedLifecycle::HalfStarted {
                live: Live {
                    head_publisher: Some(publisher.clone()),
                    head_ws_provider: Some(Arc::clone(&transport)),
                    pubsub_runtime: Some(runtime()),
                },
            },
        ] {
            let phase = lifecycle.phase();
            let plan = plan_of(lifecycle);
            assert!(
                plan.head_ws_provider.is_some(),
                "{phase:?} must hand the transport to the teardown"
            );
            assert!(
                plan.pubsub_runtime.is_some(),
                "{phase:?} must hand the dedicated runtime to the teardown"
            );
            assert!(
                plan.head_publisher.is_some(),
                "{phase:?} must hand the claimed head publisher to the teardown"
            );
            drop(plan);
        }
    }

    /// Sets its flag when the wrapped future is DROPPED — the witness that a
    /// runtime teardown cancelled the parked task.
    struct DropWitness(Arc<AtomicBool>);

    impl Drop for DropWitness {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// A bounded poll over a test flag (the teardown runs on a detached thread
    /// when the owner is dropped inside an async context).
    async fn await_flag(flag: &Arc<AtomicBool>) -> bool {
        for _ in 0..1_000 {
            if flag.load(Ordering::SeqCst) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        flag.load(Ordering::SeqCst)
    }

    /// `Drop` is total from every phase, including a HALF-STARTED owner that
    /// created its WS transport before the failure: the transport frontend is
    /// released and the dedicated runtime is torn down (its parked task is
    /// cancelled).
    #[tokio::test]
    async fn every_lifecycle_phase_drops_totally() {
        // 1. The phases that hold no launched resources.
        for phase in [
            HostedPhase::Provisioned,
            HostedPhase::Starting,
            HostedPhase::TornDown,
        ] {
            let hub = Arc::new(Hub::new());
            let hosted = HostedSources::mint(Arc::clone(&hub), [closed_feed_arm()], None, None)
                .expect("the pump mints");
            match phase {
                HostedPhase::Starting => *hosted.lifecycle.lock() = HostedLifecycle::Starting,
                HostedPhase::TornDown => *hosted.lifecycle.lock() = HostedLifecycle::TornDown,
                HostedPhase::Provisioned => {}
                other => panic!("no launched resources in {other:?}"),
            }
            drop(hosted);
        }

        // 2. The phases that DO hold a transport and a dedicated runtime: a
        //    failure before (HalfStarted, no transport) and after (with a
        //    transport) transport creation both tear down in order.
        let mut transport_created = false;
        for phase in [HostedPhase::HalfStarted, HostedPhase::Running] {
            let hub = Arc::new(Hub::new());
            let hosted = HostedSources::mint(Arc::clone(&hub), [closed_feed_arm()], None, None)
                .expect("the pump mints");
            let runtime = OwnedPubsubRuntime::build(OwnedPubsubRuntimeSpec {
                resource: "hosted_pubsub_workers_total_drop_test",
                kind: "test dedicated runtime (hosted total drop)",
                thread_name: "degenbot-hosted-total-drop",
                sizing: "test: exactly one worker",
                binding: "pinned",
            })
            .expect("build the test dedicated runtime");
            let started = Arc::new(AtomicBool::new(false));
            let cancelled = Arc::new(AtomicBool::new(false));
            let (started_task, cancelled_task) = (Arc::clone(&started), Arc::clone(&cancelled));
            runtime.spawn(async move {
                let _witness = DropWitness(cancelled_task);
                started_task.store(true, Ordering::SeqCst);
                std::future::pending::<()>().await;
            });
            assert!(
                await_flag(&started).await,
                "the parked task must start before the drop"
            );
            let transport: Option<Arc<dyn Provider<Ethereum>>> = if transport_created {
                None
            } else {
                Some(detached_transport().await)
            };
            let released = transport.as_ref().map(Arc::downgrade);
            let live = Live {
                head_publisher: None,
                head_ws_provider: transport,
                pubsub_runtime: Some(runtime),
            };
            *hosted.lifecycle.lock() = match phase {
                HostedPhase::Running => HostedLifecycle::Running { live },
                _ => HostedLifecycle::HalfStarted { live },
            };
            assert_eq!(hosted.phase(), phase);

            drop(hosted);

            if let Some(released) = released {
                assert!(
                    released.upgrade().is_none(),
                    "{phase:?}: the WS transport frontend is released by the teardown"
                );
            }
            assert!(
                await_flag(&cancelled).await,
                "{phase:?}: the dedicated runtime is torn down, cancelling its tasks"
            );
            transport_created = true;
        }
    }

    /// A capture subscriber mirroring `degenbot-fork` node.rs: it records the
    /// alloy pubsub service's events so a teardown test can assert no
    /// reconnect/backend-error storm. `install` is best-effort (first-wins on
    /// the process-wide subscriber slot).
    #[cfg(not(feature = "otel"))]
    mod capture {
        use std::sync::{Arc, OnceLock};

        type Events = Arc<parking_lot::Mutex<Vec<String>>>;

        static CAPTURED: OnceLock<Events> = OnceLock::new();

        /// Serializes the clear/observe window across tests (one process-wide
        /// capture vec); a `tokio` mutex because the guard is held across
        /// `await`s.
        static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

        struct CaptureLayer {
            events: Events,
        }

        impl<S> tracing_subscriber::Layer<S> for CaptureLayer
        where
            S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
        {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                let meta = event.metadata();
                if meta.target() != "alloy_pubsub::service" {
                    return;
                }
                let mut visitor = degenbot_core::log_capture::MessageVisitor::default();
                event.record(&mut visitor);
                let message = format!(
                    "[{}] {}",
                    meta.level(),
                    visitor
                        .into_message()
                        .unwrap_or_else(|| "<no message>".into())
                );
                self.events.lock().push(message);
            }
        }

        pub(super) fn install() {
            static INIT: std::sync::Once = std::sync::Once::new();
            INIT.call_once(|| {
                use tracing_subscriber::layer::SubscriberExt as _;
                use tracing_subscriber::util::SubscriberInitExt as _;
                let events: Events = Arc::new(parking_lot::Mutex::new(Vec::new()));
                // A re-install (or a slot another test already holds) is fine:
                // the capture is best-effort and the positive assertion does
                // not depend on it.
                let _ = CAPTURED.set(Arc::clone(&events));
                let _ = tracing_subscriber::registry()
                    .with(CaptureLayer { events })
                    .try_init();
            });
        }

        pub(super) async fn lock() -> tokio::sync::MutexGuard<'static, ()> {
            LOCK.lock().await
        }

        pub(super) fn clear() {
            if let Some(events) = CAPTURED.get() {
                events.lock().clear();
            }
        }

        #[must_use]
        pub(super) fn events() -> Vec<String> {
            CAPTURED
                .get()
                .map(|events| events.lock().clone())
                .unwrap_or_default()
        }
    }
}
