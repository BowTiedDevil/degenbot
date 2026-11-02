//! The crawl controller — the crawl's own machine, its loop, and its operator
//! surface.
//!
//! The Python `PathRegistrationPipeline` is TWO state machines wearing one
//! object: the benign registry-cap latch (`capped`) and a re-armable
//! sweep-completion latch (`_sweep_completed_edition`). Everything else on the
//! object is either a fold (counters, stage profile) or a knob. Here the two
//! latches are one enum — [`CrawlState`] — because they are the same kind of
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
//! - the JOIN ([`CrawlController::is_finished`], a [`RunPhase`]) answers "would a
//!   join block?", whose only writer is the run future itself.
//!
//! # Who spawns the run
//!
//! The controller does not spawn its own future. The DRIVER spawns
//! [`CrawlController::run`] on the runtime and owns the `JoinHandle`, because
//! that handle is the only thing that reports a panic — and a panicked crawl run
//! has to be classified (`PumpExitCause`) by the session that owns it. There is
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
//! submission (`intake_window` outstanding units, the Python's
//! `REG_INTAKE_WINDOW = 32`), receipts resolve in FIFO submission order, and the
//! single fold point ([`CrawlSummary::absorb`]) runs on the loop, never on a
//! seat. That is the Python loop's shape with the crossings removed — the
//! counters, the stage profile and the cap latch are engine state now.
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

use alloy::primitives::Address;
use degenbot_core::channel_lifecycle::{ChannelDecline, ChannelPhase, DeliveryChannel};
use degenbot_db::snapshot::TickMapDb;
use degenbot_pathfinding::PoolKind;
use parking_lot::Mutex;
use tokio::sync::mpsc;

use crate::arb_engine::crawl::discovery::{
    CrawlDiscoveryError, DbDiscoverySource, DiscoveredPath, DiscoveryConfig, DiscoverySource,
    DiscoverySweep, Edition,
};
use crate::arb_engine::crawl::flights::BuildFlights;
use crate::arb_engine::crawl::policy::CrawlPathPolicy;
use crate::arb_engine::crawl::profile::CrawlSummary;
use crate::arb_engine::crawl::receipt::CrawlReceipt;
use crate::arb_engine::crawl::unit::{
    run_crawl_unit, CrawlAbort, CrawlLedger, CrawlUnit, CrawlUnitHost, CrawlUnitSettings,
};
use crate::arb_engine::driver::EngineDriver;
use crate::bot_core::construction_io::ConstructionIo;
use crate::bot_core::registration_ledger::RegistrationUnitOutcome;
use crate::bot_core::verification_retry::RetryPolicy;
use crate::bot_core::{Bot, ConstructionRoute};

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

// ── the configuration ───────────────────────────────────────────────────────

/// The resolved crawl configuration — a VALUE, supplied by the driver shell.
///
/// Everything the crawl needs to be configured once and then run: the discovery
/// scope, the unit policy, and the two budgets (the intake window and the path
/// cap). The shell resolves the route/policy/permutation filter; the crawl
/// invents none of them.
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
    /// Path-start tokens (WETH and the native sentinel for V4).
    pub start_tokens: Vec<Address>,
    /// Path-end tokens.
    pub end_tokens: Vec<Address>,
    /// The minimum hop count, before any per-depth filter floor.
    pub min_depth: usize,
    /// The maximum hop count (`None` = unbounded).
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
    /// signed id the database reads take.
    pub fn discovery(&self) -> Result<DiscoveryConfig, CrawlConfigError> {
        Ok(DiscoveryConfig {
            chain_id: chain_id_i64(self.chain_id)?,
            pool_kinds: self.pool_kinds.clone(),
            pool_type_per_depth: self.pool_type_per_depth.clone(),
            start_tokens: self.start_tokens.clone(),
            end_tokens: self.end_tokens.clone(),
            min_depth: self.min_depth,
            max_depth: self.max_depth,
        })
    }

    /// The unit-wide values every seat reads.
    fn unit_settings(&self) -> CrawlUnitSettings {
        CrawlUnitSettings {
            route: self.route.clone(),
            policy: self.policy.clone(),
            retry: self.retry,
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
    /// The chain id does not fit the database's signed id column.
    #[error("chain id {chain_id} is out of range for the database reads")]
    ChainIdOutOfRange {
        /// The rejected id.
        chain_id: u64,
    },
    /// The discovery source could not be opened.
    #[error(transparent)]
    Discovery(#[from] CrawlDiscoveryError),
}

/// The database's signed chain id, from the configuration's unsigned one.
fn chain_id_i64(chain_id: u64) -> Result<i64, CrawlConfigError> {
    i64::try_from(chain_id).map_err(|_| CrawlConfigError::ChainIdOutOfRange { chain_id })
}

// ── the host ────────────────────────────────────────────────────────────────

/// Everything the controller drives, held as shared handles so a unit's future
/// can own its clones.
///
/// The driver owns the registry and the verify claim table, the bot owns
/// construction state, the I/O holds the RPC/DB construction seams, and the
/// snapshot handle is the tick-map arm a head construction reads.
pub struct CrawlHost {
    /// The shared per-chain orchestrator.
    pub bot: Arc<Bot>,
    /// The engine driver: verify lifecycles + the path registry.
    pub driver: Arc<EngineDriver>,
    /// The construction-I/O handle.
    pub io: Arc<ConstructionIo>,
    /// The snapshot DB handle (`None` = construction reads no tick maps).
    pub db: Option<Arc<dyn TickMapDb>>,
}

impl CrawlHost {
    /// Compose a host from the driver every engine surface already holds.
    #[must_use]
    pub fn from_driver(
        driver: &Arc<EngineDriver>,
        io: Arc<ConstructionIo>,
        db: Option<Arc<dyn TickMapDb>>,
    ) -> Self {
        Self {
            bot: Arc::clone(driver.bot()),
            driver: Arc::clone(driver),
            io,
            db,
        }
    }
}

/// The reused host handles, moved into one unit's future.
struct UnitHost {
    bot: Arc<Bot>,
    driver: Arc<EngineDriver>,
    io: Arc<ConstructionIo>,
    db: Option<Arc<dyn TickMapDb>>,
    ledger: Arc<CrawlLedger>,
    flights: Arc<BuildFlights>,
    settings: Arc<CrawlUnitSettings>,
}

impl UnitHost {
    /// Borrow the crawl unit's host view for one `run_crawl_unit` call.
    fn borrow(&self) -> CrawlUnitHost<'_> {
        CrawlUnitHost {
            bot: &self.bot,
            driver: &self.driver,
            io: &self.io,
            db: self.db.as_deref(),
            ledger: &self.ledger,
            flights: &self.flights,
            settings: &self.settings,
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
}

// ── the run's own machine ───────────────────────────────────────────────────

/// Where the RUN FUTURE is — the join fact, on its own axis.
///
/// Deliberately not derived from [`CrawlState`]: a capped crawl is not finished
/// (its run may still be draining the window), and a stop only makes a crawl
/// finishable. [`Self::Finished`] is written by the run future's resolution
/// guard (or by a `stop` that proves no run can ever resolve), so the answer is
/// "would a join block?" and nothing else.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunPhase {
    /// No run has been spawned on this controller.
    #[default]
    NotStarted,
    /// A run future is live; a join would block.
    Live,
    /// The last run future resolved (or a stop proved none can). Nothing is
    /// live.
    Finished,
}

// ── the controller ──────────────────────────────────────────────────────────

/// The crawl: one long-lived controller per engine, holding the state machine,
/// the discovery port, the fold, and the receipt stream.
pub struct CrawlController {
    host: CrawlHost,
    config: CrawlConfig,
    source: Arc<dyn DiscoverySource>,
    ledger: Arc<CrawlLedger>,
    flights: Arc<BuildFlights>,
    settings: Arc<CrawlUnitSettings>,
    seat: Arc<dyn CrawlUnitSeat>,
    state: Mutex<CrawlState>,
    /// The join fact's own machine. Lock ordering: this is the OUTERMOST lock —
    /// `state` and `receipts` may be taken while it is held, never the reverse.
    run_phase: Mutex<RunPhase>,
    summary: Mutex<CrawlSummary>,
    receipts: Mutex<DeliveryChannel<CrawlReceipt>>,
    sweep_lease: tokio::sync::Mutex<()>,
    seq: AtomicU64,
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
        host.driver.set_path_cap(config.path_cap);
        let settings = Arc::new(config.unit_settings());
        Ok(Arc::new(Self {
            host,
            config,
            source,
            ledger: Arc::new(CrawlLedger::new()),
            flights: Arc::new(BuildFlights::new()),
            settings,
            seat,
            state: Mutex::new(CrawlState::Idle),
            run_phase: Mutex::new(RunPhase::NotStarted),
            summary: Mutex::new(CrawlSummary::default()),
            receipts: Mutex::new(DeliveryChannel::detached()),
            sweep_lease: tokio::sync::Mutex::new(()),
            seq: AtomicU64::new(0),
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

    /// The join fact's phase.
    #[must_use]
    pub fn run_phase(&self) -> RunPhase {
        *self.run_phase.lock()
    }

    /// Whether a join would block: [`RunPhase::Live`] is the only phase that says
    /// "still running", and the answer is never derived from the crawl's pose (a
    /// `Capped` crawl is not finished; a `Stopping` one is not finished yet).
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.run_phase() == RunPhase::Finished
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
        let mut phase = self.run_phase.lock();
        // `Stop` never declines, so this always latches.
        let _ = self.advance(CrawlEvent::Stop);
        if *phase == RunPhase::Live {
            // The run future finishes the teardown when it resolves.
            return;
        }
        self.receipts.lock().close();
        let _ = self.advance(CrawlEvent::RunFinished);
        *phase = RunPhase::Finished;
    }

    /// The crawl: one discovery sweep, windowed submission, FIFO fold.
    ///
    /// Returns when every submitted unit has resolved (the completion clause) —
    /// on natural completion, on the cap latch, or on a stop. A fatal receipt
    /// aborts loudly: the loop stops submitting, the outstanding units finish,
    /// and the abort is the return value.
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
        self.arm_run()?;
        let _resolution = RunResolution { controller: self };
        let window = self.config.intake_window;
        // The count is the sweep's business; the run's answer is its outcome.
        self.drive(None, window).await.map(|_submitted| ())
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
    /// Returns the unit's submission sequence.
    ///
    /// # Errors
    ///
    /// [`CrawlRequestError`] — a stopped crawl, a seat refusal, a fatal unit
    /// abort, or a lost unit.
    pub async fn enqueue_path(
        &self,
        path: DiscoveredPath,
        directions: Option<Vec<bool>>,
    ) -> Result<u64, CrawlRequestError> {
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
        let Some((_, outcome)) = rx.recv().await else {
            return Err(CrawlRequestError::UnitLost { outstanding: 1 });
        };
        match outcome {
            Ok(receipt) => {
                self.fold(&receipt);
                Ok(seq)
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

    /// The windowed submission loop: discovery pulls, the FIFO reaper folds, the
    /// cap latch and a stop both stop new work, and the window drains before the
    /// call returns.
    async fn drive(&self, bound: Option<usize>, window: usize) -> Result<usize, CrawlRequestError> {
        // One sweep at a time: the FIFO window is per-call, so two interleaved
        // sweeps would each hold a partial view of the submission order.
        let _lease = self
            .sweep_lease
            .try_lock()
            .map_err(|_| CrawlStateError::SweepInFlight)?;

        // Capture the pose BEFORE the sweep: "the cap fired during THIS call" is
        // the only cap that halts a sweep (a sweep that began post-cap folds the
        // cheap cap skips, as the Python operator surface does).
        let started_capped = matches!(self.state(), CrawlState::Capped);
        let mut sweep = self.prepare_sweep().await?;

        let (tx, rx) = mpsc::unbounded_channel();
        let mut fold = Fold::new(self, rx);
        let mut submitted = 0usize;
        let mut truncated = false;
        let mut exhausted = false;
        let batch = self.config.discovery_batch_size;

        'sweep: loop {
            if self.sweep_halted(started_capped) {
                break;
            }
            if let Some(limit) = bound {
                if submitted >= limit {
                    truncated = true;
                    break;
                }
            }

            // One discovery turn: at most `discovery_batch_size` candidates,
            // pulled between two reaps so a fast sweep cannot park a full window
            // of unreaped receipts (the Python reaper's own discipline).
            let mut turn: Vec<CrawlUnit> = Vec::with_capacity(batch);
            for _ in 0..batch {
                if let Some(limit) = bound {
                    if submitted + turn.len() >= limit {
                        truncated = true;
                        break;
                    }
                }
                let Some(path) = sweep.next_path()? else {
                    exhausted = true;
                    break;
                };
                turn.push(CrawlUnit {
                    seq: self.next_seq(),
                    hops: path.hops,
                    directions: None,
                });
            }
            if turn.is_empty() {
                break;
            }

            for unit in turn {
                fold.reap();
                if fold.fatal.is_some() || self.sweep_halted(started_capped) {
                    break 'sweep;
                }
                // The window records the submission BEFORE the seat can
                // deliver (an inline seat resolves the unit inside `submit`).
                let seq = unit.seq;
                self.submit(unit, &tx)?;
                fold.record(seq);
                submitted += 1;
                if fold.inflight() >= window {
                    fold.resolve_oldest().await?;
                }
            }
            if truncated || exhausted {
                break;
            }
        }

        // The completion clause: every submitted unit resolves before the call
        // returns, so no unit (and no handle it cloned) outlives the crawl.
        fold.drain().await?;

        if let Some(abort) = fold.take_fatal() {
            return Err(CrawlRequestError::Abort(abort));
        }
        let natural = exhausted && !truncated && !self.sweep_halted(started_capped);
        if natural {
            // The latch sets only on a natural, uncapped completion, and only
            // when the probe answers: a failed probe never latches, so the next
            // trigger sweeps instead of vetoing.
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
            bot: Arc::clone(&self.host.bot),
            driver: Arc::clone(&self.host.driver),
            io: Arc::clone(&self.host.io),
            db: self.host.db.clone(),
            ledger: Arc::clone(&self.ledger),
            flights: Arc::clone(&self.flights),
            settings: Arc::clone(&self.settings),
        };
        let tx = tx.clone();
        let seq = unit.seq;
        let future: CrawlUnitFuture = Box::pin(async move {
            let receipt = run_crawl_unit(&host.borrow(), &unit).await;
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

    /// Whether this sweep must stop submitting.
    ///
    /// A stop always halts the sweep. The cap halts only when it fired *during*
    /// this call: a sweep that began post-cap is the operator's own one-shot and
    /// folds the cheap cap skips instead of returning early.
    fn sweep_halted(&self, started_capped: bool) -> bool {
        match self.state() {
            // A latched stop halts submission immediately, whether or not the
            // run future has resolved yet.
            CrawlState::Stopping | CrawlState::Stopped => true,
            CrawlState::Capped => !started_capped,
            CrawlState::Idle | CrawlState::Running { .. } => false,
        }
    }

    // ── the run lease and its resolution ────────────────────────────────────

    /// Take the run lease and mark the pipeline live under ONE lock, so a
    /// concurrent `stop` cannot slip between the pose move and the phase move
    /// (a stop that already latched `Stopping` would otherwise be outrun by a
    /// run arming itself on top of it).
    fn arm_run(&self) -> Result<(), CrawlRequestError> {
        let mut phase = self.run_phase.lock();
        if *phase == RunPhase::Live {
            return Err(CrawlRequestError::RunInFlight);
        }
        // The pose's own refusal (a stopped or capped crawl) comes first: it is
        // the typed answer the caller expects, and it leaves the phase alone.
        self.advance(CrawlEvent::RunRequested)?;
        *phase = RunPhase::Live;
        Ok(())
    }

    /// The run future's resolution edge: the receipt stream ends, a latched stop
    /// finishes, and the join fact latches.
    ///
    /// Called from [`RunResolution`]'s `Drop`, so a normal return, a fatal `Err`,
    /// a panic unwind, and a dropped (cancelled) task all take this same edge —
    /// end-of-stream can never be missed. Holds the outermost lock throughout, so
    /// no reader observes a half-torn-down crawl: whoever sees [`RunPhase::Finished`]
    /// already sees a closed stream.
    fn resolve_run(&self) {
        let mut phase = self.run_phase.lock();
        self.receipts.lock().close();
        let _ = self.advance(CrawlEvent::RunFinished);
        *phase = RunPhase::Finished;
    }

    /// The next submission sequence (1-based, monotone for the controller's
    /// life — the receipt stream's FIFO spine).
    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::AcqRel) + 1
    }
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

    /// Take the loud abort, if one fired.
    fn take_fatal(&mut self) -> Option<CrawlAbort> {
        self.fatal.take()
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
    async fn resolve_oldest(&mut self) -> Result<(), CrawlRequestError> {
        // An inline seat may already have delivered: fold it before awaiting,
        // or the wait would block on a receipt already in hand.
        self.reap();
        if self.inflight() == 0 {
            return Ok(());
        }
        match self.rx.recv().await {
            Some(item) => {
                self.buffered.insert(item.0, item.1);
                self.flush();
                Ok(())
            }
            // Every sender dropped with units still unresolved: a seat lost a
            // unit's future (a panic). At-most-once still holds; say so loudly.
            None => Err(CrawlRequestError::UnitLost {
                outstanding: self.inflight(),
            }),
        }
    }

    /// Drain the window: no submitted unit outlives the call.
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
}

impl Drop for RunResolution<'_> {
    fn drop(&mut self) {
        self.controller.resolve_run();
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

    /// The join fact: whether the run future has resolved.
    ///
    /// `false` while a run is live, which is the answer that says "do not park
    /// here; join the `JoinHandle` you kept, under your grace window".
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.controller.is_finished()
    }
}

#[cfg(test)]
mod tests;
