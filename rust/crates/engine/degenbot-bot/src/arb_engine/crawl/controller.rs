//! The crawl controller — the crawl's own machine, its loop, and its operator
//! surface.
//!
//! The crawl's benign registry-cap latch and its re-armable sweep-completion
//! latch are TWO state machines wearing one object; everything else on it is
//! either a fold (counters, stage profile) or a knob. Here the two latches are
//! one enum — [`CrawlState`] — because they are the same kind of
//! fact (a pose the pipeline is in, refusing work that pose cannot use), and
//! because a bool beside an enum cannot say which transitions are legal:
//!
//! - [`CrawlState::Idle`] — nothing has run.
//! - [`CrawlState::Running`] — the pipeline is live; `last_complete` is the
//!   sweep-completion latch (the [`Edition`] a NATURAL, uncapped sweep finished
//!   over). `None` means "no sweep has completed since the last re-arm".
//! - [`CrawlState::Capped`] — the registry cap latched. Terminal for discovery:
//!   the registry refuses every later registration, so a full sweep can only
//!   re-offer candidates the engine already answered.
//! - [`CrawlState::Stopping`] — a stop was latched and the run future has not
//!   resolved yet. It admits no work and moves no pose: the window drains, and
//!   nothing but the resolution edge leaves it.
//! - [`CrawlState::Stopped`] — terminal, absorbing, and loud: every later entry
//!   point refuses with a typed [`CrawlStateError`] instead of silently dropping
//!   the work (R6). Reachable only along the run future's resolution edge.
//!
//! [`transition`] is the whole legality table: one total function over
//! (state, event) that answers every pair. `Stop` never declines (teardown is
//! legal from every pose, mirroring the pump session's own table), and the
//! latch's only re-arm edge is [`CrawlEvent::EditionChanged`] — one transition,
//! no bool.
//!
//! # Two facts, two surfaces
//!
//! A stop latches [`CrawlState::Stopping`], never [`CrawlState::Stopped`]:
//! [`CrawlEvent::RunFinished`] is the one edge to the terminal pose, and only the
//! run future's resolution emits it. The reason is that the pose and the join are
//! different questions — a capped crawl is NOT finished (its run may still be
//! draining the window), and a stopping crawl is not finished until its run
//! resolves — so they are two machines, not one derived from the other:
//!
//! - the POSE ([`CrawlController::state`], a [`CrawlState`]) answers "what may
//!   enter?", and
//! - the FACTS ([`CrawlController::run_facts`], a [`RunFacts`]) answer "would a
//!   join block, and how did the run end?" — written only by the run's lease and
//!   its resolution guard, never by a caller.
//!
//! # Who spawns the run
//!
//! The controller does not spawn its own future. The DRIVER spawns
//! [`CrawlController::run`] on the runtime and owns the `JoinHandle`, because
//! that handle is the only thing that reports a panic — and a panicked crawl run
//! has to be classified by the session that owns it (the `JoinError` on its own
//! join, beside the pump-completion end facts). There is
//! deliberately no `take_join` and no `wait_finished`: a controller that spawned
//! nothing has no join to hand back, and an awaitable inside a `StopPlan` would
//! put a wait on `on_stop`, which is synchronous by contract.
//!
//! Teardown is therefore split: the session's `StopPlan` carries
//! `Option<CrawlHandle>` — never an `Arc<CrawlController>`, so a plan can latch a
//! stop and read the two facts and nothing else — while the bounded join belongs
//! to the driver's own `stop`, over the `JoinHandle` it kept, under its grace
//! window. `stop` never blocks and never awaits.
//!
//! # The loop
//!
//! [`CrawlController::run`] is the crawl: one discovery sweep feeds a windowed
//! submission paced by `intake_window` (the Python's `REG_INTAKE_WINDOW = 32`),
//! receipts resolve in FIFO submission order, and the single fold point
//! ([`CrawlSummary::absorb`]) runs on the loop, never on a seat. That is the
//! Python loop's shape with the crossings removed — the counters, the stage
//! profile and the cap latch are engine state now.
//!
//! A sweep ends as ONE [`SweepEnd`] — exhausted, truncated by the operator's
//! bound, or halted by a pose — and [`sweep_halt`] is the loop's only halt
//! predicate. Only a natural exhaustion latches the edition.
//!
//! The window PACES; it does not cap. The admission edge
//! (`Fold::inflight() >= window`) sits after the submit, and a receipt that
//! lands behind an older sibling waits in the buffer until the FIFO front
//! reaches it — so the loop's own gauge reads one past the window (measured)
//! with the admission discipline intact. The exact ceiling is the one read
//! where nothing can resolve, by
//! `a_parked_window_takes_on_exactly_the_window_and_names_every_submission`.
//!
//! Units ride the EXISTING fleet intake (PRG-5 preserved): a seat picks up an
//! [`InnerWork`](crate::arb_engine::fleet_intake::InnerWork) box and drives
//! C3's [`run_crawl_unit`] future with `bridged_block_on` — the same
//! `runtime.block_on` shape the Python unit has, minus the GIL attach. The seat
//! is a port ([`CrawlUnitSeat`]) so a test can run the same futures inline.
//!
//! # The receipt stream
//!
//! Receipts ride [`DeliveryChannel<CrawlReceipt>`]: `attach` is the only door,
//! `send` reports a
//! [`SendOutcome`](degenbot_core::channel_lifecycle::SendOutcome) — `NotAttached`
//! (a standalone consumer), `Closed` (the stream is over) and `ReceiverGone` (the
//! consumer vanished, which latches the channel `Closed`) stay three
//! distinguishable facts rather than one `bool` — and `close` ends the stream
//! exactly once.
//!
//! WHO closes: the run future's resolution, through its guard. A normal return, a
//! fatal `Err`, a panic unwind, and a dropped (cancelled) task all pass through
//! that one edge, so a panicking run can never leave the consumer waiting on a
//! stream nobody will end. `stop` closes it too, but only when no run future is
//! live — there is then nothing left that could ever end it. A live run's
//! receipts are never truncated by a stop: the window drains first, and a fatal
//! abort reaches the caller as the run's `Err`, not as a channel item, so a
//! closed stream can never swallow it.
//!
//! # Cancellation and the completion clause
//!
//! Nothing cancels a submitted unit: `stop` (and a fatal abort) stops *new*
//! submissions and the window drains, so every cloned handle a unit holds is
//! dropped before the run returns — the `close_snapshot_tx` canary's completion
//! clause, kept because the drain is the same edge in both languages.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::Address;
use degenbot_core::channel_lifecycle::{ChannelDecline, ChannelPhase, DeliveryChannel};
use degenbot_core::{diag, op_warn};
use degenbot_db::snapshot::TickMapDb;
use degenbot_pathfinding::PoolKind;
use degenbot_substrate::session_registry::TokenIdentity;
use parking_lot::Mutex;
use tokio::sync::mpsc;

use crate::arb_engine::crawl::admission::{AdmissionSettings, CrawlAbort, RegistrationAdmission};
use crate::arb_engine::crawl::discovery::{
    CrawlDiscoveryError, DbDiscoverySource, DiscoveredPath, DiscoveryConfig, DiscoverySource,
    DiscoverySweep, Edition,
};
use crate::arb_engine::crawl::policy::{BatchPolicyOracle, CrawlPathPolicy};
use crate::arb_engine::crawl::profile::CrawlSummary;
use crate::arb_engine::crawl::receipt::CrawlReceipt;
use crate::arb_engine::crawl::unit::{run_crawl_unit, CrawlUnit, CrawlUnitHost};
use crate::bot_core::registration_ledger::RegistrationUnitOutcome;
use crate::bot_core::verification_retry::RetryPolicy;
use crate::bot_core::ConstructionRoute;

// ── the state machine ───────────────────────────────────────────────────────

/// Where the crawl is. A `Copy` projection: readers see this, never the
/// controller's interior.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CrawlState {
    /// No run has happened and no sweep has completed.
    #[default]
    Idle,
    /// The pipeline is live. `last_complete` is the sweep-completion latch: the
    /// structural edition a natural, uncapped sweep finished over.
    Running {
        /// The edition the last NATURAL sweep completion latched, if any.
        ///
        /// Set only by [`CrawlEvent::SweepLatched`] (a bound-truncated or capped
        /// sweep never latches), cleared only by [`CrawlEvent::EditionChanged`].
        last_complete: Option<Edition>,
    },
    /// The registry cap latched; discovery is over.
    Capped,
    /// A stop was latched and the run future has not resolved yet: no new work
    /// enters, the window drains, and only [`CrawlEvent::RunFinished`] leaves.
    Stopping,
    /// Terminal: the crawl's run future resolved after a stop, so every entry
    /// point refuses.
    Stopped,
}

impl CrawlState {
    /// The lower-case name, for the FFI's name translator and error text.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running { .. } => "running",
            Self::Capped => "capped",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
        }
    }

    /// Whether the crawl is in its terminal pose.
    ///
    /// Not the join fact: [`CrawlState::Stopping`] is `false` here even though a
    /// stop is already latched, and [`CrawlController::is_finished`] is the
    /// question "would a join block?".
    #[must_use]
    pub const fn is_stopped(self) -> bool {
        matches!(self, Self::Stopped)
    }

    /// Whether a stop is latched and the run future is still draining.
    #[must_use]
    pub const fn is_stopping(self) -> bool {
        matches!(self, Self::Stopping)
    }

    /// Whether the crawl refuses new work: the stop is latched, whether or not
    /// the run future has resolved yet.
    #[must_use]
    pub const fn is_tearing_down(self) -> bool {
        matches!(self, Self::Stopping | Self::Stopped)
    }

    /// The sweep-completion latch, when the pose carries one.
    #[must_use]
    pub const fn last_complete(self) -> Option<Edition> {
        match self {
            Self::Running { last_complete } => last_complete,
            Self::Idle | Self::Capped | Self::Stopping | Self::Stopped => None,
        }
    }
}

impl std::fmt::Display for CrawlState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What happened to the crawl.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrawlEvent {
    /// A full crawl run starts. Marks the pipeline live; the latch is unchanged.
    RunRequested,
    /// A one-shot discovery sweep starts. No run lease, no latch change, and
    /// legal post-cap (the sweep's units then answer the cheap cap skip).
    SweepRequested,
    /// One operator-supplied path is admitted. Bypasses the latch (operator
    /// intent outranks discovery dedup) but not the controller.
    PathEnqueued,
    /// A sweep reached NATURAL completion over `edition` — the one edge that
    /// sets the latch.
    SweepLatched {
        /// The structural edition the sweep exhausted over.
        edition: Edition,
    },
    /// The structural edition changed: the one edge that re-arms the latch.
    EditionChanged,
    /// The registry cap latched.
    CapReached,
    /// Tear the crawl down. Latches [`CrawlState::Stopping`]: teardown is
    /// requested, the run future has not resolved yet.
    Stop,
    /// The run future resolved — however it resolved: a natural return, a fatal
    /// `Err`, a panic unwind, or a dropped (cancelled) run.
    ///
    /// The ONLY edge into [`CrawlState::Stopped`], and the only way out of
    /// [`CrawlState::Stopping`]. Emitted by the run's resolution guard, never by
    /// a caller.
    RunFinished,
}

impl CrawlEvent {
    /// The lower-case name, for error text.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RunRequested => "run",
            Self::SweepRequested => "sweep",
            Self::PathEnqueued => "enqueue",
            Self::SweepLatched { .. } => "sweep_latched",
            Self::EditionChanged => "edition_changed",
            Self::CapReached => "cap_reached",
            Self::Stop => "stop",
            Self::RunFinished => "run_finished",
        }
    }
}

impl std::fmt::Display for CrawlEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why the controller refused a move.
///
/// Every arm is a caller error: the state machine is total, so a decline means
/// the caller asked for something the current pose cannot use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CrawlStateError {
    /// The crawl is stopped; nothing is admitted after it.
    #[error("cannot apply {event}: the crawl is stopped")]
    Stopped {
        /// The terminal pose.
        current: CrawlState,
        /// The event that was refused.
        event: CrawlEvent,
    },
    /// The crawl is stopping: the stop is latched, the window is draining, and
    /// no new work is admitted while it does.
    #[error("cannot apply {event}: the crawl is stopping")]
    Stopping {
        /// The pose the event arrived in.
        current: CrawlState,
        /// The event that was refused.
        event: CrawlEvent,
    },
    /// The event does not apply in the current pose.
    #[error("cannot apply {event} from {current}")]
    EventOutOfOrder {
        /// The pose the event arrived in.
        current: CrawlState,
        /// The event that does not apply.
        event: CrawlEvent,
    },
    /// A sweep is already in flight on this controller. Raised by the sweep
    /// lease, not by [`transition`]: the lease is a concurrency guard, not a
    /// pose.
    #[error("a crawl sweep is already in flight on this controller")]
    SweepInFlight,
}

/// The whole legality table: (state, event) -> next state, or a typed decline.
///
/// Total over every pair — no combination is undefined and none is a silent
/// no-op except the deliberate ones: `EditionChanged` re-arms a *disabled* latch
/// to itself (`Idle`), a capped crawl does not re-arm (a full registry keeps
/// refusing), `PathEnqueued` moves no pose (an admission, not a transition), and
/// `Stop` is idempotent in the two torn-down poses. `Stop` is the only event that
/// never declines, because teardown is legal from every pose; it latches
/// [`CrawlState::Stopping`], and [`CrawlEvent::RunFinished`] alone finishes it.
///
/// # Errors
///
/// [`CrawlStateError::Stopped`] for any event against a stopped crawl and
/// [`CrawlStateError::Stopping`] for work offered to a stopping one;
/// [`CrawlStateError::EventOutOfOrder`] for a full run on a capped crawl (the cap
/// is terminal), for a latch without a live pipeline, and for a resolution with
/// no run.
// One arm per legal edge: two edges that happen to answer the same pose are
// still two facts about the machine, and a reviewer reads them one at a time.
#[expect(
    clippy::match_same_arms,
    reason = "the legality table keeps one arm per edge; identical bodies are distinct edges"
)]
pub fn transition(state: CrawlState, event: CrawlEvent) -> Result<CrawlState, CrawlStateError> {
    use CrawlEvent as E;
    use CrawlState as S;

    match (state, event) {
        // Teardown is total and idempotent: a live pose latches `Stopping` (the
        // run future finishes it), while the two torn-down poses answer
        // themselves so a second stop cannot re-open or regress them.
        (S::Stopping | S::Stopped, E::Stop) => Ok(state),
        (S::Idle | S::Running { .. } | S::Capped, E::Stop) => Ok(S::Stopping),

        // The resolution edge — the run future left, however it left. It is the
        // only passage into `Stopped`, it answers itself when it arrives twice,
        // and a natural end keeps the pose (and the latch) the run had: the cap
        // never implies finished, and finished never un-latches a cap.
        (S::Stopping | S::Stopped, E::RunFinished) => Ok(S::Stopped),
        (S::Running { last_complete }, E::RunFinished) => Ok(S::Running { last_complete }),
        (S::Capped, E::RunFinished) => Ok(S::Capped),
        // A resolution with no run ever started: the caller's error, and the one
        // pose that cannot receive it.
        (S::Idle, E::RunFinished) => Err(CrawlStateError::EventOutOfOrder {
            current: state,
            event,
        }),

        // `Stopping` admits no work and no other passage. The cap is the one
        // exception: a draining unit's own skip is a fold, not an admission, and
        // the stop outranks the pose it would have latched.
        (S::Stopping, E::CapReached) => Ok(S::Stopping),
        (S::Stopping, _) => Err(CrawlStateError::Stopping {
            current: state,
            event,
        }),

        // `Stopped` is absorbing: it refuses everything else, loudly and by
        // name.
        (S::Stopped, _) => Err(CrawlStateError::Stopped {
            current: state,
            event,
        }),

        // A run marks the pipeline live. The latch it finds is whatever the last
        // natural completion left; only `EditionChanged` clears it.
        (S::Idle | S::Running { .. }, E::RunRequested) => Ok(S::Running {
            last_complete: state.last_complete(),
        }),
        // The cap is terminal for discovery: a full crawl after it would spend
        // the whole sweep to collect cap skips.
        (S::Capped, E::RunRequested) => Err(CrawlStateError::EventOutOfOrder {
            current: state,
            event,
        }),

        // A one-shot sweep needs the pipeline LIVE, because a natural completion
        // of it latches the edition: from `Idle` the sweep is itself the edge
        // into `Running`, and a sweep that can never latch is the bug this arm
        // owns. Post-cap it is admitted and moves nothing — the operator's own
        // one-shot folds the cheap cap skips.
        (S::Idle, E::SweepRequested) => Ok(S::Running {
            last_complete: None,
        }),
        (S::Running { .. } | S::Capped, E::SweepRequested) => Ok(state),

        // An operator path is an admission, not a transition: it moves no pose,
        // and it never re-arms a latch (operator intent outranks discovery dedup).
        (S::Idle | S::Running { .. } | S::Capped, E::PathEnqueued) => Ok(state),

        // The latch sets only under a live pipeline, and only from a natural
        // completion (the caller decides "natural"; this table owns legality).
        (S::Running { .. }, E::SweepLatched { edition }) => Ok(S::Running {
            last_complete: Some(edition),
        }),
        (S::Idle | S::Capped, E::SweepLatched { .. }) => Err(CrawlStateError::EventOutOfOrder {
            current: state,
            event,
        }),

        // The one re-arm edge. A disabled latch has nothing to re-arm, so `Idle`
        // answers itself; a capped crawl never re-arms (see `CapReached`).
        (S::Running { .. }, E::EditionChanged) => Ok(S::Running {
            last_complete: None,
        }),
        (S::Idle | S::Capped, E::EditionChanged) => Ok(state),

        // The cap latch: idempotent, and reachable from any pose the crawl can
        // still be working from.
        (S::Idle | S::Running { .. } | S::Capped, E::CapReached) => Ok(S::Capped),
    }
}

// ── the sweep's own machine ─────────────────────────────────────────────────

/// How a sweep ENDED — the loop's one termination state.
///
/// The three ends are exclusive by construction: a sweep that spends its bound
/// never asks the source for the next candidate, and a halted one stops before it
/// pulls. That exclusivity is the completion rule's whole content — only
/// [`SweepEnd::Exhausted`] saw the graph's end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SweepEnd {
    /// The source yielded nothing further: the sweep covered the whole graph.
    Exhausted,
    /// The operator's bound stopped it with candidates still unread.
    Truncated,
    /// A pose forbade the next batch; [`SweepHalt`] names which.
    Halted(SweepHalt),
}

impl SweepEnd {
    /// Whether this end latches the sweep-completion edition.
    ///
    /// Only a natural, uncapped exhaustion covered the structure a later sweep
    /// would read, so only it says the next sweep over the same edition could
    /// re-offer nothing but candidates this pipeline already answered. A
    /// truncated sweep never read the rest of the graph, and a halted one was cut
    /// short by a pose: both leave the latch armed.
    #[must_use]
    pub const fn latches(self) -> bool {
        matches!(self, Self::Exhausted)
    }
}

/// What halted a sweep before its next batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SweepHalt {
    /// The registry cap latched *during* this sweep; discovery is over.
    CapOnset,
    /// A stop is latched: no new unit enters, and the window drains.
    Stop,
    /// A submitted unit aborted loudly; the window still drains, and the run
    /// answers with the abort.
    Abort,
}

/// The ONE halt predicate: whether the sweep must stop before its next batch, and
/// why.
///
/// `started` is the pose the sweep began from and `current` the pose now, because
/// the cap's TIMING is the rule: one already latched when the sweep began is the
/// operator's own one-shot over a full registry, whose units answer the cheap cap
/// skip, so the sweep runs to its end. A cap that latches *during* the sweep ends
/// it.
///
/// The other two halts are unconditional. A stop ends submission whether or not
/// the run future has resolved yet (the window drains either way), and a fatal
/// abort outranks every pose: the drain completes and the run returns the abort.
#[must_use]
pub fn sweep_halt(started: CrawlState, current: CrawlState, aborted: bool) -> Option<SweepHalt> {
    if aborted {
        return Some(SweepHalt::Abort);
    }
    match current {
        CrawlState::Stopping | CrawlState::Stopped => Some(SweepHalt::Stop),
        CrawlState::Capped if !matches!(started, CrawlState::Capped) => Some(SweepHalt::CapOnset),
        CrawlState::Idle | CrawlState::Running { .. } | CrawlState::Capped => None,
    }
}

/// One discovery turn: the candidates a pull produced, or the end it reached.
enum Turn {
    /// The turn's first candidate and the rest of it (`0..discovery_batch_size`
    /// more). The first is carried apart so the loop always has a unit to submit
    /// once a pull succeeded.
    Pulled {
        /// The candidate to submit now.
        first: CrawlUnit,
        /// The turn's remaining candidates, in sweep order.
        rest: Vec<CrawlUnit>,
    },
    /// The operator's bound is spent.
    Truncated,
    /// The source yielded nothing further.
    Exhausted,
}

// ── the configuration ───────────────────────────────────────────────────────

/// The resolved crawl configuration — a VALUE, supplied by the driver shell.
///
/// Everything the crawl needs to be configured once and then run: the discovery
/// scope, the unit policy, and the two intake bounds (the submission window,
/// which paces, and the path cap). The shell resolves the
/// route/policy/permutation filter; the crawl invents none of them.
#[derive(Clone, Debug)]
pub struct CrawlConfig {
    /// The chain every read and construction is scoped to.
    pub chain_id: u64,
    /// The SQLite file the discovery reads and the edition probe open.
    pub database_path: PathBuf,
    /// The resolved construction route (factory rungs + the generic builder).
    pub route: ConstructionRoute,
    /// The pool families the discovery graph includes.
    pub pool_kinds: Vec<PoolKind>,
    /// The per-depth family permutation filter (`None` = unconstrained).
    pub pool_type_per_depth: Option<Vec<Option<Vec<PoolKind>>>>,
    /// Path-start boundary tokens: the conceptual `NativeToken` and any
    /// ERC-20 anchors (WETH), never a raw protocol sentinel (ADR-097 D1).
    pub start_tokens: Vec<TokenIdentity>,
    /// Path-end boundary tokens, spelled like `start_tokens`.
    pub end_tokens: Vec<TokenIdentity>,
    /// The minimum hop count, before any per-depth filter floor.
    pub min_depth: usize,
    /// The maximum hop count (`None` = unbounded), validated against the
    /// pathfinding core's hop domain by [`Self::discovery`].
    pub max_depth: Option<usize>,
    /// How many candidates the loop pulls from the sweep per turn before it
    /// reaps the window.
    pub discovery_batch_size: usize,
    /// How many units may be outstanding before the loop blocks on the oldest
    /// (the Python's `REG_INTAKE_WINDOW`).
    pub intake_window: usize,
    /// The engine registry's path cap (`None` = unbounded).
    pub path_cap: Option<usize>,
    /// The verify retry policy — a value, never a crawl-invented loop.
    pub retry: RetryPolicy,
    /// The path-composition policy as data.
    pub policy: CrawlPathPolicy,
    /// A foreign policy the core cannot evaluate as data, consulted in place of
    /// [`CrawlPathPolicy::evaluate`]. A policy that IS expressible as data
    /// supplies [`CrawlPathPolicy`] and leaves this `None` (production's shape:
    /// the no-op policy is the default value).
    pub oracle: Option<Arc<dyn BatchPolicyOracle>>,
    /// How many candidates one oracle consult covers. The v1 crawl's
    /// eligibility stage consults a candidate at a time (a composition view
    /// exists only once the hops are built), so this bounds the batch and keeps
    /// a batch from outgrowing the submission window.
    pub policy_batch_size: usize,
    /// The cycle's profit token: the direction resolver's start and end.
    pub profit_token: Address,
    /// The verify lifecycle's snapshot block (`None` = the lifecycle's own).
    pub snapshot_block: Option<u64>,
    /// An explicit construction block (`None` = the chain head at build time).
    pub state_block: Option<u64>,
}

impl CrawlConfig {
    /// The discovery side of the configuration, as one value.
    ///
    /// # Errors
    ///
    /// [`CrawlConfigError::ChainIdOutOfRange`] when `chain_id` does not fit the
    /// signed id the database reads take, and
    /// [`CrawlConfigError::MaxDepthOutOfRange`] when `max_depth` is wider than
    /// the pathfinding core's hop domain.
    pub fn discovery(&self) -> Result<DiscoveryConfig, CrawlConfigError> {
        Ok(DiscoveryConfig {
            chain_id: chain_id_i64(self.chain_id)?,
            pool_kinds: self.pool_kinds.clone(),
            pool_type_per_depth: self.pool_type_per_depth.clone(),
            start_tokens: self.start_tokens.clone(),
            end_tokens: self.end_tokens.clone(),
            min_depth: self.min_depth,
            max_depth: self
                .max_depth
                .map(|max_depth| {
                    u32::try_from(max_depth)
                        .map_err(|_| CrawlConfigError::MaxDepthOutOfRange { max_depth })
                })
                .transpose()?,
        })
    }

    /// The admission-wide values every seat reads.
    ///
    /// The verify retry policy is NOT one of them: it is the verify home's
    /// construction value, applied once when the crawl is attached.
    pub(crate) fn admission_settings(&self) -> AdmissionSettings {
        AdmissionSettings {
            route: self.route.clone(),
            policy: self.policy.clone(),
            oracle: self.oracle.clone(),
            profit_token: self.profit_token,
            snapshot_block: self.snapshot_block,
            state_block: self.state_block,
        }
    }

    /// Refuse a configuration the crawl cannot honour.
    ///
    /// # Errors
    ///
    /// [`CrawlConfigError`] — an empty boundary set or pool-family list, a zero
    /// window/batch, or a chain id the database reads cannot take.
    fn validate(&self) -> Result<(), CrawlConfigError> {
        if self.start_tokens.is_empty() {
            return Err(CrawlConfigError::EmptyBoundary { which: "start" });
        }
        if self.end_tokens.is_empty() {
            return Err(CrawlConfigError::EmptyBoundary { which: "end" });
        }
        if self.pool_kinds.is_empty() {
            return Err(CrawlConfigError::EmptyPoolKinds);
        }
        if self.intake_window == 0 {
            return Err(CrawlConfigError::ZeroIntakeWindow);
        }
        if self.discovery_batch_size == 0 {
            return Err(CrawlConfigError::ZeroDiscoveryBatch);
        }
        if self.policy_batch_size == 0 {
            return Err(CrawlConfigError::ZeroPolicyBatch);
        }
        // A policy batch wider than the submission window could never be
        // filled: the window is what paces outstanding units.
        if self.policy_batch_size > self.intake_window {
            return Err(CrawlConfigError::PolicyBatchExceedsWindow {
                batch: self.policy_batch_size,
                window: self.intake_window,
            });
        }
        let _ = chain_id_i64(self.chain_id)?;
        Ok(())
    }
}

/// Why a crawl could not be configured.
#[derive(Debug, thiserror::Error)]
pub enum CrawlConfigError {
    /// A boundary token set is empty: no traversal exists.
    #[error("the crawl's {which} token set is empty")]
    EmptyBoundary {
        /// Which boundary (`start` / `end`).
        which: &'static str,
    },
    /// No pool family was requested, so the graph would be empty.
    #[error("the crawl's pool-family list is empty")]
    EmptyPoolKinds,
    /// A zero window would deadlock the first submit.
    #[error("the crawl's intake window must be at least 1")]
    ZeroIntakeWindow,
    /// A zero batch never pulls a candidate.
    #[error("the crawl's discovery batch size must be at least 1")]
    ZeroDiscoveryBatch,
    /// A zero policy batch could never consult an oracle.
    #[error("the crawl's policy batch size must be at least 1")]
    ZeroPolicyBatch,
    /// A policy batch wider than the submission window could never fill.
    #[error(
        "the crawl's policy batch size ({batch}) must not exceed its intake window ({window})"
    )]
    PolicyBatchExceedsWindow {
        /// The configured policy batch size.
        batch: usize,
        /// The configured intake window.
        window: usize,
    },
    /// The driver's `Bot` carries no construction I/O, so no crawl can be
    /// booted over it (the Python path attaches one at `Bot.__init__`).
    #[error("the engine has no ConstructionIo attached, so no crawl can be booted")]
    MissingConstructionIo,
    /// A crawl is already installed on this driver: one boot per driver.
    #[error("a crawl is already installed on this engine driver")]
    CrawlAlreadyBegun,
    /// The receipt stream's sink could not be attached (the channel is already
    /// attached or already closed).
    #[error(transparent)]
    Receipts(#[from] ChannelDecline),
    /// The chain id does not fit the database's signed id column.
    #[error("chain id {chain_id} is out of range for the database reads")]
    ChainIdOutOfRange {
        /// The rejected id.
        chain_id: u64,
    },
    /// The hop bound is wider than the pathfinding core's hop domain, so no
    /// search could honour it.
    #[error("the crawl's max_depth ({max_depth}) is above the pathfinding hop ceiling")]
    MaxDepthOutOfRange {
        /// The rejected bound.
        max_depth: usize,
    },
    /// The discovery source could not be opened.
    #[error(transparent)]
    Discovery(#[from] CrawlDiscoveryError),
}

/// The database's signed chain id, from the configuration's unsigned one.
fn chain_id_i64(chain_id: u64) -> Result<i64, CrawlConfigError> {
    i64::try_from(chain_id).map_err(|_| CrawlConfigError::ChainIdOutOfRange { chain_id })
}

// ── the snapshot arm ────────────────────────────────────────────────────────

/// The tick-map arm: a handle SOURCE, asked once per unit.
///
/// A head-stamped V3/V4 construction seeds its tick map from ONE long-lived
/// snapshot handle — the process holds it, arms it at the snapshot load, and
/// commits the read transaction when registration finishes. The crawl is a
/// second caller of that handle, never a second owner: it asks this arm for a
/// handle at the moment a unit runs and drops it when the unit resolves, exactly
/// as the retired Python pipeline asked `PyBot::db_handle()` per use. A crawl
/// that instead held its own clone for the run's life would pin the read
/// transaction past the close (the close's own canary names that clone), and it
/// could never observe the close: every unit would keep reading a stale DB view.
///
/// With a source in place the close is the arm's own edge — once the owner
/// withdraws the handle, every later ask answers `None` and construction takes
/// the no-DB arm (chain reads).
pub trait SnapshotArm: Send + Sync {
    /// The handle for one use, or `None` when the arm is closed.
    fn acquire(&self) -> Option<Arc<dyn TickMapDb>>;
}

/// A fixed handle: the standalone consumer's arm. Every ask clones it, so the
/// consumer's own handle stays the one it owns.
impl SnapshotArm for Option<Arc<dyn TickMapDb>> {
    fn acquire(&self) -> Option<Arc<dyn TickMapDb>> {
        self.clone()
    }
}

// ── the host ────────────────────────────────────────────────────────────────

/// Everything the controller drives, held as shared handles so a unit's future
/// can own its clones.
///
/// The driver owns the registry and the verify claim table, the bot owns
/// construction state, the I/O holds the RPC/DB construction seams, and the
/// snapshot arm is where a head construction asks for the tick-map handle it
/// reads.
pub struct CrawlHost {
    /// Registration admission — the one implementation of the stage order, and
    /// with it the construction state, the I/O handle, the engine stage seam,
    /// the verify home, the ledger, the build flights, and the policy values.
    pub admission: Arc<RegistrationAdmission>,
    /// The tick-map arm (`None` = construction reads no tick maps).
    pub snapshot: Option<Arc<dyn SnapshotArm>>,
}

impl CrawlHost {
    /// One unit's tick-map handle: a fresh ask of the arm, owned by that unit's
    /// future alone.
    fn acquire_db(&self) -> Option<Arc<dyn TickMapDb>> {
        self.snapshot.as_ref().and_then(|arm| arm.acquire())
    }
}

/// The reused host handles, moved into one unit's future.
struct UnitHost {
    admission: Arc<RegistrationAdmission>,
    db: Option<Arc<dyn TickMapDb>>,
}

impl UnitHost {
    /// Borrow the crawl unit's host view for one `run_crawl_unit` call.
    fn borrow(&self) -> CrawlUnitHost<'_> {
        CrawlUnitHost {
            admission: &self.admission,
            db: self.db.as_deref(),
        }
    }
}

// ── the seat ────────────────────────────────────────────────────────────────

/// One unit's future, ready to run: it resolves the unit and delivers the
/// outcome through its own channel, so the seat needs no result path.
pub type CrawlUnitFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>>;

/// Where a unit's future runs.
///
/// The production seat is the fleet intake ([`FleetSeat`]): the box IS the
/// pooled `PoolStateUpdater` unit, and the seat thread drives the future
/// through `bridged_block_on` (the Python unit's `runtime.block_on` shape, minus
/// the GIL attach). The port exists so a test — or a standalone Rust consumer
/// with no fleet — can run the same futures inline.
pub trait CrawlUnitSeat: Send + Sync {
    /// Run one unit's future to completion.
    ///
    /// # Errors
    ///
    /// The seat's own refusal to accept work (a fleet boot refusal), rendered
    /// into the crawl's error vocabulary by the controller.
    fn dispatch(&self, future: CrawlUnitFuture) -> Result<(), String>;

    /// Whether the seat is HOLDING submitted work right now, and why.
    ///
    /// The default — no hold — is the inline seat's answer: whatever it accepted
    /// is running or done. The fleet seat answers the process posture, whose
    /// cordon queues `PoolStateUpdater` units in the intake backlog without
    /// running them; they are never dropped, and the cordon's clean-window
    /// hysteresis releases them together.
    ///
    /// The window needs the distinction: a held unit is not a parked one, so a
    /// self-healing hold must not be charged to the park deadline. See
    /// [`CrawlController::await_unit`].
    fn hold(&self) -> Option<SeatHold> {
        None
    }
}

/// Why a seat is holding submitted work instead of running it.
///
/// A hold is not silence and not a loss: the units are queued AT the seat and
/// run when the hold's own recovery edge arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeatHold {
    /// The fleet posture cordoned the seat's role. `PoolStateUpdater` is
    /// `CordonClass::Deferrable`, so a `Cordoned` posture holds its intake in the
    /// unbounded host backlog (never dropped, never refused) until the
    /// clean-window hysteresis lifts the cordon.
    Posture,
}

impl std::fmt::Display for SeatHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Posture => f.write_str("the cordoned fleet posture"),
        }
    }
}

/// The production seat: the process's pooled registration intake.
///
/// No new pool, no new queue: the unit body is the existing
/// `FleetIntake::spawn` box (PRG-5 preserved), and the fleet's own bounded queue
/// + duty-counted seats remain the concurrency.
pub struct FleetSeat;

impl CrawlUnitSeat for FleetSeat {
    fn dispatch(&self, future: CrawlUnitFuture) -> Result<(), String> {
        let intake = crate::arb_engine::fleet_intake::registration_intake()
            .map_err(|error| error.to_string())?;
        intake.spawn(Box::new(move || {
            // A seat is a plain pooled thread, so the bridge takes the ambient
            // runtime directly (no scheduler duty to hand off).
            degenbot_core::runtime::bridged_block_on(future);
        }));
        Ok(())
    }

    /// The one hold a fleet intake can report: the shared posture owner has the
    /// seat's role cordoned, so admission spills every unit into the host's
    /// unbounded backlog and runs none of them until the cordon lifts.
    ///
    /// The LANE-DEATH hold is deliberately not reported: it is sticky (only a
    /// fresh process lifts it), and the faulted host resolves the work it held
    /// terminally instead of running it — that is the lost-unit arm's business,
    /// not a hold the window may wait on.
    fn hold(&self) -> Option<SeatHold> {
        let posture = degenbot_workers::posture::process_owner();
        if posture.lane_death_held() {
            return None;
        }
        let role = degenbot_workers::role::WorkerRole::PoolStateUpdater;
        (!posture.admits_lease(role.cordon_class())).then_some(SeatHold::Posture)
    }
}

/// Where a seat's outcome lands: the unit's sequence plus its receipt or its
/// loud abort.
type UnitOutcome = (u64, Result<CrawlReceipt, CrawlAbort>);

/// The unit outcome channel's sender.
type UnitSink = mpsc::UnboundedSender<UnitOutcome>;

// ── the request errors ──────────────────────────────────────────────────────

/// Why a controller call could not complete.
///
/// The state refusals keep their own vocabulary ([`CrawlStateError`], R6's typed
/// refusal) and are wrapped here so one error type covers every entry point.
#[derive(Debug, thiserror::Error)]
pub enum CrawlRequestError {
    /// The request is not legal in the current pose.
    #[error(transparent)]
    State(#[from] CrawlStateError),
    /// A discovery read or hop resolution failed.
    #[error(transparent)]
    Discovery(#[from] CrawlDiscoveryError),
    /// The configuration's discovery view could not be built.
    #[error(transparent)]
    Config(#[from] CrawlConfigError),
    /// The discovery preparation task could not run.
    #[error("the discovery preparation task failed: {detail}")]
    DiscoveryTask {
        /// The join failure, rendered.
        detail: String,
    },
    /// The seat refused the unit (a fleet boot refusal, rendered).
    #[error("the crawl's unit seat refused the work: {detail}")]
    Seat {
        /// The seat's refusal, rendered.
        detail: String,
    },
    /// A run future is already live on this controller. One run at a time: the
    /// window is per-run, so two would each hold a partial view of the FIFO
    /// order, and the join fact would have two writers.
    #[error("a crawl run is already in flight on this controller")]
    RunInFlight,
    /// The unit resolved without registering a path (skip, reject, or cap).
    #[error("crawl unit {sequence} completed without path registration: {outcome}")]
    RegistrationNotCompleted {
        /// The operator unit's submission sequence.
        sequence: u64,
        /// The terminal crawl outcome and its refusal tag, when present.
        outcome: String,
    },
    /// A unit's loud-abort arm fired: the crawl stops and the caller sees it.
    #[error(transparent)]
    Abort(CrawlAbort),
    /// A submitted unit never delivered its outcome (its future was dropped
    /// without sending: a seat died mid-unit). At-most-once still holds — the
    /// unit ran zero times or its outcome is unknown — so the crawl refuses to
    /// pretend it folded.
    #[error("{outstanding} submitted crawl units never delivered an outcome")]
    UnitLost {
        /// How many submitted units were still outstanding.
        outstanding: usize,
    },
    /// Submitted units delivered NOTHING for the window's idle deadline: they
    /// are parked, not merely slow (a seat thread stuck inside a unit, a build
    /// flight's peer whose leader vanished, a chain read that never answers).
    /// The run resolves and names them rather than parking its consumer forever.
    #[error(
        "the crawl's window stalled: no receipt for {waited_ms}ms with {} submitted unit(s) outstanding (seqs {stalled:?})",
        stalled.len()
    )]
    UnitStalled {
        /// The submission sequences still outstanding, oldest first.
        stalled: Vec<u64>,
        /// How long the window waited in silence.
        waited_ms: u64,
    },
    /// The seat reported a HOLD for the whole hold budget
    /// ([`HELD_WINDOW_BUDGET`]): a cordoned fleet posture queues this role's
    /// units in the intake backlog and runs them when the clean-window
    /// hysteresis lifts the cordon, and this window outlasted every bound the
    /// run will wait.
    ///
    /// Named apart from [`Self::UnitStalled`] because the cause — and the
    /// operator's lever — is the posture, not a parked seat: nothing is stuck,
    /// the cordon simply never lifted.
    #[error(
        "the crawl's window is held by {hold} for {held_ms}ms with {} submitted unit(s) outstanding (seqs {stalled:?})",
        stalled.len()
    )]
    IntakeHeld {
        /// The submission sequences still outstanding, oldest first.
        stalled: Vec<u64>,
        /// How long the hold outlasted the run's budget.
        held_ms: u64,
        /// The seat's own hold reason.
        hold: SeatHold,
    },
}

// ── the run's own machine ───────────────────────────────────────────────────

/// The RUN's facts — the join, the answer, and the reason an answer is missing,
/// in ONE value.
///
/// Deliberately not derived from [`CrawlState`]: a capped crawl is not finished
/// (its run may still be draining the window), and a stop only makes a crawl
/// finishable. The arms split what a phase beside an optional outcome could not
/// say:
///
/// - [`Self::NotStarted`] — the run lease is free and nothing has ended: a run
///   may still arm, so a wait for one must not read this as an end.
/// - [`Self::Live`] — a run future holds the lease: a join would block, and a
///   second run is refused.
/// - [`Self::Resolved`] — the run RETURNED and recorded how it ended.
/// - [`Self::EndedWithoutRun`] — a stop ended the lifecycle with no run to
///   resolve.
/// - [`Self::EndedWithoutOutcome`] — the run future ended without recording: a
///   panic unwind or a dropped (cancelled) task.
///
/// The phase-plus-outcome pair read all three ends as "finished, no outcome", so
/// a consumer woken by end-of-stream could not tell a stopped crawl from a lost
/// run. Here each end is its own arm and [`Self::has_ended`] is the
/// join-and-stream question.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RunFacts {
    /// No run has been spawned and none has ended: the run lease is free.
    #[default]
    NotStarted,
    /// A run future is live on this controller.
    Live,
    /// The last run future resolved, and this is how.
    Resolved(RunOutcome),
    /// A stop ended the run lifecycle with no run to resolve: nothing was live,
    /// so nothing could ever answer.
    EndedWithoutRun,
    /// The run future ended without recording an outcome.
    EndedWithoutOutcome(OutcomeMissing),
}

/// Why a run future ended without recording an outcome.
///
/// Both arms are the resolution guard's reading of its own drop — never a
/// caller's claim: a run has no other way to end unanswered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutcomeMissing {
    /// The run future unwound: a panic inside the loop. The driver's
    /// `JoinHandle` carries the `JoinError` that names it.
    Panicked,
    /// The run future was dropped mid-flight: its task was aborted, or the
    /// runtime shut down under it. Nothing was returned, so nothing is claimed.
    Cancelled,
}

impl OutcomeMissing {
    /// The lower-case name, for logs and error text.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Panicked => "panicked",
            Self::Cancelled => "cancelled",
        }
    }
}

impl RunFacts {
    /// Whether a run future is live: the one pose a join would block in.
    #[must_use]
    pub const fn is_live(&self) -> bool {
        matches!(self, Self::Live)
    }

    /// Whether the run lifecycle has ENDED: no run is live, and none can resolve
    /// without a new lease.
    ///
    /// The join-and-stream question, and the only one [`Self::NotStarted`] is
    /// excluded from: no run has ended there, so a wait for one must keep
    /// waiting.
    #[must_use]
    pub const fn has_ended(&self) -> bool {
        matches!(
            self,
            Self::Resolved(_) | Self::EndedWithoutRun | Self::EndedWithoutOutcome(_)
        )
    }

    /// How the run ended, when it ended with an answer.
    #[must_use]
    pub fn outcome(&self) -> Option<&RunOutcome> {
        match self {
            Self::Resolved(outcome) => Some(outcome),
            Self::NotStarted
            | Self::Live
            | Self::EndedWithoutRun
            | Self::EndedWithoutOutcome(_) => None,
        }
    }

    /// Why the run ended without an answer, when it ended without one.
    #[must_use]
    pub const fn missing(&self) -> Option<OutcomeMissing> {
        match self {
            Self::EndedWithoutOutcome(reason) => Some(*reason),
            Self::NotStarted | Self::Live | Self::Resolved(_) | Self::EndedWithoutRun => None,
        }
    }

    /// The stable spelling, for logs and the operator surface.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::NotStarted => "not-started",
            Self::Live => "live",
            Self::Resolved(_) => "resolved",
            Self::EndedWithoutRun => "ended-without-run",
            Self::EndedWithoutOutcome(_) => "ended-without-outcome",
        }
    }

    /// Take the run lease: the lifecycle is open and one run holds it.
    ///
    /// Answers `false` when a run already holds it — the one refusal, so the
    /// one-run-at-a-time rule is the fact's own.
    fn arm(&mut self) -> bool {
        if self.is_live() {
            return false;
        }
        *self = Self::Live;
        true
    }

    /// Record one terminal fact: the FIRST end of a run is the fact, and a later
    /// offer is told the one that stands.
    ///
    /// The write-once rule is what the resolution edge needs: a stop landing after
    /// the run resolved, and a second guard, must not re-author the answer.
    fn record(&mut self, offer: Self) -> RunFactsWrite {
        if self.has_ended() {
            return RunFactsWrite::Retained(self.clone());
        }
        *self = offer;
        RunFactsWrite::Recorded
    }
}

/// What one offer to the run's facts did — the duplicate policy, typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunFactsWrite {
    /// The offer became the run's fact.
    Recorded,
    /// The lifecycle had already ended: nothing changed, and this is the fact
    /// that stands.
    Retained(RunFacts),
}

/// Record one end of the crawl's run, reporting a losing offer so a duplicate
/// resolution is visible rather than silently dropped.
fn record_run_facts(facts: &mut RunFacts, offer: RunFacts) -> RunFactsWrite {
    let write = facts.record(offer);
    if let RunFactsWrite::Retained(held) = &write {
        diag!(
            domain = pump,
            retained = held.name(),
            "the crawl's run had already ended; the first fact stands"
        );
    }
    write
}

// ── the controller ──────────────────────────────────────────────────────────

/// The crawl: one long-lived controller per engine, holding the state machine,
/// the discovery port, the fold, and the receipt stream.
pub struct CrawlController {
    host: CrawlHost,
    config: CrawlConfig,
    source: Arc<dyn DiscoverySource>,
    seat: Arc<dyn CrawlUnitSeat>,
    state: Mutex<CrawlState>,
    /// The run's facts: the lease, the answer, and the reason an answer is
    /// missing, as ONE value under ONE lock. Lock ordering: this is the OUTERMOST
    /// lock — `state` and `receipts` may be taken while it is held, never the
    /// reverse, which is what makes a terminal fact and a closed stream the same
    /// observation ([`CrawlController::resolve_run`]).
    run_facts: Mutex<RunFacts>,
    summary: Mutex<CrawlSummary>,
    receipts: Mutex<DeliveryChannel<CrawlReceipt>>,
    sweep_lease: tokio::sync::Mutex<()>,
    /// Wakes an operator-only run when teardown begins.
    stop_notify: tokio::sync::Notify,
    seq: AtomicU64,
    /// The window's idle deadline — [`UNIT_IDLE_DEADLINE`] in production.
    ///
    /// A test's seam over the constant. It is a field and not a `cfg(test)`
    /// branch on the call site because a suite cannot spend a minute proving a
    /// parked unit is named; production never writes it, so the lock is taken
    /// once per window wait and always reads the constant back.
    unit_idle_deadline: Mutex<Duration>,
    /// How long a reported hold may keep the window waiting —
    /// [`HELD_WINDOW_BUDGET`] in production.
    ///
    /// The same test-seam shape as [`Self::unit_idle_deadline`], for the same
    /// reason: a suite cannot spend minutes proving a never-lifting cordon is
    /// named.
    held_window_budget: Mutex<Duration>,
}

/// How a crawl run resolved.
///
/// The run future's `Result` is not `Clone` and the driver's `JoinHandle` only
/// reports panics, so the run leaves its own answer here: the FFI (and any
/// consumer that owns the stream instead of the join) reads it after
/// end-of-stream. Only a run that RETURNED records one: a panic or a cancellation
/// ends [`RunFacts::EndedWithoutOutcome`], and the driver's `JoinHandle` carries
/// the `JoinError` that names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    /// Every submitted unit resolved and the sweep ended on its own terms.
    Completed,
    /// The crawl aborted loudly: the receipt fold's fatal unit.
    Aborted(String),
    /// The run itself failed (a discovery read, a seat refusal, a lost unit, a
    /// bad configuration at run time).
    Failed(String),
}

impl RunOutcome {
    /// Whether the run ended without an abort or a failure.
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        matches!(self, Self::Completed)
    }

    /// The failure text, when the run did not end cleanly.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Completed => None,
            Self::Aborted(detail) | Self::Failed(detail) => Some(detail),
        }
    }
}

impl CrawlController {
    /// Configure a controller over the process's file-backed discovery reads.
    ///
    /// # Errors
    ///
    /// [`CrawlConfigError`] — an invalid configuration, or a database the
    /// discovery source cannot open.
    pub fn attach(host: CrawlHost, config: CrawlConfig) -> Result<Arc<Self>, CrawlConfigError> {
        config.validate()?;
        let chain_id = chain_id_i64(config.chain_id)?;
        let source = DbDiscoverySource::open(config.database_path.as_path(), chain_id)?;
        Self::attach_with(host, config, Arc::new(source), Arc::new(FleetSeat))
    }

    /// Configure a controller over a caller-supplied discovery source and seat —
    /// the seam a standalone consumer (its own row store, its own executor) and
    /// a test use.
    ///
    /// # Errors
    ///
    /// [`CrawlConfigError`] — an invalid configuration.
    pub fn attach_with(
        host: CrawlHost,
        config: CrawlConfig,
        source: Arc<dyn DiscoverySource>,
        seat: Arc<dyn CrawlUnitSeat>,
    ) -> Result<Arc<Self>, CrawlConfigError> {
        config.validate()?;
        // The cap is engine state, and the shell's configure step is the one
        // place it is set: a value, applied once.
        host.admission.stages().set_path_cap(config.path_cap);
        // The crawl's verify retry policy is a construction value of the ONE
        // verify home every adapter shares, installed once here rather than
        // carried per unit.
        let mut verify = host.admission.verify().config();
        verify.retry = config.retry;
        host.admission.verify().configure(verify);
        Ok(Arc::new(Self {
            host,
            config,
            source,
            seat,
            state: Mutex::new(CrawlState::Idle),
            run_facts: Mutex::new(RunFacts::default()),
            summary: Mutex::new(CrawlSummary::default()),
            receipts: Mutex::new(DeliveryChannel::detached()),
            sweep_lease: tokio::sync::Mutex::new(()),
            stop_notify: tokio::sync::Notify::new(),
            seq: AtomicU64::new(0),
            unit_idle_deadline: Mutex::new(UNIT_IDLE_DEADLINE),
            held_window_budget: Mutex::new(HELD_WINDOW_BUDGET),
        }))
    }

    /// Attach the receipt consumer's sender — the channel's only door.
    ///
    /// # Errors
    ///
    /// [`ChannelDecline`] — a sender is already attached, or the stream is
    /// already closed (the run ended, or the crawl was stopped).
    pub fn attach_receipts(
        &self,
        tx: mpsc::UnboundedSender<CrawlReceipt>,
    ) -> Result<(), ChannelDecline> {
        self.receipts.lock().attach(tx)
    }

    /// The current pose.
    #[must_use]
    pub fn state(&self) -> CrawlState {
        *self.state.lock()
    }

    /// The sweep-completion latch, when the pose carries one.
    #[must_use]
    pub fn last_complete(&self) -> Option<Edition> {
        self.state().last_complete()
    }

    /// Whether the receipt stream has ended — the STREAM fact, not the join
    /// fact: [`DeliveryChannel::close`] is total, so a stop with no live run ends
    /// the stream without any run future having resolved. No receipt can arrive
    /// after it.
    #[must_use]
    pub fn receipts_ended(&self) -> bool {
        self.receipts.lock().phase() == ChannelPhase::Closed
    }

    /// The run's facts: the join, the answer, and the reason an answer is
    /// missing.
    ///
    /// Crate-internal on purpose: [`RunFacts`] is not part of the public `crawl`
    /// surface ([`RunOutcome`] is, and is what `run_outcome` hands back), so this
    /// door would name a type a standalone consumer could not spell.
    #[must_use]
    pub(crate) fn run_facts(&self) -> RunFacts {
        self.run_facts.lock().clone()
    }

    /// Whether the run lifecycle has ended: [`RunFacts::Live`] is the only pose a
    /// join blocks in, and the answer is never derived from the crawl's pose (a
    /// `Capped` crawl is not finished; a `Stopping` one is not finished yet).
    ///
    /// A reader that sees `true` also sees a closed receipt stream:
    /// [`CrawlController::resolve_run`] closes the stream and lands the fact under
    /// this lock, in that order.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.run_facts().has_ended()
    }

    /// The window's idle deadline (see [`UNIT_IDLE_DEADLINE`]).
    fn unit_idle_deadline(&self) -> Duration {
        *self.unit_idle_deadline.lock()
    }

    /// Shorten the window's idle deadline — a test's seam over
    /// [`UNIT_IDLE_DEADLINE`], set before the run it bounds.
    #[cfg(test)]
    pub(crate) fn set_unit_idle_deadline_for_test(&self, deadline: Duration) {
        *self.unit_idle_deadline.lock() = deadline;
    }

    /// The window's hold budget (see [`HELD_WINDOW_BUDGET`]).
    fn held_window_budget(&self) -> Duration {
        *self.held_window_budget.lock()
    }

    /// Shorten the window's hold budget — a test's seam over
    /// [`HELD_WINDOW_BUDGET`], set before the run it bounds.
    #[cfg(test)]
    pub(crate) fn set_held_window_budget_for_test(&self, budget: Duration) {
        *self.held_window_budget.lock() = budget;
    }

    /// The window's wait for ONE unit outcome, under two clocks.
    ///
    /// [`UNIT_IDLE_DEADLINE`] measures SILENCE: a seat that parks a unit, a build
    /// flight whose leader vanished, a chain read that never answers. It must NOT
    /// measure a HOLD. The fleet seat reports one ([`CrawlUnitSeat::hold`]) when a
    /// cordoned posture queues this role's units in the intake backlog, and those
    /// units are neither stuck nor lost — the cordon's clean-window hysteresis
    /// releases the whole backlog at once. A live boot is the evidence: its first
    /// window filled before the cordon entered, the cordon then outlived the
    /// minute deadline, and the park arm aborted a crawl that was merely waiting
    /// (zero receipts, and every one of them on the lift).
    ///
    /// So a reported hold re-arms the wait ([`HOLD_RECHECK`]) under its OWN budget
    /// ([`HELD_WINDOW_BUDGET`]) instead of firing the park arm, and the park arm
    /// stays exactly as it was for a seat that reports no hold. The hold is loud
    /// (one warning per continuous hold), and a hold that outlasts its budget is
    /// named ([`WaitFailure::Held`]) rather than waited on forever: a run never
    /// hangs, and never aborts a crawl it did not diagnose.
    ///
    /// The two clocks are [`WindowWait`], not a pair of locals: every observation
    /// of the seat — at the head of a step and again after its timeout — is one
    /// transition of that machine, so a hold that onsets or lifts mid-step lands
    /// in the same table as one seen at the head.
    async fn await_unit(
        &self,
        rx: &mut mpsc::UnboundedReceiver<UnitOutcome>,
    ) -> Result<Option<UnitOutcome>, WaitFailure> {
        let clocks = WaitClocks {
            idle: self.unit_idle_deadline(),
            hold: self.held_window_budget(),
        };
        let mut wait = WindowWait::Silent;
        loop {
            let step = wait.observe(self.seat.hold(), clocks, Instant::now())?;
            if let WaitEdge::HoldOnset(hold) = step.edge {
                op_warn!(
                    domain = pump,
                    hold = %hold,
                    budget_ms = millis(clocks.hold),
                    "the crawl's window is HELD, not parked — the seat holds every submitted unit; waiting for the hold to lift"
                );
            }
            wait = step.pose;
            match tokio::time::timeout(step.bound, rx.recv()).await {
                Ok(received) => return Ok(received),
                // The wait expired: silence only names a park when the seat
                // answers "no hold" at the expiry too.
                Err(_elapsed) => match step.expired(self.seat.hold()) {
                    WaitExpiry::Stalled => {
                        return Err(WaitFailure::Stalled {
                            waited: clocks.idle,
                        });
                    }
                    WaitExpiry::Wait => {}
                },
            }
        }
    }

    /// The crawl's cumulative measurement — counters and stage profile.
    #[must_use]
    pub fn summary(&self) -> CrawlSummary {
        self.summary.lock().clone()
    }

    /// The stage-profile block, as the crawler logs it.
    #[must_use]
    pub fn stage_profile_lines(&self) -> Vec<String> {
        self.summary.lock().stage_profile_lines()
    }

    /// The progress line, as the crawler logs it.
    #[must_use]
    pub fn progress_line(&self) -> String {
        self.summary.lock().progress_line()
    }

    /// Latch the crawl's stop. Total, idempotent, and never blocking.
    ///
    /// A live run future is only ASKED to stop: the pose latches
    /// [`CrawlState::Stopping`], the sweep stops submitting at its next turn, and
    /// the window drains, so no submitted unit is cancelled and no receipt is
    /// truncated. The run future's own resolution is the only path to
    /// [`CrawlState::Stopped`]; joining it is the driver's job, over the
    /// `JoinHandle` it owns, under its grace window.
    ///
    /// With no live run there is nothing to join, so the stop applies the
    /// resolution edge itself: the stream ends and the pose is terminal.
    pub fn stop(&self) {
        let mut facts = self.run_facts.lock();
        // `Stop` never declines, so this always latches.
        let _ = self.advance(CrawlEvent::Stop);
        self.stop_notify.notify_one();
        if facts.is_live() {
            // The run future finishes the teardown when it resolves.
            return;
        }
        // No run future can resolve here, so the stop IS the resolution edge: the
        // stream ends, and the fact says why nothing resolved.
        self.receipts.lock().close();
        let _ = self.advance(CrawlEvent::RunFinished);
        record_run_facts(&mut facts, RunFacts::EndedWithoutRun);
    }

    /// The crawl: one discovery sweep, windowed submission, FIFO fold.
    ///
    /// Returns when every submitted unit has resolved (the completion clause) —
    /// on natural completion, on the operator's bound, on the cap latch, or on a
    /// stop. A fatal receipt aborts loudly: the loop stops submitting, the
    /// outstanding units finish, and the abort is the return value.
    ///
    /// The CONTROLLER never spawns this future (see the module docs): the driver
    /// spawns it and keeps the `JoinHandle`. Whoever holds it gets the resolution
    /// edge for free — [`RunResolution`] ends the receipt stream, finishes a
    /// latched stop, and latches the join fact, including when this future panics
    /// or is dropped mid-flight.
    ///
    /// # Errors
    ///
    /// [`CrawlRequestError`] — a refused run, a run already in flight, a
    /// discovery failure, a seat refusal, a fatal unit abort, or a lost unit.
    ///
    /// The receipt stream is closed by [`RunResolution`], never here: a stream
    /// end written on the `Ok` path would be skipped by every `Err` and by a
    /// panic.
    pub async fn run(&self) -> Result<(), CrawlRequestError> {
        self.run_with_discovery(true).await
    }

    /// Run with or without the automatic startup sweep. Operator-only mode keeps
    /// the controller and receipt stream live for `enqueue_path` until `stop`.
    ///
    /// # Errors
    ///
    /// [`CrawlRequestError`] — a refused run, discovery failure, seat refusal,
    /// fatal unit abort, lost unit, or stop outcome.
    pub async fn run_with_discovery(
        &self,
        startup_discovery: bool,
    ) -> Result<(), CrawlRequestError> {
        self.arm_run()?;
        let mut resolution = RunResolution::new(self);
        let window = self.config.intake_window;
        // The count is the sweep's business; the run's answer is its outcome.
        let result = if startup_discovery {
            self.drive(None, window).await.map(|_submitted| ())
        } else {
            self.stop_notify.notified().await;
            Ok(())
        };
        // Settle BEFORE the guard drops: the guard takes the resolution edge,
        // and a consumer woken by end-of-stream must already be able to read how
        // the run ended.
        resolution.settle(match &result {
            Ok(()) => RunOutcome::Completed,
            Err(CrawlRequestError::Abort(abort)) => RunOutcome::Aborted(abort.to_string()),
            Err(error) => RunOutcome::Failed(error.to_string()),
        });
        result
    }

    /// How the last run resolved, when it resolved with an answer; `None` while
    /// no run has, and when one panicked or was cancelled — [`Self::run_facts`]
    /// names which.
    #[must_use]
    pub fn run_outcome(&self) -> Option<RunOutcome> {
        self.run_facts().outcome().cloned()
    }

    /// Trigger ONE bounded discovery sweep (the operator surface).
    ///
    /// A sweep over the already-latched edition is refused cheaply: the
    /// unchanged structure can only re-yield paths the pipeline already
    /// processed, so the answer is `0`. A changed (or unreadable) edition
    /// re-arms the latch before the sweep runs, and only a NATURAL, uncapped
    /// completion latches again.
    ///
    /// Returns the number of paths the sweep processed.
    ///
    /// # Errors
    ///
    /// [`CrawlRequestError`] — a refused sweep (a stopping or stopped crawl, or
    /// a sweep already in flight), a discovery failure, a seat refusal, a fatal
    /// unit abort, or a lost unit.
    pub async fn trigger_discovery(
        &self,
        bound: Option<usize>,
    ) -> Result<usize, CrawlRequestError> {
        self.advance(CrawlEvent::SweepRequested)?;
        let edition = self.source.edition().ok();
        if let (Some(probe), Some(latched)) = (edition, self.last_complete()) {
            if probe == latched {
                return Ok(0);
            }
        }
        // Re-arm: the edition moved, or the probe is unavailable (fail-open — a
        // failed probe never vetoes a sweep and never latches one).
        self.advance(CrawlEvent::EditionChanged)?;
        self.drive(bound, 1).await
    }

    /// Add ONE specific path at any time (the operator surface).
    ///
    /// Deliberately bypasses the sweep-completion latch — operator intent
    /// outranks discovery dedup — but not the controller: after `Stopped` the
    /// call refuses with a typed [`CrawlStateError`] (R6) instead of dropping the
    /// path silently. Post-cap it is admitted, and the unit answers the cheap cap
    /// skip (no build RPC).
    ///
    /// Returns the unit's submission sequence and engine path ID, or the
    /// terminal outcome when the unit did not register a path.
    ///
    /// # Errors
    ///
    /// [`CrawlRequestError`] — a stopped crawl, a seat refusal, a fatal unit
    /// abort, a lost unit, one parked past the idle deadline
    /// ([`UNIT_IDLE_DEADLINE`]), or one a posture hold kept past
    /// [`HELD_WINDOW_BUDGET`].
    pub async fn enqueue_path(
        &self,
        path: DiscoveredPath,
        directions: Option<Vec<bool>>,
    ) -> Result<(u64, u64), CrawlRequestError> {
        self.advance(CrawlEvent::PathEnqueued)?;
        let unit = CrawlUnit {
            seq: self.next_seq(),
            hops: path.hops,
            directions,
        };
        let seq = unit.seq;
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.submit(unit, &tx)?;
        // Drop the controller's own sender so the channel's end is the unit's
        // own drop — a seat that never ran the future shows up as `UnitLost`.
        drop(tx);
        // Bounded like the loop's window wait — and under the same two clocks:
        // the caller is a Python await on the far side of the FFI, so a parked
        // unit must answer the operator loudly instead of parking that call (and
        // the thread behind it) for good, while a HELD one must not be mistaken
        // for a parked one.
        let (_, outcome) = match self.await_unit(&mut rx).await {
            Ok(Some(outcome)) => outcome,
            Ok(None) => return Err(CrawlRequestError::UnitLost { outstanding: 1 }),
            Err(WaitFailure::Stalled { waited }) => {
                return Err(CrawlRequestError::UnitStalled {
                    stalled: vec![seq],
                    waited_ms: millis(waited),
                });
            }
            Err(WaitFailure::Held { held, hold }) => {
                return Err(CrawlRequestError::IntakeHeld {
                    stalled: vec![seq],
                    held_ms: millis(held),
                    hold,
                });
            }
        };
        match outcome {
            Ok(receipt) => {
                let path_id = receipt.path_id;
                let mut outcome = receipt.refusal_tag().map_or_else(
                    || receipt.kind().to_owned(),
                    |tag| format!("{}:{tag}", receipt.kind()),
                );
                let detail = match &receipt.outcome {
                    RegistrationUnitOutcome::Skip { detail, .. }
                    | RegistrationUnitOutcome::Reject { detail }
                    | RegistrationUnitOutcome::RegisterFailed { detail } => detail,
                    RegistrationUnitOutcome::Cap | RegistrationUnitOutcome::Registered { .. } => {
                        &None
                    }
                };
                if let Some(detail) = detail {
                    outcome.push_str(" (");
                    outcome.push_str(detail);
                    outcome.push(')');
                }
                self.fold(&receipt);
                let path_id = path_id.ok_or(CrawlRequestError::RegistrationNotCompleted {
                    sequence: seq,
                    outcome,
                })?;
                Ok((seq, path_id))
            }
            Err(abort) => Err(CrawlRequestError::Abort(abort)),
        }
    }

    // ── the loop ────────────────────────────────────────────────────────────

    /// Prepare the sweep off the async loop: the DB reads and the graph build
    /// are one blocking turn, never a worker stall.
    async fn prepare_sweep(&self) -> Result<DiscoverySweep, CrawlRequestError> {
        let source = Arc::clone(&self.source);
        let config = self.config.discovery()?;
        degenbot_core::runtime::get_runtime()
            .spawn_blocking(move || DiscoverySweep::prepare(source.as_ref(), &config))
            .await
            .map_err(|error| CrawlRequestError::DiscoveryTask {
                detail: error.to_string(),
            })?
            .map_err(CrawlRequestError::Discovery)
    }

    /// Pull one discovery turn: up to `discovery_batch_size` candidates, pulled
    /// between two reaps so a fast sweep cannot park a full window of unreaped
    /// receipts (the Python reaper's own discipline).
    ///
    /// The bound is spent BEFORE a candidate is pulled, so a truncated sweep never
    /// reads past the operator's limit. A source that ends mid-turn answers the
    /// prefix it did yield and `None` from then on (exhaustion is sticky), so the
    /// turn after the prefix reports the source's end.
    fn pull_turn(
        &self,
        sweep: &mut DiscoverySweep,
        batch: usize,
        bound: Option<usize>,
        submitted: usize,
    ) -> Result<Turn, CrawlRequestError> {
        let want = bound.map_or(batch, |limit| batch.min(limit.saturating_sub(submitted)));
        if want == 0 {
            return Ok(Turn::Truncated);
        }
        let mut units: Vec<CrawlUnit> = Vec::with_capacity(want);
        for _ in 0..want {
            let Some(path) = sweep.next_path()? else {
                break;
            };
            units.push(CrawlUnit {
                seq: self.next_seq(),
                hops: path.hops,
                directions: None,
            });
        }
        let mut pulled = units.into_iter();
        match pulled.next() {
            Some(first) => Ok(Turn::Pulled {
                first,
                rest: pulled.collect(),
            }),
            // `want` is at least one, so an empty turn means the first pull
            // answered `None`: there is nothing left to sweep.
            None => Ok(Turn::Exhausted),
        }
    }

    /// The windowed submission loop: discovery pulls a turn, the FIFO reaper
    /// folds, [`sweep_halt`] ends the sweep, and the window drains before the call
    /// returns. Only a [`SweepEnd::Exhausted`] end whose probe answers latches the
    /// edition.
    async fn drive(&self, bound: Option<usize>, window: usize) -> Result<usize, CrawlRequestError> {
        // One sweep at a time: the FIFO window is per-call, so two interleaved
        // sweeps would each hold a partial view of the submission order.
        let _lease = self
            .sweep_lease
            .try_lock()
            .map_err(|_| CrawlStateError::SweepInFlight)?;

        // The pose this sweep began from: a cap already latched here is the
        // operator's one-shot over a full registry, never a halt.
        let started = self.state();
        let mut sweep = self.prepare_sweep().await?;

        let (tx, rx) = mpsc::unbounded_channel();
        let mut fold = Fold::new(self, rx);
        let batch = self.config.discovery_batch_size;
        let mut turn: VecDeque<CrawlUnit> = VecDeque::new();
        let mut submitted = 0usize;

        let end = 'sweep: loop {
            // The loop's ONE halt point, at the head of every iteration: it asks
            // both "may another batch be pulled?" and "may another unit of the
            // batch in hand be submitted?". The reap comes first, so a receipt
            // folded since the last submit is read before the question — a fatal
            // unit ends the sweep before another candidate is pulled.
            fold.reap();
            if let Some(halt) = sweep_halt(started, self.state(), fold.aborted()) {
                break 'sweep SweepEnd::Halted(halt);
            }
            let unit = if let Some(unit) = turn.pop_front() {
                unit
            } else {
                match self.pull_turn(&mut sweep, batch, bound, submitted)? {
                    Turn::Pulled { first, rest } => {
                        turn.extend(rest);
                        first
                    }
                    Turn::Truncated => break 'sweep SweepEnd::Truncated,
                    Turn::Exhausted => break 'sweep SweepEnd::Exhausted,
                }
            };
            // The window records the submission BEFORE the seat can deliver (an
            // inline seat resolves the unit inside `submit`).
            let seq = unit.seq;
            self.submit(unit, &tx)?;
            fold.record(seq);
            submitted += 1;
            if fold.inflight() >= window {
                fold.resolve_oldest().await?;
            }
        };

        // The drive's own sender goes BEFORE the drain: a unit's own drop is
        // then the channel's end, so a seat that never ran a future (a lane
        // death's fault drain, a seat thread that died mid-unit) reaches the
        // loud `UnitLost` arm instead of the drain awaiting a receipt nobody
        // can send. While submission is live a sender must exist, so the same
        // silence mid-sweep is the idle deadline's to name.
        drop(tx);

        // The completion clause: every submitted unit resolves before the call
        // returns, so no unit (and no handle it cloned) outlives the crawl.
        fold.drain().await?;

        if let Some(abort) = fold.take_fatal() {
            return Err(CrawlRequestError::Abort(abort));
        }
        if end.latches() {
            // The probe decides only whether the edition is READABLE — a failed
            // probe never latches, so the next trigger sweeps instead of vetoing
            // — while the transition table decides legality: a capped crawl
            // refuses the latch, because a full registry keeps refusing and could
            // only re-offer paths this pipeline already answered.
            if let Ok(edition) = self.source.edition() {
                let _ = self.advance(CrawlEvent::SweepLatched { edition });
            }
        }
        Ok(submitted)
    }

    /// The one fold point: counters, stage profile, the receipt stream, and the
    /// cap latch. Called on the loop, in submission order, exactly once per
    /// receipt.
    fn fold(&self, receipt: &CrawlReceipt) {
        self.summary.lock().absorb(receipt);
        {
            let mut channel = self.receipts.lock();
            // A gone receiver is not a crawl failure: the fold is internal, the
            // stream is observation.
            let _ = channel.send(receipt.clone());
        }
        if matches!(receipt.outcome, RegistrationUnitOutcome::Cap) {
            let _ = self.advance(CrawlEvent::CapReached);
        }
    }

    /// Hand one unit to the seat, with its outcome funneled back to the loop.
    fn submit(&self, unit: CrawlUnit, tx: &UnitSink) -> Result<(), CrawlRequestError> {
        let host = UnitHost {
            admission: Arc::clone(&self.host.admission),
            // Per use, never held for the crawl's life: the owner's close must
            // find its own handle alone, and a unit submitted after the close
            // reads no tick maps.
            db: self.host.acquire_db(),
        };
        let tx = tx.clone();
        let seq = unit.seq;
        let future: CrawlUnitFuture = Box::pin(async move {
            let receipt = run_crawl_unit(&host.borrow(), &unit).await;
            // The host drops BEFORE the outcome is published: its per-use tick-map
            // handle is the only handle a unit holds, so a caller that has seen
            // every receipt knows no unit retains one — the completion clause the
            // owner's close (the sole-holder canary) reads.
            drop(host);
            let _ = tx.send((seq, receipt));
        });
        self.seat
            .dispatch(future)
            .map_err(|detail| CrawlRequestError::Seat { detail })
    }

    // ── the pose helpers ────────────────────────────────────────────────────

    /// Apply one event to the pose.
    fn advance(&self, event: CrawlEvent) -> Result<CrawlState, CrawlStateError> {
        let mut state = self.state.lock();
        let next = transition(*state, event)?;
        *state = next;
        Ok(next)
    }

    // ── the run lease and its resolution ────────────────────────────────────

    /// Take the run lease and mark the pipeline live under ONE lock, so a
    /// concurrent `stop` cannot slip between the pose move and the phase move
    /// (a stop that already latched `Stopping` would otherwise be outrun by a
    /// run arming itself on top of it).
    fn arm_run(&self) -> Result<(), CrawlRequestError> {
        let mut facts = self.run_facts.lock();
        if facts.is_live() {
            return Err(CrawlRequestError::RunInFlight);
        }
        // The pose's own refusal (a stopped or capped crawl) comes first: it is
        // the typed answer the caller expects, and it leaves the facts alone.
        self.advance(CrawlEvent::RunRequested)?;
        // A run already live was just ruled out, so the lease is free.
        let _ = facts.arm();
        Ok(())
    }

    /// The run future's resolution edge: the receipt stream ends, a latched stop
    /// finishes, and the join fact latches.
    ///
    /// Called from [`RunResolution`]'s `Drop`, so a normal return, a fatal `Err`,
    /// a panic unwind, and a dropped (cancelled) task all take this same edge —
    /// end-of-stream can never be missed, and the fact names which one it was.
    ///
    /// Holds the outermost lock throughout, so no reader observes a half-torn-down
    /// crawl: the stream closes FIRST and the fact lands second, under this lock,
    /// so whoever sees a terminal fact already sees a closed stream.
    fn resolve_run(&self, offer: RunFacts) {
        let mut facts = self.run_facts.lock();
        self.receipts.lock().close();
        let _ = self.advance(CrawlEvent::RunFinished);
        if let Some(reason) = offer.missing() {
            // A run that never answered is worth a line of its own: the driver's
            // join names a panic, and a cancellation has no other trace at all.
            diag!(
                domain = pump,
                reason = reason.as_str(),
                "the crawl's run ended without recording an outcome"
            );
        }
        record_run_facts(&mut facts, offer);
    }

    /// The next submission sequence (1-based, monotone for the controller's
    /// life — the receipt stream's FIFO spine).
    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::AcqRel) + 1
    }
}

/// How long the window tolerates a silent unit before the crawl names it and
/// resolves.
///
/// The completion clause has to END. A run's consumer is parked on the receipt
/// stream, and that stream ends only when the run future resolves, so a single
/// unit that never answers — a seat thread that died after accepting the future
/// (the S2 intake fault drains held work and runs it zero times), a build
/// flight's peer whose leader vanished, a chain read that never returns — would
/// otherwise hold the run, the crawl, and the boot behind it open forever with
/// no error and no log line.
///
/// The bound is per receipt, not per window: a slow-but-progressing window
/// resets it with every arrival, so it only ever measures SILENCE. Every unit
/// that completes does so in microseconds (the whole 1M-path boot averaged
/// 257µs/unit), so a minute of no receipt at all is a parked window, not slow
/// work — and the crawl says which submissions never landed instead of parking
/// with them.
const UNIT_IDLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long a SEAT-REPORTED hold may keep the window waiting before the run
/// names it.
///
/// The idle deadline above measures silence; a hold is not silence. The fleet
/// seat holds this role's units whenever the posture cordon is up, and the
/// cordon's release edge is the throttle feed's clean-window hysteresis — ten
/// seconds by default, but there is no FLOOR on it: a host under sustained
/// cgroup throttling never produces the clean sample the exit needs, and the
/// hold then lasts as long as the pressure does. A live boot proved the shape:
/// the crawl's first window filled inside a cordon, the cordon outlived the
/// minute deadline, and the run aborted a crawl whose units were queued and
/// healthy — not one had run, and not one was lost.
///
/// So a held window waits, loudly, under its own budget. Five minutes is
/// generous against a ten-second exit hysteresis while still ending: a cordon
/// that outlasts it is a posture problem the run must report
/// ([`CrawlRequestError::IntakeHeld`]) rather than a window it keeps.
const HELD_WINDOW_BUDGET: Duration = Duration::from_secs(300);

/// How often a held window re-reads the seat's hold.
///
/// A hold has no channel to wake a waiter on (the seat reports it by asking, not
/// by sending), so the held wait is a bounded tick — the seat host's own
/// backstop-timer idiom — and never a busy-spin: one short critical section per
/// tick, and a receipt still wins the race the instant it lands.
const HOLD_RECHECK: Duration = Duration::from_secs(1);

/// A duration in whole milliseconds, saturated — the `*_ms` field of every
/// window error.
fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Why a window wait was NAMED instead of landing an outcome
/// ([`CrawlController::await_unit`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitFailure {
    /// Live senders, no receipt, no hold at the expiry: the seat parks the unit.
    Stalled {
        /// How long the wait ran in silence ([`UNIT_IDLE_DEADLINE`]).
        waited: Duration,
    },
    /// The seat held the units for the whole hold budget
    /// ([`HELD_WINDOW_BUDGET`]): a cordon that never lifted.
    Held {
        /// How long the hold outlasted the budget.
        held: Duration,
        /// The seat's own reason.
        hold: SeatHold,
    },
}

// ── the window's wait ───────────────────────────────────────────────────────

/// The window's wait: what it is measuring right now.
///
/// Two clocks, two questions. [`UNIT_IDLE_DEADLINE`] measures SILENCE — a seat
/// that parks a unit, a build flight whose leader vanished, a chain read that
/// never answers. [`HELD_WINDOW_BUDGET`] measures a HOLD — a cordoned posture
/// queues this role's units and releases the whole backlog on its clean-window
/// edge, so those units are neither stuck nor lost.
///
/// The pose is the whole bookkeeping: a hold that has been seen is [`Self::Held`]
/// and carries the only datum the hold budget needs (when this CONTINUOUS hold
/// began), and the hold is announced on the [`WaitEdge::HoldOnset`] edge. That
/// edge is crossed once per continuous hold by construction — the same hold seen
/// again is [`WaitEdge::Steady`] — so no separate "already announced" flag can
/// drift from the pose it describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowWait {
    /// No hold is up: the step measures silence.
    Silent,
    /// The seat holds every submitted unit: the step measures the hold.
    Held {
        /// When this continuous hold was first observed.
        since: Instant,
    },
}

impl WindowWait {
    /// Fold the seat's answer into the next step.
    ///
    /// Total over both poses and both answers, and the whole of the hold's
    /// bookkeeping: the budget's clock starts on the onset edge, a repeated hold
    /// is steady (so the hold is announced once), a lift returns to silence, and a
    /// hold that outlasts the budget is named instead of waited on.
    ///
    /// # Errors
    ///
    /// [`WaitFailure::Held`] when the continuous hold has outlasted
    /// [`HELD_WINDOW_BUDGET`].
    fn observe(
        self,
        hold: Option<SeatHold>,
        clocks: WaitClocks,
        now: Instant,
    ) -> Result<WaitStep, WaitFailure> {
        match (self, hold) {
            // Silence, and the seat answers silence: the idle deadline is the
            // whole step.
            (Self::Silent, None) => Ok(WaitStep {
                pose: Self::Silent,
                bound: clocks.idle,
                clock: WaitClock::Silence,
                edge: WaitEdge::Steady,
            }),
            // A hold just began. Its budget starts HERE, and this is the one
            // announcement this continuous hold gets.
            (Self::Silent, Some(hold)) => Ok(WaitStep {
                pose: Self::Held { since: now },
                bound: clocks.hold.min(HOLD_RECHECK),
                clock: WaitClock::Hold,
                edge: WaitEdge::HoldOnset(hold),
            }),
            // The hold lifted: the mask is over and silence is measured again.
            (Self::Held { .. }, None) => Ok(WaitStep {
                pose: Self::Silent,
                bound: clocks.idle,
                clock: WaitClock::Silence,
                edge: WaitEdge::HoldLifted,
            }),
            // The same hold, still up: no edge, no second announcement, and the
            // step waits what is left of the budget — [`HOLD_RECHECK`] at a time,
            // because a seat reports a hold by being asked.
            (Self::Held { since }, Some(hold)) => {
                let elapsed = now.saturating_duration_since(since);
                if elapsed >= clocks.hold {
                    return Err(WaitFailure::Held {
                        held: elapsed,
                        hold,
                    });
                }
                Ok(WaitStep {
                    pose: Self::Held { since },
                    bound: clocks.hold.saturating_sub(elapsed).min(HOLD_RECHECK),
                    clock: WaitClock::Hold,
                    edge: WaitEdge::Steady,
                })
            }
        }
    }
}

/// The two clocks one window wait is bounded by — read once per wait, so a step
/// transition never takes a lock.
#[derive(Clone, Copy, Debug)]
struct WaitClocks {
    /// How long SILENCE may last before the seat is named a park
    /// ([`UNIT_IDLE_DEADLINE`]).
    idle: Duration,
    /// How long a CONTINUOUS hold may last before the cordon is the report
    /// ([`HELD_WINDOW_BUDGET`]).
    hold: Duration,
}

/// Which clock a step's bound came from — what the step's expiry is read
/// against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitClock {
    /// The idle deadline: the step measured silence.
    Silence,
    /// The hold budget: a cordon is up, and nothing is stuck.
    Hold,
}

/// The edge one observation of the seat crossed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitEdge {
    /// Nothing changed: the same silence, or the same continuous hold.
    Steady,
    /// A hold just began — the once-per-continuous-hold announcement.
    HoldOnset(SeatHold),
    /// The hold lifted: the silence clock starts over.
    HoldLifted,
}

/// One step of the window's wait: the pose to wait in, the bound it waits for,
/// the clock that bound came from, and the edge its observation crossed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WaitStep {
    pose: WindowWait,
    bound: Duration,
    clock: WaitClock,
    edge: WaitEdge,
}

impl WaitStep {
    /// What this step's timeout means, read against the seat's answer at that
    /// moment.
    ///
    /// An expiry is SILENCE only when the step measured the idle deadline and the
    /// seat answers no hold now: a step that measured a hold just ended a hold's
    /// tick, and a hold that onsets during a silent step is caught by the next
    /// observation as the onset it is — never as a park.
    const fn expired(self, hold: Option<SeatHold>) -> WaitExpiry {
        match (self.clock, hold) {
            (WaitClock::Silence, None) => WaitExpiry::Stalled,
            (WaitClock::Silence | WaitClock::Hold, _) => WaitExpiry::Wait,
        }
    }
}

/// What a step's timeout means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitExpiry {
    /// The step measured silence and nothing holds the seat now: the seat parks
    /// its units, and the idle deadline is the bound that named it.
    Stalled,
    /// Wait again: a hold is up, or one just lifted.
    Wait,
}

// ── the FIFO window ─────────────────────────────────────────────────────────

/// The submission window: the order this call submitted in, the receipts that
/// landed early, and the one fold point.
///
/// Seats are concurrent, so receipts arrive in any order. They are buffered by
/// sequence and folded only when every EARLIER submission of this call has been
/// folded — the retired workers' FIFO order, which is what the progress anchors'
/// counter drift and the `path_count % 1000` lines depend on.
struct Fold<'a> {
    controller: &'a CrawlController,
    rx: mpsc::UnboundedReceiver<UnitOutcome>,
    order: VecDeque<u64>,
    buffered: BTreeMap<u64, Result<CrawlReceipt, CrawlAbort>>,
    fatal: Option<CrawlAbort>,
}

impl<'a> Fold<'a> {
    fn new(controller: &'a CrawlController, rx: mpsc::UnboundedReceiver<UnitOutcome>) -> Self {
        Self {
            controller,
            rx,
            order: VecDeque::new(),
            buffered: BTreeMap::new(),
            fatal: None,
        }
    }

    /// How many submitted units are unresolved.
    fn inflight(&self) -> usize {
        self.order.len()
    }

    /// The unresolved submission sequences, oldest first (the stalled report).
    fn outstanding(&self) -> Vec<u64> {
        self.order.iter().copied().collect()
    }

    /// Take the loud abort, if one fired.
    fn take_fatal(&mut self) -> Option<CrawlAbort> {
        self.fatal.take()
    }

    /// Whether a submitted unit has aborted loudly — the sweep's halt input.
    fn aborted(&self) -> bool {
        self.fatal.is_some()
    }

    /// Fold everything that has already landed, without awaiting.
    fn reap(&mut self) {
        while let Ok(item) = self.rx.try_recv() {
            self.buffered.insert(item.0, item.1);
        }
        self.flush();
    }

    /// Record one submission in the window.
    fn record(&mut self, seq: u64) {
        self.order.push_back(seq);
    }

    /// Await one receipt, then fold everything it unblocks.
    ///
    /// Under the window's two clocks ([`CrawlController::await_unit`]): the idle
    /// deadline names a parked seat, the hold budget names a cordon that never
    /// lifted, and a hold in between is waited through. Either way the run
    /// resolves and names its outstanding submissions instead of parking its
    /// consumer with them.
    async fn resolve_oldest(&mut self) -> Result<(), CrawlRequestError> {
        // An inline seat may already have delivered: fold it before awaiting,
        // or the wait would block on a receipt already in hand.
        self.reap();
        if self.inflight() == 0 {
            return Ok(());
        }
        match self.controller.await_unit(&mut self.rx).await {
            Ok(Some(item)) => {
                self.buffered.insert(item.0, item.1);
                self.flush();
                Ok(())
            }
            // Every sender dropped with units still unresolved: a seat lost a
            // unit's future (a panic). At-most-once still holds; say so loudly.
            Ok(None) => Err(CrawlRequestError::UnitLost {
                outstanding: self.inflight(),
            }),
            // Live senders, no receipt, no hold: a parked unit. Name every
            // submission that never landed — a run never parks on one unit's
            // silence.
            Err(WaitFailure::Stalled { waited }) => Err(CrawlRequestError::UnitStalled {
                stalled: self.outstanding(),
                waited_ms: millis(waited),
            }),
            // A hold that outlived the run's patience: nothing is stuck, and the
            // report says which lever (the posture) to pull.
            Err(WaitFailure::Held { held, hold }) => Err(CrawlRequestError::IntakeHeld {
                stalled: self.outstanding(),
                held_ms: millis(held),
                hold,
            }),
        }
    }

    /// Drain the window: no submitted unit outlives the call, and none parks
    /// it (each wait is the idle deadline's).
    async fn drain(&mut self) -> Result<(), CrawlRequestError> {
        while self.inflight() > 0 {
            self.resolve_oldest().await?;
        }
        Ok(())
    }

    /// Fold the contiguous head of this call's submission order.
    fn flush(&mut self) {
        while let Some(&front) = self.order.front() {
            let Some(outcome) = self.buffered.remove(&front) else {
                break;
            };
            self.order.pop_front();
            match outcome {
                Ok(receipt) => self.controller.fold(&receipt),
                // The loud contract: an abort is not a counted skip, and it is
                // not lost either — the call returns it after the drain.
                Err(abort) => {
                    if self.fatal.is_none() {
                        self.fatal = Some(abort);
                    }
                }
            }
        }
    }
}

// ── the run's resolution guard ──────────────────────────────────────────────

/// The run future's resolution guard.
///
/// Its `Drop` is the resolution edge ([`CrawlController::resolve_run`]), which is
/// why it is a guard and not a tail statement: a run that returns `Err`, panics,
/// or is dropped mid-flight must still end the receipt stream — a consumer parked
/// on a stream nobody will close is the failure this type exists to make
/// impossible.
struct RunResolution<'a> {
    controller: &'a CrawlController,
    /// How the run returned; `None` while it is still running, and after a panic
    /// or a cancellation.
    settled: Option<RunOutcome>,
}

impl<'a> RunResolution<'a> {
    /// Armed before the run's work starts: from here the guard owns the edge.
    fn new(controller: &'a CrawlController) -> Self {
        Self {
            controller,
            settled: None,
        }
    }

    /// Record how the run is returning. The `Drop` takes the same edge either
    /// way, and a run that never returns never reaches here.
    fn settle(&mut self, outcome: RunOutcome) {
        self.settled = Some(outcome);
    }
}

impl Drop for RunResolution<'_> {
    fn drop(&mut self) {
        let offer = match self.settled.take() {
            Some(outcome) => RunFacts::Resolved(outcome),
            // The run never settled: an unwind is the panic arm, and a future
            // dropped outside one was cancelled by whoever owned its task.
            None => RunFacts::EndedWithoutOutcome(if std::thread::panicking() {
                OutcomeMissing::Panicked
            } else {
                OutcomeMissing::Cancelled
            }),
        };
        self.controller.resolve_run(offer);
    }
}

// ── the teardown handle ─────────────────────────────────────────────────────

/// The crawl's teardown handle — what a session's `StopPlan` carries.
///
/// Cheap (`Arc` + `Clone`), pyo3-free, and deliberately narrow: a `StopPlan`
/// holds `Option<CrawlHandle>`, never an `Arc<CrawlController>`, so the plan can
/// latch a stop and read the two facts a teardown needs and nothing else. A plan
/// must not be able to start a crawl, fold a receipt, or reach the controller's
/// interior.
///
/// No awaitable lives here either: `stop` is a latch, `is_finished` is a read,
/// and the bounded join belongs to the driver that owns the run future's
/// `JoinHandle` (see the module's "Who spawns the run").
#[derive(Clone)]
pub struct CrawlHandle {
    controller: Arc<CrawlController>,
}

impl CrawlHandle {
    /// Wrap a controller's shared handle — the driver's construction, made once
    /// at attach.
    #[must_use]
    pub fn new(controller: Arc<CrawlController>) -> Self {
        Self { controller }
    }

    /// Latch the crawl stopped (total, idempotent, never blocks).
    ///
    /// Latches [`CrawlState::Stopping`] while the run future is live; the pose
    /// only becomes [`CrawlState::Stopped`] when that future resolves.
    pub fn stop(&self) {
        self.controller.stop();
    }

    /// The pose projection (`idle` / `running` / `capped` / `stopping` /
    /// `stopped`) — what may still enter, never the join fact.
    #[must_use]
    pub fn state(&self) -> CrawlState {
        self.controller.state()
    }

    /// The join fact: whether the run lifecycle has ended.
    ///
    /// `false` while a run is live, which is the answer that says "do not park
    /// here; join the `JoinHandle` you kept, under your grace window" — and also
    /// `false` before any run armed, because that lifecycle has not ended either.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.controller.is_finished()
    }
}

#[cfg(test)]
mod tests;
