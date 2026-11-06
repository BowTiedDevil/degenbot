//! `pump_session` — the pump session's lifecycle, as one machine.
//!
//! The phase is not an ordering, because the session is not a ladder:
//! `subscribe` is legal from `Detached` **or** from a phase that already loaded
//! a snapshot, because the snapshot may be loaded before or after the WS
//! subscribe. A comparison cannot say that. So the states are named, and
//! [`transition`] is the whole legality table: one total function over (pose,
//! event) that answers every pair. Anything not allowed is a typed
//! [`PumpDecline`], never a silent no-op.
//!
//! # The pose carries its own data (ADR-093 D2)
//!
//! Legality is only half of the machine. A pose that is a bare byte beside
//! payload slots lets a payload move the table no longer admits still install
//! itself: a live pump armed onto a `Stopped` session, a crawl booted over one
//! (ADR-093 §1.3). So the private [`PumpPose`] **owns** what that pose holds:
//! `Subscribed` owns the pump and the live stream, `Running` owns the live-loop
//! handle. [`PumpSession::take`] moves a payload OUT under the lock and
//! [`SessionMove`]'s land methods are the only ways back in, each re-checking
//! the table first. A dropped move publishes its abandonment edge and drops its
//! payload (aborting a loop it cannot install), so no cancellation strands the
//! session and no refused landing leaves a task running.
//!
//! Nothing here awaits: the take-out and the land are synchronous, and the
//! driver's awaits live between them, which is what keeps a `parking_lot` guard
//! from ever crossing an await.

use std::fmt;
use std::sync::Arc;

use degenbot_core::{op_error, op_info};
use degenbot_ingestion::IngestEvent as WsEvent;
use parking_lot::Mutex;
use tokio::sync::{watch, Notify};
use tokio::task::JoinHandle;

use crate::arb_engine::crawl::controller::CrawlHandle;
use crate::bot_core::block_pump::BlockPump;

/// Where a pump session is. A `Copy` projection: readers see this, never a
/// handle into the session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PumpPhase {
    /// No WS connection and nothing pending.
    #[default]
    Detached,
    /// The subscribe handshake is in flight.
    Connecting,
    /// The first complete block is observed and the stream is open; `resume`
    /// is legal.
    Subscribed,
    /// The `S+1..W` backfill is in flight.
    Resuming,
    /// The live loop is spawned and running.
    Running,
    /// The live loop returned without a `Stop` — stream end, panic, or abort.
    Ended,
    /// A resume did not commit — cancelled mid-flight, or its `S+1..W` backfill
    /// failed. Either way the WS session is gone and no live loop runs.
    ///
    /// Terminal for recovery: `stop` is its one exit. The re-subscribe edge is
    /// refused ([`PumpDecline::SubscribeAfterAbandonment`]) because the
    /// backfill buffer stages events without dedup — a retry over a partially
    /// staged attempt would double-apply liquidity events at registration.
    /// Recovery is a new session. (Not [`PumpPhase::is_terminal`]: teardown
    /// has not begun, so `stop` from here still owes its own work.)
    Abandoned,
    /// `Stop` latched; teardown is in progress.
    Stopping,
    /// Terminal.
    Stopped,
}

impl PumpPhase {
    /// The lower-case name, for the FFI's name translator and for error text.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Detached => "detached",
            Self::Connecting => "connecting",
            Self::Subscribed => "subscribed",
            Self::Resuming => "resuming",
            Self::Running => "running",
            Self::Ended => "ended",
            Self::Abandoned => "abandoned",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
        }
    }

    /// Whether teardown has begun or finished.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Stopping | Self::Stopped)
    }

    /// Whether a live loop exists or is being started — the fact teardown
    /// needs to decide whether anything must be joined.
    #[must_use]
    pub const fn live_loop_armed(self) -> bool {
        matches!(self, Self::Resuming | Self::Running)
    }

    /// Whether the session holds a WS subscription (directly, or through the
    /// live loop that owns the pump).
    #[must_use]
    pub const fn holds_subscription(self) -> bool {
        matches!(
            self,
            Self::Connecting | Self::Subscribed | Self::Resuming | Self::Running
        )
    }
}

impl fmt::Display for PumpPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Display for PumpEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What happened. Events carry no payload: the payload moves are the session's
/// (a move hands the pump and the ticket over), while this table owns
/// legality.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PumpEvent {
    /// Open the WS subscribe handshake.
    SubscribeRequested,
    /// The handshake completed and produced a resumable session.
    Subscribed,
    /// The handshake failed.
    SubscribeFailed,
    /// The future driving the handshake was dropped before it committed.
    SubscribeAbandoned,
    /// Begin the backfill and start the live loop.
    ResumeRequested,
    /// The live loop was spawned.
    ResumeSpawned,
    /// The resume did not commit: the future driving it was dropped mid-flight,
    /// or its `S+1..W` backfill returned `Err`.
    ResumeAbandoned,
    /// The live loop returned on its own.
    PumpExited,
    /// A crawl was attached to this session.
    ///
    /// A table event and not a side guard inside the crawl boot: attach legality
    /// has to be answered by the table alone, or the table is a partial
    /// authority and "no crawl after the session is gone" is left to whatever
    /// an entry point remembers (ADR-093 D2, the §1.3(b) regression).
    CrawlAttached,
    /// Tear the session down.
    Stop,
    /// Teardown finished.
    StopCompleted,
}

impl PumpEvent {
    /// The lower-case name, for error text.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SubscribeRequested => "subscribe",
            Self::Subscribed => "subscribed",
            Self::SubscribeFailed => "subscribe_failed",
            Self::SubscribeAbandoned => "subscribe_abandoned",
            Self::ResumeRequested => "resume",
            Self::ResumeSpawned => "resume_spawned",
            Self::ResumeAbandoned => "resume_abandoned",
            Self::PumpExited => "pump_exited",
            Self::CrawlAttached => "crawl_attached",
            Self::Stop => "stop",
            Self::StopCompleted => "stop_completed",
        }
    }
}

/// Why a session move was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PumpDecline {
    /// `subscribe` is legal only from [`PumpPhase::Detached`].
    #[error("cannot subscribe from {current}; subscribe requires detached")]
    SubscribeRequiresDetached {
        /// The pose the request arrived in.
        current: PumpPhase,
    },
    /// `subscribe` arrived at a session whose resume was abandoned.
    ///
    /// [`PumpPhase::Abandoned`] is terminal, so `stop` is its one exit: a
    /// re-subscribe could double-apply the partially staged backfill buffer.
    #[error(
        "cannot subscribe: the session was abandoned and only stop is legal; restart the session to recover (a re-subscribe could double-apply its staged backfill)"
    )]
    SubscribeAfterAbandonment,
    /// `resume` is legal only from [`PumpPhase::Subscribed`].
    #[error("cannot resume from {current}; resume requires subscribed")]
    ResumeRequiresSubscribed {
        /// The pose the request arrived in.
        current: PumpPhase,
    },
    /// A resume was abandoned and the WS session is gone.
    #[error("the previous resume was abandoned; subscribe again before resuming")]
    ResumeAfterAbandonment,
    /// A crawl attach arrived at a session that is gone or tearing down.
    #[error("cannot attach a crawl from {current}; the session is gone or tearing down")]
    CrawlRefused {
        /// The pose the request arrived in.
        current: PumpPhase,
    },
    /// Another lifecycle move is in flight; the session admits one at a time.
    #[error("cannot begin {event} from {current}: another session move is in flight")]
    MoveInFlight {
        /// The in-flight pose the request arrived at.
        current: PumpPhase,
        /// The move that was refused.
        event: PumpEvent,
    },
    /// The event does not apply in the current pose.
    #[error("cannot apply {event} from {current}")]
    EventOutOfOrder {
        /// The pose the event arrived in.
        current: PumpPhase,
        /// The event that does not apply.
        event: PumpEvent,
    },
    /// The session is terminal.
    #[error("cannot apply {event}: the session is {current}")]
    Stopped {
        /// The terminal pose.
        current: PumpPhase,
        /// The event that was refused.
        event: PumpEvent,
    },
}

impl PumpDecline {
    /// The pose the refusal names — the pose the request arrived in, or the
    /// terminal `Abandoned` for the two abandonment refusals.
    #[must_use]
    pub const fn current(self) -> PumpPhase {
        match self {
            Self::SubscribeRequiresDetached { current }
            | Self::ResumeRequiresSubscribed { current }
            | Self::CrawlRefused { current }
            | Self::MoveInFlight { current, .. }
            | Self::EventOutOfOrder { current, .. }
            | Self::Stopped { current, .. } => current,
            Self::SubscribeAfterAbandonment | Self::ResumeAfterAbandonment => PumpPhase::Abandoned,
        }
    }
}

/// Where `Stop` lands a session — total over every pose, and the teardown
/// table's whole row.
///
/// A session with nothing to tear down lands [`PumpPhase::Stopped`] at once;
/// one holding something to tear down lands [`PumpPhase::Stopping`] and owes
/// one `StopCompleted` when the teardown returns.
#[must_use]
pub const fn stop_pose(pose: PumpPhase) -> PumpPhase {
    match pose {
        PumpPhase::Detached | PumpPhase::Ended | PumpPhase::Abandoned | PumpPhase::Stopped => {
            PumpPhase::Stopped
        }
        PumpPhase::Connecting
        | PumpPhase::Subscribed
        | PumpPhase::Resuming
        | PumpPhase::Running
        | PumpPhase::Stopping => PumpPhase::Stopping,
    }
}

/// The whole legality table: (pose, event) -> next pose, or a typed decline.
///
/// Total over every pair — no combination is undefined and none is a silent
/// no-op. `Stop` is the only event that never declines, because teardown is
/// legal from every pose; `CrawlAttached` is the only event that leaves the
/// pose where it found it, because it moves payload and not the lifecycle.
///
/// # Errors
///
/// [`PumpDecline::SubscribeRequiresDetached`] when `subscribe` arrives at a
/// session that is already past `Detached`;
/// [`PumpDecline::SubscribeAfterAbandonment`] when it arrives at an
/// [`PumpPhase::Abandoned`] one; [`PumpDecline::ResumeRequiresSubscribed`] when
/// `resume` arrives without a resumable session;
/// [`PumpDecline::ResumeAfterAbandonment`] when a resume is attempted after a
/// cancelled one; [`PumpDecline::CrawlRefused`] when a crawl attach arrives at
/// a session that is gone or tearing down; [`PumpDecline::EventOutOfOrder`] for
/// an event the pose does not expect; and [`PumpDecline::Stopped`] against a
/// session that is tearing down or already stopped.
pub fn transition(pose: PumpPhase, event: PumpEvent) -> Result<PumpPhase, PumpDecline> {
    use PumpEvent as E;
    use PumpPhase as P;

    // Teardown is total. It declines nothing and never leaves a terminal pose.
    if matches!(event, E::Stop) {
        return Ok(stop_pose(pose));
    }

    // `Stopped` is absorbing: it refuses everything else, loudly and by name.
    // `Stopping` is not absorbing — it still owes one `StopCompleted`.
    if pose == P::Stopped {
        return Err(PumpDecline::Stopped {
            current: pose,
            event,
        });
    }

    match (pose, event) {
        // subscribe: the one door in, and only from a clean start. `Abandoned`
        // does not reopen it — see its own row below.
        (P::Detached, E::SubscribeRequested) => Ok(P::Connecting),

        // handshake outcome
        (P::Connecting, E::Subscribed) => Ok(P::Subscribed),
        (P::Connecting, E::SubscribeFailed | E::SubscribeAbandoned) => Ok(P::Detached),

        // resume
        (P::Subscribed, E::ResumeRequested) => Ok(P::Resuming),
        (P::Resuming, E::ResumeSpawned) => Ok(P::Running),
        // Cancelled or backfill-failed: both leave the pump and its WS session
        // gone with nothing to resume, so `Abandoned` is the honest landing.
        // A failure's typed error travels beside the pose (the caller's
        // `Err`), never through it — the table carries no payloads.
        (P::Resuming, E::ResumeAbandoned) => Ok(P::Abandoned),

        // the live loop returned without a stop
        (P::Running, E::PumpExited) => Ok(P::Ended),

        // teardown completion
        (P::Stopping, E::StopCompleted) => Ok(P::Stopped),

        // A tearing-down session owes one `StopCompleted` and nothing else.
        (P::Stopping, _) => Err(PumpDecline::Stopped {
            current: pose,
            event,
        }),

        // A cancelled resume cannot be retried: the pump and its WS session
        // were dropped with the future.
        (P::Abandoned, E::ResumeRequested) => Err(PumpDecline::ResumeAfterAbandonment),

        // Nor re-subscribed: `Abandoned` is terminal. The backfill buffer stages
        // events without dedup, so a second attempt over a partially staged one
        // would double-apply liquidity events at registration; recovery is a
        // new session.
        (P::Abandoned, E::SubscribeRequested) => Err(PumpDecline::SubscribeAfterAbandonment),

        // `subscribe` from a live session is not a recovery, it is a mistake.
        (_, E::SubscribeRequested) => Err(PumpDecline::SubscribeRequiresDetached { current: pose }),

        // `resume` anywhere but `Subscribed` has nothing to resume.
        (_, E::ResumeRequested) => Err(PumpDecline::ResumeRequiresSubscribed { current: pose }),

        // A crawl attach is legal while the session is alive: the crawl runs
        // over the same `Bot` and stops with the session's teardown.
        (
            P::Detached | P::Connecting | P::Subscribed | P::Resuming | P::Running,
            E::CrawlAttached,
        ) => Ok(pose),
        (_, E::CrawlAttached) => Err(PumpDecline::CrawlRefused { current: pose }),

        // Everything else is an event arriving out of order.
        _ => Err(PumpDecline::EventOutOfOrder {
            current: pose,
            event,
        }),
    }
}

/// One published transition: the pose the session held and the pose the table
/// landed it on.
///
/// [`transition`] answers legality; this is the answer of a projection that has
/// already written the landing, so a caller can tell "I began this move" from
/// "the session was already there" (teardown idempotence reads the pair that
/// way).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PhaseAdvance {
    /// The pose the event arrived in.
    pub(crate) from: PumpPhase,
    /// The pose the table landed on.
    pub(crate) to: PumpPhase,
}

/// The session between `connect` and `run`: the subscribed pump, the first
/// complete block, and the live WS stream the pump will drive.
///
/// The `Subscribed` pose OWNS this value, so "the session is subscribed" and
/// "there is a pump to resume" cannot disagree.
pub(crate) struct ConnectedSession {
    /// The subscribed pump (owns the WS transport + the live stream).
    pub(crate) pump: BlockPump,
    /// The first complete WS block `W` — the backfill's upper bound and
    /// `Connected::link_block`.
    pub(crate) first_block: u64,
    /// The live WS event stream, handed to the pump at resume.
    pub(crate) stream: futures_util::stream::BoxStream<'static, WsEvent>,
}

/// What a `Running` pose owns: the live loop's join fact, and the completion
/// broadcast the loop owns while it runs.
///
/// The completion sender is MOVED into the spawned task in production (the
/// channel's close is the completion signal, which is what makes a panic a
/// completion too). A loop that is abandoned instead of installed — a landing
/// refused by a concurrent close — is aborted HERE, so a refused move can
/// never leave a task running.
pub(crate) struct LiveLoop {
    pub(crate) handle: JoinHandle<()>,
    pub(crate) completion: Option<watch::Sender<bool>>,
}

impl LiveLoop {
    /// The refused-landing path: cancel the loop and release its completion
    /// sender so any `wait_pump_finished` waiter resolves.
    fn abandon(self) {
        self.handle.abort();
        drop(self.completion);
    }
}

/// The crawl as the session's teardown holds it.
///
/// The controller never spawns its own run (see `crawl::controller`, "Who
/// spawns the run"), so the `JoinHandle` belongs to whoever spawned it — the
/// boot wiring — and is handed over together with the narrowed [`CrawlHandle`].
/// The pair stays attached to the session so ONE teardown joins both loops
/// under one grace window.
pub(crate) struct CrawlTeardown {
    /// The narrowed teardown edge: latch the stop, read the two facts.
    pub(crate) handle: CrawlHandle,
    /// The run future's handle — the join fact's only reporter.
    pub(crate) run: JoinHandle<()>,
}

/// How a crawl's run resolved during teardown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrawlExit {
    /// The session never booted a crawl.
    None,
    /// The run resolved within the grace window (its own stop, or its own end).
    Resolved,
    /// The latch could not reach the run; it was aborted.
    Aborted,
    /// The run ended with a join error (a panic).
    JoinError,
}

/// How the live loop resolved during teardown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PumpExit {
    /// No live loop was armed: the session was torn down before it ran.
    NotArmed,
    /// The loop observed the shutdown flag on its own tick and returned.
    Cooperative,
    /// The grace expired and the loop was aborted.
    Aborted,
    /// The loop ended with a join error (a panic).
    JoinError,
}

/// The teardown's payload: everything `Stop` takes out of the session.
///
/// Close-consumed: the driver latches the crawl, joins both loops and drops the
/// completion sender, then reports what it tore down.
pub(crate) struct SessionTeardown {
    pub(crate) live: Option<LiveLoop>,
    pub(crate) crawl: Option<CrawlTeardown>,
    pub(crate) completion: Option<watch::Sender<bool>>,
}

impl SessionTeardown {
    /// Latch the crawl's stop — total, idempotent, and never blocking.
    ///
    /// With a live run this lands the crawl's `Stopping` and leaves the
    /// submission window draining: no submitted unit is cancelled and no
    /// receipt is truncated, because only the run future's own resolution
    /// reaches its `Stopped`. The caller still owes the bounded join — the pose
    /// is not the join fact.
    pub(crate) fn latch_crawl(&self) {
        if let Some(crawl) = &self.crawl {
            crawl.handle.stop();
        }
    }

    /// Join both loops under `grace`, escalating to `abort()`.
    ///
    /// The crawl was asked to stop BEFORE this call (see the driver's `close`):
    /// both loops then unwind at once, so the crawl drains its window while the
    /// pump walks its own tick. Abort is the escalation for a loop parked where
    /// the latch cannot reach it, never the primary mechanism.
    pub(crate) async fn join(&mut self, grace: std::time::Duration) -> (PumpExit, CrawlExit) {
        let pump = match self.live.take() {
            None => PumpExit::NotArmed,
            Some(live) => join_one(live.handle, live.completion, grace, "BlockPump").await,
        };
        let crawl = match self.crawl.take() {
            None => CrawlExit::None,
            Some(crawl) => match join_one(crawl.run, None, grace, "crawl run").await {
                PumpExit::NotArmed | PumpExit::Cooperative => CrawlExit::Resolved,
                PumpExit::Aborted => CrawlExit::Aborted,
                PumpExit::JoinError => CrawlExit::JoinError,
            },
        };
        // Any completion sender the session still held (a teardown before the
        // loop ever armed) drops with the payload: waiters resolve.
        drop(self.completion.take());
        (pump, crawl)
    }
}

/// Join ONE loop under the grace window, escalating to `abort()`.
async fn join_one(
    mut handle: JoinHandle<()>,
    completion: Option<watch::Sender<bool>>,
    grace: std::time::Duration,
    what: &'static str,
) -> PumpExit {
    let cooperative = tokio::time::timeout(grace, &mut handle).await;
    let exit = match cooperative {
        Ok(Ok(())) => PumpExit::Cooperative,
        Ok(Err(join_err)) => {
            op_error!(
                domain = pump,
                loop_name = what,
                error = %join_err,
                "pump session: a loop ended with a join error before the grace expired"
            );
            PumpExit::JoinError
        }
        Err(_) => {
            handle.abort();
            let _ = handle.await;
            op_info!(
                domain = pump,
                loop_name = what,
                "pump session: a loop did not resolve within grace — aborted (fallback)"
            );
            PumpExit::Aborted
        }
    };
    drop(completion);
    exit
}

/// The private, data-carrying pose. Each variant owns exactly what that pose
/// holds; the `Copy` projection ([`PumpPhase`]) is derived from it.
#[derive(Default)]
pub(crate) enum PumpPose {
    #[default]
    Detached,
    Connecting,
    Subscribed(Box<ConnectedSession>),
    Resuming,
    Running(Box<LiveLoop>),
    /// The live loop returned on its own. The pose OWNS the loop's join fact:
    /// the task is finished, but the join (and how it resolved) is still the
    /// teardown's to report.
    Ended(Box<LiveLoop>),
    Abandoned,
    Stopping,
    Stopped,
}

impl PumpPose {
    pub(crate) fn phase(&self) -> PumpPhase {
        match self {
            Self::Detached => PumpPhase::Detached,
            Self::Connecting => PumpPhase::Connecting,
            Self::Subscribed(_) => PumpPhase::Subscribed,
            Self::Resuming => PumpPhase::Resuming,
            Self::Running(_) => PumpPhase::Running,
            Self::Ended(_) => PumpPhase::Ended,
            Self::Abandoned => PumpPhase::Abandoned,
            Self::Stopping => PumpPhase::Stopping,
            Self::Stopped => PumpPhase::Stopped,
        }
    }
}

/// One payload in flight between a take-out and its landing.
#[derive(Default)]
pub(crate) enum MovePayload {
    /// Nothing to carry: the move only writes a pose.
    #[default]
    None,
    /// The connected session a `run` consumes.
    Connected(Box<ConnectedSession>),
    /// The teardown a `close` consumes.
    Teardown(Box<SessionTeardown>),
}

impl MovePayload {
    /// Drop a payload that could not be installed. A loop or crawl run is
    /// ABORTED, never detached: a refused landing must not leave a task running
    /// over a dead session.
    fn abandon(self) {
        match self {
            Self::None | Self::Connected(_) => {}
            Self::Teardown(teardown) => {
                teardown.latch_crawl();
                if let Some(live) = teardown.live {
                    live.abandon();
                }
                if let Some(crawl) = teardown.crawl {
                    crawl.run.abort();
                }
                drop(teardown.completion);
            }
        }
    }
}

/// The session's whole data: the pose, the crawl it owns, the completion
/// broadcast, the consumer claim, and the in-flight move count.
pub(crate) struct PumpSession {
    inner: Mutex<PumpState>,
}

struct PumpState {
    pose: PumpPose,
    crawl: Option<CrawlTeardown>,
    completion: Option<watch::Sender<bool>>,
    claimed: bool,
    in_flight: usize,
    wake: Arc<Notify>,
}

impl PumpSession {
    pub(crate) fn new(completion: watch::Sender<bool>) -> Self {
        Self {
            inner: Mutex::new(PumpState {
                pose: PumpPose::default(),
                crawl: None,
                completion: Some(completion),
                claimed: false,
                in_flight: 0,
                wake: Arc::new(Notify::new()),
            }),
        }
    }

    /// The `Copy` projection of the pose. Never a handle into the session.
    pub(crate) fn phase(&self) -> PumpPhase {
        self.inner.lock().pose.phase()
    }

    /// The crawl's narrowed teardown edge, when the session owns one.
    pub(crate) fn crawl_edge(&self) -> Option<CrawlHandle> {
        self.inner.lock().crawl.as_ref().map(|c| c.handle.clone())
    }

    /// Take the consumer claim — once per session.
    ///
    /// # Errors
    ///
    /// The caller's `ClaimRefusal::AlreadyClaimed` when a hand-off already
    /// happened; this door answers `false` so the refusal stays the driver's
    /// vocabulary.
    pub(crate) fn claim(&self) -> bool {
        let mut state = self.inner.lock();
        if state.claimed {
            return false;
        }
        state.claimed = true;
        true
    }

    /// Hand the completion broadcast to the live loop that is about to run.
    ///
    /// The loop OWNS the sender while it runs, so the channel's close is the
    /// completion signal — which is what makes a panic a completion too. A
    /// session torn down before it ever ran holds the sender until its teardown
    /// drops it, so a pre-run waiter still resolves.
    pub(crate) fn take_completion(&self) -> Option<watch::Sender<bool>> {
        self.inner.lock().completion.take()
    }

    /// Construction-time test seed: a session already in `Subscribed`, owning a
    /// local pump and its stream.
    #[cfg(test)]
    pub(crate) fn seed_connected_for_test(&self, pending: Box<ConnectedSession>) {
        self.inner.lock().pose = PumpPose::Subscribed(pending);
    }

    /// Construction-time test seed: a session already in `Running` over a
    /// stand-in loop.
    #[cfg(test)]
    pub(crate) fn seed_running_for_test(&self, live: Box<LiveLoop>) {
        self.inner.lock().pose = PumpPose::Running(live);
    }

    /// The pump task's ONE exit edge: `Running -> Ended`, refused when teardown
    /// already owns the landing.
    ///
    /// The one writer of a pose outside the move protocol, and it names exactly
    /// one transition — the fact a pump task produces that nobody above it can
    /// observe itself.
    pub(crate) fn pump_exited(&self) -> Result<PhaseAdvance, PumpDecline> {
        let mut state = self.inner.lock();
        let from = state.pose.phase();
        let to = transition(from, PumpEvent::PumpExited)?;
        let live = match std::mem::replace(&mut state.pose, PumpPose::Detached) {
            PumpPose::Running(live) => live,
            other => {
                state.pose = other;
                return Err(PumpDecline::EventOutOfOrder {
                    current: from,
                    event: PumpEvent::PumpExited,
                });
            }
        };
        state.pose = PumpPose::Ended(live);
        Ok(PhaseAdvance { from, to })
    }

    /// Decide legality and move this pose's payload OUT, under the lock.
    ///
    /// This is where every await begins: the lock is released before the move
    /// returns, so no guard crosses one. Only the moves a caller may START are
    /// take-able (`SubscribeRequested`, `ResumeRequested`, `CrawlAttached`,
    /// `Stop`); the landing events belong to [`SessionMove`]'s land methods,
    /// which re-check the pose.
    ///
    /// # Errors
    ///
    /// A [`PumpDecline`]: [`PumpDecline::MoveInFlight`] when another lifecycle
    /// move holds the session, and the table's own answer otherwise.
    pub(crate) fn take(self: &Arc<Self>, event: PumpEvent) -> Result<SessionMove, PumpDecline> {
        let mut state = self.inner.lock();
        let from = state.pose.phase();
        let counted = matches!(
            event,
            PumpEvent::SubscribeRequested | PumpEvent::ResumeRequested
        );
        if counted && state.in_flight > 0 {
            return Err(PumpDecline::MoveInFlight {
                current: from,
                event,
            });
        }
        let to = transition(from, event)?;
        let payload = match event {
            PumpEvent::SubscribeRequested => {
                state.pose = PumpPose::Connecting;
                MovePayload::None
            }
            PumpEvent::CrawlAttached => {
                // One crawl per session: refused HERE, at the take-out, so the
                // caller does no configuration or discovery work for an attach
                // that cannot land.
                if state.crawl.is_some() {
                    return Err(PumpDecline::CrawlRefused { current: from });
                }
                MovePayload::None
            }
            PumpEvent::ResumeRequested => {
                match std::mem::replace(&mut state.pose, PumpPose::Resuming) {
                    PumpPose::Subscribed(pending) => MovePayload::Connected(pending),
                    // The table admits `ResumeRequested` only from `Subscribed`,
                    // which always owns a pending session.
                    other => {
                        state.pose = other;
                        return Err(PumpDecline::ResumeRequiresSubscribed { current: from });
                    }
                }
            }
            PumpEvent::Stop => {
                let live = match std::mem::replace(&mut state.pose, PumpPose::Detached) {
                    PumpPose::Running(live) | PumpPose::Ended(live) => Some(*live),
                    other => {
                        state.pose = other;
                        None
                    }
                };
                state.pose = if to == PumpPhase::Stopping {
                    PumpPose::Stopping
                } else {
                    PumpPose::Stopped
                };
                MovePayload::Teardown(Box::new(SessionTeardown {
                    live,
                    crawl: state.crawl.take(),
                    completion: state.completion.take(),
                }))
            }
            // Every other event is a LANDING, not a move: a take-out that
            // wrote a pose with no payload would let a landing be skipped.
            _ => {
                return Err(PumpDecline::EventOutOfOrder {
                    current: from,
                    event,
                })
            }
        };
        if counted {
            state.in_flight += 1;
        }
        drop(state);
        Ok(SessionMove {
            session: Arc::clone(self),
            from,
            abandonment: abandonment_for(event),
            payload,
            counted,
            done: false,
        })
    }

    /// Wait until no lifecycle move is in flight.
    ///
    /// The close's "coordinate any in-flight move" step: the move's guard
    /// publishes its abandonment edge as it unwinds, so the wait resolves after
    /// the session has landed that edge.
    pub(crate) async fn wait_for_moves(&self) {
        loop {
            let wake = {
                let state = self.inner.lock();
                if state.in_flight == 0 {
                    return;
                }
                Arc::clone(&state.wake)
            };
            wake.notified().await;
        }
    }

    /// Publish `StopCompleted` — the teardown's last act.
    pub(crate) fn complete_close(&self) -> Result<PhaseAdvance, PumpDecline> {
        let mut state = self.inner.lock();
        let from = state.pose.phase();
        let to = transition(from, PumpEvent::StopCompleted)?;
        state.pose = PumpPose::Stopped;
        Ok(PhaseAdvance { from, to })
    }

    /// Write one payload-less landing, re-checking the pose under the lock.
    fn land_edge(&self, event: PumpEvent) -> Result<PhaseAdvance, PumpDecline> {
        let mut state = self.inner.lock();
        let from = state.pose.phase();
        let to = transition(from, event)?;
        let Some(pose) = plain_pose_for(event) else {
            return Err(PumpDecline::EventOutOfOrder {
                current: from,
                event,
            });
        };
        state.pose = pose;
        Ok(PhaseAdvance { from, to })
    }

    /// Install the connected session: the `Subscribed` pose now OWNS the pump.
    fn land_connected(&self, pending: Box<ConnectedSession>) -> Result<PhaseAdvance, PumpDecline> {
        let mut state = self.inner.lock();
        let from = state.pose.phase();
        let to = transition(from, PumpEvent::Subscribed)?;
        state.pose = PumpPose::Subscribed(pending);
        Ok(PhaseAdvance { from, to })
    }

    /// Install the spawned live loop: the `Running` pose now OWNS its join fact.
    ///
    /// A refused landing ABORTS the spawned loop — a move the table did not
    /// admit must never leave a pump running over a dead session (the §1.3(a)
    /// regression).
    fn land_live(&self, live: Box<LiveLoop>) -> Result<PhaseAdvance, PumpDecline> {
        let landed = {
            let mut state = self.inner.lock();
            let from = state.pose.phase();
            match transition(from, PumpEvent::ResumeSpawned) {
                Ok(to) => {
                    state.pose = PumpPose::Running(live);
                    Ok(PhaseAdvance { from, to })
                }
                Err(decline) => Err((decline, live)),
            }
        };
        match landed {
            Ok(advance) => Ok(advance),
            Err((decline, live)) => {
                live.abandon();
                Err(decline)
            }
        }
    }

    /// Install a crawl's teardown pair on a live session.
    ///
    /// A refused attach reports the table's decline and returns the pair, so the
    /// caller aborts the run it could not install.
    fn land_crawl(
        &self,
        crawl: Box<CrawlTeardown>,
    ) -> Result<PhaseAdvance, (PumpDecline, Box<CrawlTeardown>)> {
        let mut state = self.inner.lock();
        let from = state.pose.phase();
        let to = match transition(from, PumpEvent::CrawlAttached) {
            Ok(to) => to,
            Err(decline) => return Err((decline, crawl)),
        };
        if state.crawl.is_some() {
            return Err((PumpDecline::CrawlRefused { current: from }, crawl));
        }
        state.crawl = Some(*crawl);
        Ok(PhaseAdvance { from, to })
    }
}

/// The table's abandonment edge for a move, if it has one.
const fn abandonment_for(event: PumpEvent) -> Option<PumpEvent> {
    match event {
        PumpEvent::SubscribeRequested => Some(PumpEvent::SubscribeAbandoned),
        PumpEvent::ResumeRequested => Some(PumpEvent::ResumeAbandoned),
        _ => None,
    }
}

/// One session move in flight, guarded against the future being dropped.
///
/// `connect` and `run` publish their in-flight pose (`Connecting`, `Resuming`)
/// BEFORE their first await, so a reader always sees that WS work is
/// outstanding. The price of an in-flight pose is that a future dropped in that
/// window would park the session where no other entry point is legal — so the
/// guard carries the table's abandonment edge for that leg and publishes it from
/// `Drop`. The leg's own landing defuses the guard; that pairing is the
/// documented cancel policy: **an in-flight pose is always either landed or
/// abandoned, never left standing**, and a payload a refused landing could not
/// install is aborted rather than detached.
pub(crate) struct SessionMove {
    session: Arc<PumpSession>,
    from: PumpPhase,
    abandonment: Option<PumpEvent>,
    payload: MovePayload,
    counted: bool,
    done: bool,
}

impl SessionMove {
    /// The pose the move arrived in.
    pub(crate) fn from(&self) -> PumpPhase {
        self.from
    }

    /// Take the payload out for the leg's own use (the pump, the stream).
    pub(crate) fn take_payload(&mut self) -> MovePayload {
        std::mem::take(&mut self.payload)
    }

    /// Land the handshake: the session now OWNS the connected session.
    ///
    /// # Errors
    ///
    /// [`PumpDecline`] when the pose moved on while the handshake was in flight
    /// (a concurrent close). The payload is then abandoned, never installed.
    pub(crate) fn land_connected(
        mut self,
        pending: Box<ConnectedSession>,
    ) -> Result<PhaseAdvance, PumpDecline> {
        self.done = true;
        // A refused landing drops `pending` with the call: the pump and its WS
        // session die with it, exactly as they do with a cancelled connect.
        let landed = self.session.land_connected(pending);
        self.count_down();
        landed
    }

    /// Land the spawned live loop: the `Running` pose now owns its join fact.
    ///
    /// # Errors
    ///
    /// [`PumpDecline`] when a concurrent close landed the teardown first. The
    /// spawned loop is then ABORTED (never detached), so the landing can never
    /// leave a pump running over a dead session.
    pub(crate) fn land_live(mut self, live: Box<LiveLoop>) -> Result<PhaseAdvance, PumpDecline> {
        self.done = true;
        // The refused landing aborts the loop inside `PumpSession::land_live`, so
        // a landing the table no longer admits cannot leave it running.
        let landed = self.session.land_live(live);
        self.count_down();
        landed
    }

    /// Land a crawl attach: the session now owns the crawl's teardown pair.
    ///
    /// # Errors
    ///
    /// [`PumpDecline`] when the session is gone or tearing down; the crawl's
    /// run is then aborted, so a refused attach never leaves it running.
    pub(crate) fn land_crawl(
        mut self,
        crawl: Box<CrawlTeardown>,
    ) -> Result<PhaseAdvance, PumpDecline> {
        self.done = true;
        let landed = match self.session.land_crawl(crawl) {
            Ok(advance) => Ok(advance),
            Err((decline, refused)) => {
                // A crawl the table refused must not keep running over a dead
                // session: latch its stop and abort its run.
                refused.handle.stop();
                refused.run.abort();
                Err(decline)
            }
        };
        self.count_down();
        landed
    }

    /// Land a payload-less edge (`SubscribeFailed`, `SubscribeAbandoned`,
    /// `ResumeAbandoned`, `StopCompleted`).
    ///
    /// # Errors
    ///
    /// [`PumpDecline`] when the pose moved on; the pose is left where it is.
    pub(crate) fn land_empty(mut self, event: PumpEvent) -> Result<PhaseAdvance, PumpDecline> {
        self.done = true;
        let landed = self.session.land_edge(event);
        self.count_down();
        landed
    }

    /// Defuse the guard and release the in-flight count without landing.
    ///
    /// The `Stop` move uses this: its take-out already wrote the teardown pose,
    /// so there is nothing left to land — the payload is the close's.
    pub(crate) fn release(mut self) {
        self.done = true;
        self.count_down();
    }

    fn count_down(&mut self) {
        if !self.counted {
            return;
        }
        self.counted = false;
        let mut state = self.session.inner.lock();
        state.in_flight = state.in_flight.saturating_sub(1);
        let wake = Arc::clone(&state.wake);
        drop(state);
        wake.notify_waiters();
    }
}

/// The plain pose a payload-less landing writes, or `None` for an edge whose
/// landing must carry a payload (so a payload-less landing on it is refused
/// rather than silently installing a pose with nothing in it).
const fn plain_pose_for(event: PumpEvent) -> Option<PumpPose> {
    match event {
        PumpEvent::SubscribeFailed | PumpEvent::SubscribeAbandoned => Some(PumpPose::Detached),
        PumpEvent::ResumeAbandoned => Some(PumpPose::Abandoned),
        PumpEvent::Stop | PumpEvent::StopCompleted => Some(PumpPose::Stopped),
        // `Ended` owns the join fact, and the other payload-carrying edges
        // (`Subscribed`, `ResumeSpawned`, `CrawlAttached`) plus the two move
        // requests are never payload-less landings: a payload-less landing on
        // any of them is refused rather than installing an empty pose.
        PumpEvent::PumpExited
        | PumpEvent::Subscribed
        | PumpEvent::ResumeSpawned
        | PumpEvent::CrawlAttached
        | PumpEvent::SubscribeRequested
        | PumpEvent::ResumeRequested => None,
    }
}

impl Drop for SessionMove {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        // Dropped mid-flight (a cancelled future, or an early return): the
        // abandonment edge is the table's honest landing for a leg that never
        // committed. A decline means the session moved on its own (teardown
        // began), and there is nothing left to restore.
        if let Some(event) = self.abandonment {
            let _ = self.session.land_edge(event);
        }
        self.count_down();
        let payload = std::mem::take(&mut self.payload);
        payload.abandon();
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;
    use PumpEvent as E;
    use PumpPhase as P;

    const ALL_POSES: [PumpPhase; 9] = [
        P::Detached,
        P::Connecting,
        P::Subscribed,
        P::Resuming,
        P::Running,
        P::Ended,
        P::Abandoned,
        P::Stopping,
        P::Stopped,
    ];

    const ALL_EVENTS: [PumpEvent; 11] = [
        E::SubscribeRequested,
        E::Subscribed,
        E::SubscribeFailed,
        E::SubscribeAbandoned,
        E::ResumeRequested,
        E::ResumeSpawned,
        E::ResumeAbandoned,
        E::PumpExited,
        E::CrawlAttached,
        E::Stop,
        E::StopCompleted,
    ];

    fn session() -> Arc<PumpSession> {
        let (tx, _rx) = watch::channel(false);
        Arc::new(PumpSession::new(tx))
    }

    #[test]
    fn every_state_answers_every_event() {
        // Totality over the full 9x11 grid: no pair is undefined and no pair is
        // a silent no-op. The count pins the table's SHAPE, so an edge added or
        // removed in one place has to be acknowledged here.
        let mut allowed = 0;
        for pose in ALL_POSES {
            for event in ALL_EVENTS {
                if transition(pose, event).is_ok() {
                    allowed += 1;
                }
            }
        }
        // 9 poses admit `Stop`, plus the nine named edges of the graph, plus the
        // five poses a crawl attach is legal from.
        assert_eq!(allowed, 23, "the graph's allowed edge count changed");
    }

    #[test]
    fn subscribe_is_legal_only_from_detached() {
        for pose in ALL_POSES {
            let allowed = pose == P::Detached;
            assert_eq!(
                transition(pose, E::SubscribeRequested).ok(),
                allowed.then_some(P::Connecting),
                "subscribe from {pose}"
            );
        }
        assert_eq!(
            transition(P::Abandoned, E::SubscribeRequested),
            Err(PumpDecline::SubscribeAfterAbandonment)
        );
    }

    #[test]
    fn resume_is_legal_exactly_when_subscribed() {
        for pose in ALL_POSES {
            let verdict = transition(pose, E::ResumeRequested);
            let expected = match pose {
                P::Subscribed => Ok(P::Resuming),
                P::Abandoned => Err(PumpDecline::ResumeAfterAbandonment),
                P::Stopping | P::Stopped => Err(PumpDecline::Stopped {
                    current: pose,
                    event: E::ResumeRequested,
                }),
                _ => Err(PumpDecline::ResumeRequiresSubscribed { current: pose }),
            };
            assert_eq!(verdict, expected, "resume from {pose}");
        }
    }

    #[test]
    fn a_crawl_attaches_while_the_session_is_alive_and_never_after() {
        for pose in ALL_POSES {
            let verdict = transition(pose, E::CrawlAttached);
            let alive = matches!(
                pose,
                P::Detached | P::Connecting | P::Subscribed | P::Resuming | P::Running
            );
            if alive {
                assert_eq!(verdict, Ok(pose), "attach from {pose} keeps the pose");
            } else {
                assert!(
                    matches!(
                        verdict,
                        Err(PumpDecline::CrawlRefused { .. } | PumpDecline::Stopped { .. })
                    ),
                    "attach from {pose} was {verdict:?}"
                );
            }
        }
    }

    #[test]
    fn stop_is_total_and_never_declines() {
        for pose in ALL_POSES {
            let verdict = transition(pose, E::Stop).expect("stop never declines");
            assert!(
                matches!(verdict, P::Stopping | P::Stopped),
                "stop from {pose} landed on {verdict}"
            );
        }
    }

    #[test]
    fn stopped_is_absorbing() {
        for event in ALL_EVENTS {
            if event == E::Stop {
                assert_eq!(transition(P::Stopped, event), Ok(P::Stopped));
                continue;
            }
            assert_eq!(
                transition(P::Stopped, event),
                Err(PumpDecline::Stopped {
                    current: P::Stopped,
                    event
                }),
                "{event} against a stopped session"
            );
        }
    }

    #[test]
    fn a_live_loop_that_ends_on_its_own_reaches_ended() {
        assert_eq!(transition(P::Running, E::PumpExited), Ok(P::Ended));
        assert_eq!(transition(P::Ended, E::Stop), Ok(P::Stopped));
    }

    #[test]
    fn abandoned_is_reachable_only_by_a_cancelled_resume() {
        assert_eq!(
            transition(P::Resuming, E::ResumeAbandoned),
            Ok(P::Abandoned)
        );
        for pose in ALL_POSES {
            if pose == P::Resuming {
                continue;
            }
            for event in ALL_EVENTS {
                assert_ne!(
                    transition(pose, event),
                    Ok(P::Abandoned),
                    "({pose}, {event}) reached abandoned"
                );
            }
        }
    }

    #[test]
    fn abandoned_has_exactly_one_exit() {
        let exits: Vec<PumpPhase> = ALL_EVENTS
            .iter()
            .filter_map(|e| transition(P::Abandoned, *e).ok())
            .collect();
        assert_eq!(exits, vec![P::Stopped]);
        assert_eq!(
            transition(P::Abandoned, E::SubscribeRequested),
            Err(PumpDecline::SubscribeAfterAbandonment)
        );
        assert_eq!(
            transition(P::Abandoned, E::ResumeRequested),
            Err(PumpDecline::ResumeAfterAbandonment)
        );
    }

    #[test]
    fn a_cancelled_subscribe_restores_detached() {
        assert_eq!(
            transition(P::Connecting, E::SubscribeFailed),
            Ok(P::Detached)
        );
        assert_eq!(
            transition(P::Connecting, E::SubscribeAbandoned),
            Ok(P::Detached)
        );
    }

    #[test]
    fn a_cancelled_resume_is_not_restorable() {
        assert_eq!(
            transition(P::Resuming, E::ResumeAbandoned),
            Ok(P::Abandoned)
        );
        assert_eq!(
            transition(P::Abandoned, E::ResumeSpawned),
            Err(PumpDecline::EventOutOfOrder {
                current: P::Abandoned,
                event: E::ResumeSpawned
            })
        );
    }

    #[test]
    fn stop_decides_between_immediate_and_joined_teardown() {
        for pose in [P::Detached, P::Ended, P::Abandoned, P::Stopped] {
            assert_eq!(
                transition(pose, E::Stop),
                Ok(P::Stopped),
                "stop from {pose}"
            );
        }
        for pose in [P::Connecting, P::Subscribed, P::Resuming, P::Running] {
            assert_eq!(
                transition(pose, E::Stop),
                Ok(P::Stopping),
                "stop from {pose}"
            );
        }
        assert_eq!(transition(P::Stopping, E::Stop), Ok(P::Stopping));
        assert_eq!(transition(P::Stopping, E::StopCompleted), Ok(P::Stopped));
    }

    #[test]
    fn stop_completed_is_only_meaningful_while_stopping() {
        for pose in ALL_POSES {
            let verdict = transition(pose, E::StopCompleted);
            if pose == P::Stopping {
                assert_eq!(verdict, Ok(P::Stopped));
            } else {
                assert!(verdict.is_err(), "stop_completed from {pose} was allowed");
            }
        }
    }

    #[test]
    fn the_armed_projections_name_the_teardown_facts() {
        for pose in ALL_POSES {
            assert_eq!(
                pose.live_loop_armed(),
                matches!(pose, P::Resuming | P::Running),
                "live_loop_armed({pose})"
            );
            assert_eq!(
                pose.holds_subscription(),
                matches!(
                    pose,
                    P::Connecting | P::Subscribed | P::Resuming | P::Running
                ),
                "holds_subscription({pose})"
            );
            assert_eq!(pose.is_terminal(), matches!(pose, P::Stopping | P::Stopped));
        }
    }

    #[test]
    fn pose_and_event_names_are_stable() {
        // The FFI translates names rather than authoring legality, so the
        // strings are the boundary contract.
        assert_eq!(P::Detached.as_str(), "detached");
        assert_eq!(P::Abandoned.as_str(), "abandoned");
        assert_eq!(E::ResumeRequested.as_str(), "resume");
        assert_eq!(E::StopCompleted.as_str(), "stop_completed");
    }

    #[test]
    fn a_move_in_flight_refuses_a_second_and_the_guard_releases_it() {
        let session = session();
        let held = session
            .take(E::SubscribeRequested)
            .expect("detached admits subscribe");
        assert_eq!(session.phase(), P::Connecting);
        assert_eq!(
            session.take(E::SubscribeRequested).err(),
            Some(PumpDecline::MoveInFlight {
                current: P::Connecting,
                event: E::SubscribeRequested
            }),
            "a concurrent move is refused as in flight, not as a bad pose"
        );
        // Dropping the leg publishes its abandonment edge and releases the slot.
        drop(held);
        assert_eq!(session.phase(), P::Detached);
        assert!(session.take(E::SubscribeRequested).is_ok());
    }

    #[test]
    fn a_take_only_admits_the_moves_a_caller_may_begin() {
        let session = session();
        // Landing events are not take-able: a take-out that wrote a payload-less
        // pose would let a caller skip the landing.
        for event in [E::Subscribed, E::ResumeSpawned, E::StopCompleted] {
            assert!(
                session.take(event).is_err(),
                "{event} must not be take-able"
            );
        }
        assert_eq!(
            session.phase(),
            P::Detached,
            "a refused take writes nothing"
        );
    }

    #[test]
    fn the_claim_is_once_only() {
        let session = session();
        assert!(session.claim());
        assert!(!session.claim(), "a second hand-off is refused");
    }

    #[test]
    fn the_pump_exit_edge_lands_ended_and_is_refused_during_teardown() {
        let session = session();
        assert_eq!(
            session.pump_exited(),
            Err(PumpDecline::EventOutOfOrder {
                current: P::Detached,
                event: E::PumpExited
            })
        );
        let stop = session.take(E::Stop).expect("stop is total");
        stop.release();
        assert_eq!(session.phase(), P::Stopped);
        assert!(
            session.pump_exited().is_err(),
            "stopped refuses the exit edge"
        );
    }
}
