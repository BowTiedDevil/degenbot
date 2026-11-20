//! The strategy host: the process-wide owner of one event hub, one route
//! registry, one nonce authority, and the registered strategy drivers.
//!
//! A strategy is admitted to the host as a *driver FSM instance*: a named
//! record whose lifecycle is `Registered -> Enabled -> Running -> {Stopped,
//! Halted, Disabled}`. The lifecycle is a total transition function — an operator verb
//! that would move a driver out of turn returns a typed decline rather than
//! panicking. `Halted` and `Disabled` are terminal: a halt is a frozen
//! tombstone the operator can query but never restart, and only a fresh
//! registration (a new process) can revive the name.
//!
//! The host stores each driver as one authoritative lifecycle value and
//! projects it to the copyable [`DriverRecord`] a caller reads, so the facts a
//! pose owns travel inside the pose instead of beside it: a pre-start record
//! owns its loop-provisioning slot, and a tombstone owns the cause it halted
//! on. A `Running` record therefore doubles as proof that the start boundary
//! consumed its factory, and a halted record without a cause is
//! unrepresentable.
//!
//! The host owns the shared handles and lends them out; it deliberately starts
//! no driver loop in this module, so the host's only job is admission and
//! lifecycle. Enabling a name the host never registered, or a name registered
//! without a configured facet, fails loudly.
//!
//! A driver that owns process-lifetime artifacts is lent a [`LaneNamespace`]
//! under the host state root, and a driver that runs a loop is provisioned with
//! a spawn factory the host boots at [`StrategyHost::start_driving`]. The host
//! itself never boots a loop inline. Every enabled driver carries a factory by
//! the time the start boundary runs — including the settlement arm, whose
//! factory is the engine pump session's lifetime — so an enabled record with no
//! loop is a miswired boot and is refused rather than silently never started.
//! The factory is once-only: a second attach is a typed refusal, never a silent
//! replacement of the loop the record already promises.
//!
//! Loop death is observed, never inferred. A driver's loop reports through
//! [`DriverTask`], and two host-owned edges fold that report —
//! [`StrategyHost::drive_and_fold`] for a consumer awaiting its own task and
//! [`HostSupervisor`] for the hosted boots. No transition is drop-based: a
//! task's death is not synchronously observable, so nothing pretends it is.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use degenbot_eventhub::Hub;
use indexmap::IndexMap;
use tokio::sync::mpsc::UnboundedSender;

use degenbot_substrate::driver_spawn::{DriverExit, DriverSpawnFactory, LaneNamespace};
use degenbot_substrate::nonce::{NonceAuthority, StrategyId};
use degenbot_substrate::route_registry::RouteRegistry;

/// The copyable pose label of one registered strategy driver: what a caller
/// compares and reports. It is distinct from the driver loop's own protocol run
/// phase, which the host folds in on the driver's exit, and it is the label of
/// the host's authoritative lifecycle value, which owns the payload each pose
/// carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverPose {
    /// Named to the host; not yet enabled by the operator.
    Registered,
    /// Enabled by the operator; the driver's loop has not started.
    Enabled,
    /// The driver's loop is running.
    Running,
    /// The driver's loop returned cleanly: the lane is gone, but the record is
    /// not a tombstone. The operator may still disable it.
    Stopped,
    /// Terminal tombstone: the driver halted on its own local violation.
    Halted,
    /// Terminal: the operator disabled the driver.
    Disabled,
}

impl DriverPose {
    /// Whether no further transition is possible. A terminal state is a frozen
    /// record, never a restart candidate.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Halted | Self::Disabled)
    }
}

/// Why a lifecycle move was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FsmDecline {
    /// `enable` is only legal from [`DriverPose::Registered`].
    #[error("enable requires the Registered state")]
    EnableRequiresRegistered,
    /// `start` is only legal from [`DriverPose::Enabled`].
    #[error("running requires the Enabled state")]
    RunningRequiresEnabled,
    /// `halt` is only legal from [`DriverPose::Running`].
    #[error("halt requires the Running state")]
    HaltRequiresRunning,
    /// `stop` is only legal from [`DriverPose::Running`].
    #[error("stop requires the Running state")]
    StopRequiresRunning,
    /// `start` was called on a record the host holds a loop factory for: the
    /// start boundary is the one move that may claim a provisioned loop running.
    #[error("start requires an enabled record the host is not booting")]
    RunningRequiresUnbooted,
    /// `disable` refuses the terminal states: a tombstone is frozen.
    #[error("disable rejects terminal states (Halted/Disabled are frozen)")]
    DisableRejectsTerminal,
}

/// Why a loop-factory attach was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SpawnDecline {
    /// The factory is once-only provisioning: a second attach would replace the
    /// loop the record already promises.
    #[error("its loop factory is already attached")]
    AlreadyAttached,
    /// Only a pre-start record can be provisioned: a booted or terminal record
    /// would hold a factory nothing ever hands to a start boundary.
    #[error("it is past the pre-start provisioning window")]
    PastProvisioning,
}

/// The cause a halted tombstone carries: what the driver's own loop reported
/// when it stopped on a local violation.
///
/// It is a payload of the lifecycle's halted variant, so a halted record cannot
/// exist without one.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HaltCause(String);

impl HaltCause {
    fn new(detail: impl Into<String>) -> Self {
        Self(detail.into())
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

/// The loop-provisioning slot a pre-start driver owns.
///
/// Unwired is legal on both pre-start poses: a `Registered` record is
/// provisioned by its boot before the start boundary, and an `Enabled` record
/// whose boot missed its arm is refused at that boundary rather than at the
/// enable move. The slot is reachable only through the pre-start variants, so it
/// cannot outlive the window in which a boot can take it.
enum SpawnSlot {
    /// No loop factory has been attached yet.
    Unwired,
    /// The boot's once-only factory, waiting for the start boundary.
    Wired(DriverSpawnFactory),
}

impl SpawnSlot {
    /// Whether a factory is waiting for the start boundary.
    const fn is_wired(&self) -> bool {
        matches!(self, Self::Wired(_))
    }
}

impl fmt::Debug for SpawnSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unwired => f.write_str("Unwired"),
            Self::Wired(_) => f.write_str("Wired(..)"),
        }
    }
}

/// The host's authoritative value for one driver: the lifecycle pose and every
/// payload that pose owns.
///
/// The moves below are the total transition function, and [`DriverRecord`] is
/// this value's projection. A payload never outlives the pose that owns it: the
/// provisioning slot travels through the pre-start poses and is consumed by the
/// start boundary, and a tombstone carries the cause it halted on.
#[derive(Debug)]
enum DriverLifecycle {
    /// Named to the host; not yet enabled by the operator. The slot holds the
    /// loop factory the boot may hand to the start boundary.
    Registered(SpawnSlot),
    /// Enabled by the operator; the driver's loop has not started. The slot
    /// still holds the boot's factory, if one was attached.
    Enabled(SpawnSlot),
    /// The driver's loop is running: its factory was consumed to get here.
    Running,
    /// The driver's loop returned cleanly: the lane is gone, but the record is
    /// not a tombstone. The operator may still disable it.
    Stopped,
    /// Terminal tombstone: the driver halted on its own local violation,
    /// carrying the cause it reported.
    Halted(HaltCause),
    /// Terminal: the operator disabled the driver.
    Disabled,
}

impl DriverLifecycle {
    /// The copyable pose plus the tombstone cause, as one total projection: the
    /// facts a caller may read cannot disagree, because one match produces both.
    fn projection(&self) -> (DriverPose, Option<&HaltCause>) {
        match self {
            Self::Registered(_) => (DriverPose::Registered, None),
            Self::Enabled(_) => (DriverPose::Enabled, None),
            Self::Running => (DriverPose::Running, None),
            Self::Stopped => (DriverPose::Stopped, None),
            Self::Halted(cause) => (DriverPose::Halted, Some(cause)),
            Self::Disabled => (DriverPose::Disabled, None),
        }
    }

    /// The lifecycle's pose label.
    fn pose(&self) -> DriverPose {
        self.projection().0
    }

    /// Whether a loop factory is waiting for the start boundary.
    const fn has_spawn(&self) -> bool {
        match self {
            Self::Registered(slot) | Self::Enabled(slot) => slot.is_wired(),
            Self::Running | Self::Stopped | Self::Halted(_) | Self::Disabled => false,
        }
    }

    /// The operator enable move: `Registered -> Enabled`, carrying the
    /// provisioning slot across unchanged.
    ///
    /// # Errors
    ///
    /// [`FsmDecline::EnableRequiresRegistered`] from any other state.
    fn on_enable(&mut self) -> Result<(), FsmDecline> {
        match self {
            Self::Registered(slot) => {
                let slot = std::mem::replace(slot, SpawnSlot::Unwired);
                *self = Self::Enabled(slot);
                Ok(())
            }
            Self::Enabled(_) | Self::Running | Self::Stopped | Self::Halted(_) | Self::Disabled => {
                Err(FsmDecline::EnableRequiresRegistered)
            }
        }
    }

    /// The driver's own start move: `Enabled -> Running`.
    ///
    /// A record the host holds a factory for reaches `Running` through the
    /// host's start boundary, which consumes that factory; this verb is for a
    /// loop that is already live and has no host boot behind it.
    ///
    /// # Errors
    ///
    /// [`FsmDecline::RunningRequiresEnabled`] from any other state, and
    /// [`FsmDecline::RunningRequiresUnbooted`] while the boot still holds this
    /// record's factory.
    fn on_start(&mut self) -> Result<(), FsmDecline> {
        match self {
            Self::Enabled(SpawnSlot::Unwired) => {
                *self = Self::Running;
                Ok(())
            }
            Self::Enabled(SpawnSlot::Wired(_)) => Err(FsmDecline::RunningRequiresUnbooted),
            Self::Registered(_)
            | Self::Running
            | Self::Stopped
            | Self::Halted(_)
            | Self::Disabled => Err(FsmDecline::RunningRequiresEnabled),
        }
    }

    /// The driver's own halt move: `Running -> Halted`, carrying the cause.
    ///
    /// # Errors
    ///
    /// [`FsmDecline::HaltRequiresRunning`] from any other state.
    fn on_halt(&mut self, cause: HaltCause) -> Result<(), FsmDecline> {
        match self {
            Self::Running => {
                *self = Self::Halted(cause);
                Ok(())
            }
            Self::Registered(_)
            | Self::Enabled(_)
            | Self::Stopped
            | Self::Halted(_)
            | Self::Disabled => Err(FsmDecline::HaltRequiresRunning),
        }
    }

    /// The driver's own clean-stop move: `Running -> Stopped`.
    ///
    /// # Errors
    ///
    /// [`FsmDecline::StopRequiresRunning`] from any other state.
    fn on_stop(&mut self) -> Result<(), FsmDecline> {
        if matches!(self, Self::Running) {
            *self = Self::Stopped;
            Ok(())
        } else {
            Err(FsmDecline::StopRequiresRunning)
        }
    }

    /// The operator disable move: any non-terminal state -> `Disabled`.
    ///
    /// # Errors
    ///
    /// [`FsmDecline::DisableRejectsTerminal`] from Halted or Disabled.
    fn on_disable(&mut self) -> Result<(), FsmDecline> {
        if self.pose().is_terminal() {
            return Err(FsmDecline::DisableRejectsTerminal);
        }
        *self = Self::Disabled;
        Ok(())
    }

    /// Attach the once-only loop factory a pre-start record is promised.
    ///
    /// # Errors
    ///
    /// [`SpawnDecline::AlreadyAttached`] when the slot is already wired, and
    /// [`SpawnDecline::PastProvisioning`] once the record left the pre-start
    /// window: a booted or terminal record would hold a factory nothing ever
    /// hands to a start boundary.
    fn attach_spawn(&mut self, factory: DriverSpawnFactory) -> Result<(), SpawnDecline> {
        match self {
            Self::Registered(slot) | Self::Enabled(slot) => match slot {
                SpawnSlot::Unwired => {
                    *slot = SpawnSlot::Wired(factory);
                    Ok(())
                }
                SpawnSlot::Wired(_) => Err(SpawnDecline::AlreadyAttached),
            },
            Self::Running | Self::Stopped | Self::Halted(_) | Self::Disabled => {
                Err(SpawnDecline::PastProvisioning)
            }
        }
    }

    /// Consume the provisioning slot and claim `Running`: the one move that
    /// boots a loop, so a `Running` record is proof its factory was taken.
    ///
    /// Returns `None` and leaves the value untouched when no factory is waiting
    /// — an `Enabled` record the start boundary's pre-flight never admits.
    fn begin_running(&mut self) -> Option<DriverSpawnFactory> {
        let previous = std::mem::replace(self, Self::Running);
        match previous {
            Self::Enabled(SpawnSlot::Wired(factory)) => Some(factory),
            other => {
                *self = other;
                None
            }
        }
    }
}

/// One driver as the host stores it: the admission status the operator reads and
/// the authoritative lifecycle value.
#[derive(Debug)]
struct DriverEntry {
    facet: FacetStatus,
    lifecycle: DriverLifecycle,
}

impl DriverEntry {
    /// The record projection a caller may hold.
    fn record(&self, id: &StrategyId) -> DriverRecord {
        let (state, halt) = self.lifecycle.projection();
        DriverRecord {
            id: id.clone(),
            facet: self.facet,
            state,
            halt: halt.cloned(),
        }
    }
}

/// The cockpit session's operator-facing lifecycle: the four-pose shell the
/// Python `BotRunner` presents over the host-owned engine.
///
/// The host owns one operator lifecycle per driver ([`DriverPose`]); the
/// cockpit session is the settlement engine's shell, so its phase table lives
/// here beside the driver FSM rather than being authored a second time in
/// Python. The Python `_Phase` translates these verdicts and never decides
/// legality — the Rust table is the single authority.
///
/// ```text
/// New ──start()──► Started ──run()──► Running
///  │                  │                  │
///  └──── shutdown() ──┴─ shutdown() ─────┴──► Closed
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    /// Constructed; no actors built.
    New,
    /// Actors built; the pump has not resumed.
    Started,
    /// The main loop owns the session.
    Running,
    /// Terminal: teardown ran (idempotent from any phase).
    Closed,
}

/// Why a cockpit session-phase move was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SessionDecline {
    /// `start` is only legal from [`SessionPhase::New`]; `Started` re-entry is
    /// a deliberate idempotent no-op.
    #[error("session can only start from New")]
    StartRequiresNew,
    /// `run` is only legal from [`SessionPhase::Started`].
    #[error("run requires the Started phase")]
    RunRequiresStarted,
    /// The operator add-a-path/discovery verbs are only legal while
    /// [`SessionPhase::Running`].
    #[error("query requires the Running phase")]
    QueryRequiresRunning,
}

impl SessionPhase {
    /// The operator startup move: `New -> Started`. `Started` re-entry is an
    /// idempotent no-op; `Running`/`Closed` decline.
    ///
    /// # Errors
    ///
    /// [`SessionDecline::StartRequiresNew`] from `Running`/`Closed`.
    pub fn on_start(self) -> Result<Self, SessionDecline> {
        match self {
            Self::New | Self::Started => Ok(Self::Started),
            Self::Running | Self::Closed => Err(SessionDecline::StartRequiresNew),
        }
    }

    /// The main-loop entry move: `Started -> Running`.
    ///
    /// # Errors
    ///
    /// [`SessionDecline::RunRequiresStarted`] from any other phase.
    pub fn on_run(self) -> Result<Self, SessionDecline> {
        if self == Self::Started {
            Ok(Self::Running)
        } else {
            Err(SessionDecline::RunRequiresStarted)
        }
    }

    /// The operator add-a-path/discovery move: legal only while `Running` (the
    /// phase is unchanged).
    ///
    /// # Errors
    ///
    /// [`SessionDecline::QueryRequiresRunning`] from any other phase.
    pub fn on_query(self) -> Result<Self, SessionDecline> {
        if self == Self::Running {
            Ok(Self::Running)
        } else {
            Err(SessionDecline::QueryRequiresRunning)
        }
    }

    /// The teardown move: every phase reaches [`SessionPhase::Closed`];
    /// idempotent, so it can never decline.
    #[must_use]
    pub const fn on_shutdown(self) -> Self {
        match self {
            Self::New | Self::Started | Self::Running | Self::Closed => Self::Closed,
        }
    }

    /// The Python-facing lower-case name of this phase.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Started => "started",
            Self::Running => "running",
            Self::Closed => "closed",
        }
    }
}

/// Whether a registered driver has a usable config facet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FacetStatus {
    /// The booted config named this driver's facet with its required keys.
    Configured,
    /// The name is known but no facet was booted; enabling must fail loudly.
    Unconfigured,
}

/// The host's queryable record for one driver FSM instance: the authoritative
/// lifecycle projected to the copyable facts a caller may read.
///
/// The projection is total over the lifecycle value, so [`Self::state`] and
/// [`Self::halt_detail`] cannot disagree: a cause is present exactly on a
/// halted tombstone. A record is a value the caller owns, so reading the plane
/// never holds a borrow into the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverRecord {
    id: StrategyId,
    facet: FacetStatus,
    state: DriverPose,
    halt: Option<HaltCause>,
}

impl DriverRecord {
    /// The driver's registered name.
    #[must_use]
    pub fn id(&self) -> &StrategyId {
        &self.id
    }

    /// The driver's facet status at registration.
    #[must_use]
    pub fn facet(&self) -> FacetStatus {
        self.facet
    }

    /// The driver's current lifecycle state.
    #[must_use]
    pub fn state(&self) -> DriverPose {
        self.state
    }

    /// The halt cause, present exactly on a halted tombstone.
    #[must_use]
    pub fn halt_detail(&self) -> Option<&str> {
        self.halt.as_ref().map(HaltCause::as_str)
    }
}

/// A refused host verb.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    /// `register` named a driver the host already has.
    #[error("strategy \"{0}\" is already registered")]
    AlreadyRegistered(StrategyId),
    /// `enable`/`disable`/`halt` named a driver the host never registered.
    #[error("unknown strategy \"{0}\": the host has no registered driver with that name")]
    UnknownStrategy(StrategyId),
    /// `enable` named a registered driver with no configured facet.
    #[error("strategy \"{0}\" is unconfigured: no facet with its required keys was booted")]
    UnconfiguredStrategy(StrategyId),
    /// The lifecycle refused the move.
    #[error("strategy \"{id}\" cannot transition: {decline}")]
    Transition {
        /// The driver the move targeted.
        id: StrategyId,
        /// The refused move's typed reason.
        decline: FsmDecline,
    },
    /// `lane_namespace` named a driver that has never been enabled (or was
    /// disabled): no lane resources exist to name.
    #[error("strategy \"{0}\" has no enabled lane namespace")]
    StrategyNotEnabled(StrategyId),
    /// `lane_namespace` was called before the host learned its state root.
    #[error("the host has no state root installed")]
    StateRootUnset,
    /// `start_driving` found an enabled driver with no spawn factory: the boot
    /// never wired the loop the record's `Enabled` pose promises.
    #[error(
        "strategy \"{0}\" is enabled with no spawn factory: the boot never wired its driver loop"
    )]
    MissingSpawn(StrategyId),
    /// `attach_spawn` was refused: the provisioning slot is once-only and exists
    /// only before the record's loop boots.
    #[error("strategy \"{id}\" cannot take a loop factory: {decline}")]
    Spawn {
        /// The driver the attach targeted.
        id: StrategyId,
        /// The refused attach's typed reason.
        decline: SpawnDecline,
    },
}

/// The typed fate of one submission, addressed to the strategy that owns it.
///
/// This is the host's notice vocabulary for a head update. It mirrors the
/// submission ledger's reconcile outcomes and adds the reorg rewind's lease
/// revocation, so a driver receives every way its signed nonce can stop being
/// its own without reading the ledger or authority directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrategyNotice {
    /// The chain's next nonce passed the submission's nonce: the slot was
    /// consumed on-chain.
    Landed,
    /// The nonce left the authority's outstanding set without landing: the
    /// reservation was released or replaced before the chain confirmed it.
    Stale,
    /// A gap opened immediately below the submission: the strategy's natural
    /// fill is the vacated predecessor.
    Orphaned {
        /// The vacated predecessor nonce the strategy may re-stamp at.
        fillable_nonce: u64,
    },
    /// The strategy's live lease for `nonce` was revoked: a reorg rewound the
    /// window it was stamped inside, or the head sweep found the chain had
    /// already passed it. The lease must be re-obtained.
    LeaseRevoked {
        /// The revoked lease's nonce.
        nonce: u64,
    },
}

/// One typed notice addressed to the strategy that owns the affected nonce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadNotice {
    strategy: StrategyId,
    nonce: u64,
    notice: StrategyNotice,
}

impl HeadNotice {
    /// Build a notice for the owning strategy.
    #[must_use]
    pub fn new(strategy: StrategyId, nonce: u64, notice: StrategyNotice) -> Self {
        Self {
            strategy,
            nonce,
            notice,
        }
    }

    /// The strategy that owns the affected submission.
    #[must_use]
    pub fn strategy(&self) -> &StrategyId {
        &self.strategy
    }

    /// The account nonce whose fate changed.
    #[must_use]
    pub const fn nonce(&self) -> u64 {
        self.nonce
    }

    /// What happened to the submission.
    #[must_use]
    pub const fn notice(&self) -> &StrategyNotice {
        &self.notice
    }
}

/// The head feed's submission-truth arm.
///
/// The per-strategy submission ledger lives in the submission crate, which
/// depends on this one, so the host cannot name it. The host instead drives a
/// trait object: its head update refreshes the authority (reorg-safe) and then
/// asks the reconciler to close outstanding records out against that snapshot.
/// The submission ledger implements this trait, and its notices travel in the
/// host's [`StrategyNotice`] vocabulary, so delivery stays host-owned.
pub trait HeadReconciler: Send + Sync {
    /// Reconcile every outstanding submission record against the chain's
    /// confirmed nonce and the authority's outstanding set, returning one
    /// notice per state change.
    fn reconcile_head(&self, confirmed: u64, outstanding: &[u64]) -> Vec<HeadNotice>;

    /// Whether any submission record is still non-terminal.
    ///
    /// The host's per-head feed reads this with the authority's outstanding set
    /// to decide whether a head update has anything to reconcile: a boot with
    /// no hosted activity must not pay for a chain-nonce refresh.
    fn has_outstanding(&self) -> bool {
        false
    }
}

/// A driver loop the host started, awaiting its terminal exit.
pub struct DriverTask {
    id: StrategyId,
    handle: tokio::task::JoinHandle<DriverExit>,
}

impl DriverTask {
    /// The driver the task runs.
    #[must_use]
    pub fn id(&self) -> &StrategyId {
        &self.id
    }

    /// Await the loop's terminal exit. A panicked task is a self-halt: the
    /// lane boundary turns the unwinding panic into a tombstone instead of
    /// letting it reach the host process.
    pub async fn wait(self) -> DriverExit {
        self.handle
            .await
            .unwrap_or_else(|error| DriverExit::Halted(format!("driver task failed: {error}")))
    }
}

/// The process-wide host: one hub, one route registry, one nonce authority,
/// and the registered driver FSM instances.
pub struct StrategyHost {
    hub: Arc<Hub>,
    registry: Arc<RouteRegistry>,
    nonce: Arc<NonceAuthority>,
    /// The registered drivers, each held as one authoritative lifecycle value
    /// that owns the payloads its pose carries.
    drivers: IndexMap<StrategyId, DriverEntry>,
    state_root: Option<PathBuf>,
    /// The submission-truth arm of the head feed, installed by the boot that
    /// owns the per-strategy ledger. `None` until the submission crate's
    /// reconciler is attached, which keeps the host usable by pure-bot
    /// consumers that never submit.
    reconciler: Option<Arc<dyn HeadReconciler>>,
    /// Per-strategy notice sinks. A head update delivers each notice only to
    /// the strategy that owns the affected record.
    head_sinks: parking_lot::Mutex<IndexMap<StrategyId, UnboundedSender<HeadNotice>>>,
}

/// The host's fresh hub paired with the caller's attachment.
///
/// [`StrategyHost::mint`] builds the pair together, so the hub a driver sees and
/// the value it was minted with travel as one value and cannot be mis-paired.
pub struct HostHub<T> {
    hub: Arc<Hub>,
    attachment: T,
}

impl<T> HostHub<T> {
    /// The hub half of the pair.
    #[must_use]
    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    /// Split the pair into its hub and the caller's attachment.
    #[must_use]
    pub fn into_parts(self) -> (Arc<Hub>, T) {
        (self.hub, self.attachment)
    }
}

impl StrategyHost {
    /// A host owning the shared handles and no registered drivers.
    #[must_use]
    pub fn new(hub: Arc<Hub>, registry: Arc<RouteRegistry>, nonce: Arc<NonceAuthority>) -> Self {
        Self {
            hub,
            registry,
            nonce,
            drivers: IndexMap::new(),
            state_root: None,
            reconciler: None,
            head_sinks: parking_lot::Mutex::new(IndexMap::new()),
        }
    }

    /// Mint the process hub and pair it with the caller's attachment.
    ///
    /// Infallible: the hub is fresh and the attachment is a value the caller
    /// already owns, so there is no declaration to validate and no partially
    /// registered hub to reach. The returned [`HostHub`] carries the hub and the
    /// attachment as one pair, so a driver consumes them together.
    #[must_use]
    pub fn mint<T>(
        registry: Arc<RouteRegistry>,
        nonce: Arc<NonceAuthority>,
        attachment: T,
    ) -> (Self, HostHub<T>) {
        let hub = Arc::new(Hub::new());
        (
            Self::new(Arc::clone(&hub), registry, nonce),
            HostHub { hub, attachment },
        )
    }

    /// The host's shared event hub.
    #[must_use]
    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    /// The host's shared route registry.
    #[must_use]
    pub fn registry(&self) -> &Arc<RouteRegistry> {
        &self.registry
    }

    /// The host's shared nonce authority.
    #[must_use]
    pub fn nonce(&self) -> &Arc<NonceAuthority> {
        &self.nonce
    }

    /// Install the submission-truth arm the head feed reconciles against.
    ///
    /// Called once at boot by the layer that owns the per-strategy ledger. A
    /// host without one still refreshes the authority on every head; it simply
    /// has no submission records to close out.
    pub fn attach_reconciler(&mut self, reconciler: Arc<dyn HeadReconciler>) {
        self.reconciler = Some(reconciler);
    }

    /// Subscribe a registered strategy to its head notices.
    ///
    /// # Errors
    ///
    /// [`HostError::UnknownStrategy`] for a name the host never registered.
    pub fn subscribe_head(
        &self,
        id: &StrategyId,
        sink: UnboundedSender<HeadNotice>,
    ) -> Result<(), HostError> {
        if !self.drivers.contains_key(id) {
            return Err(HostError::UnknownStrategy(id.clone()));
        }
        self.head_sinks.lock().insert(id.clone(), sink);
        Ok(())
    }

    /// Whether any strategy holds a lease/broadcast or any reconciled
    /// submission record is still non-terminal.
    ///
    /// The per-head feed gates its one chain-nonce read on this, so an idle
    /// host pays nothing per head. The gate is one rule for every boot shape:
    /// the heads that reach it come from the session's ONE hub head source
    /// (the block pump's accepted-header edge), whatever strategies are active.
    #[must_use]
    pub fn has_hosted_activity(&self) -> bool {
        self.nonce.has_outstanding()
            || self
                .reconciler
                .as_ref()
                .is_some_and(|reconciler| reconciler.has_outstanding())
    }

    /// Apply one head update: refresh the confirmed chain nonce reorg-safely,
    /// reconcile submission truth against the authority snapshot, and deliver
    /// every typed notice to the strategy that owns it.
    ///
    /// The reorg-safe refresh is the only head entry point: a backward move
    /// restores reorged broadcasts and revokes leases stamped inside the
    /// rewound window, while a forward move lands confirmed broadcasts. The
    /// returned notices are the same values delivered to the sinks, so an
    /// in-process caller can act without subscribing.
    ///
    /// One writer, two consumers: the authority writes every nonce-lifespan
    /// fact, this method is the sole construction and delivery site for head
    /// notices, and the return value and the per-strategy sinks carry the
    /// identical values.
    #[must_use]
    pub fn on_head(&self, confirmed: u64) -> Vec<HeadNotice> {
        let advisories = self.nonce.set_confirmed_reorg(confirmed);
        let outstanding = self.nonce.outstanding_nonces();
        let mut notices: Vec<HeadNotice> = advisories
            .into_iter()
            .map(|advisory| {
                HeadNotice::new(
                    advisory.strategy().clone(),
                    advisory.nonce(),
                    StrategyNotice::LeaseRevoked {
                        nonce: advisory.nonce(),
                    },
                )
            })
            .collect();
        if let Some(reconciler) = &self.reconciler {
            notices.extend(reconciler.reconcile_head(confirmed, &outstanding));
        }
        {
            let sinks = self.head_sinks.lock();
            for notice in &notices {
                if let Some(sink) = sinks.get(&notice.strategy) {
                    let _ = sink.send(notice.clone());
                }
            }
        }
        notices
    }

    /// Admit a strategy as a driver FSM instance in [`DriverPose::Registered`].
    ///
    /// # Errors
    ///
    /// [`HostError::AlreadyRegistered`] if the name is taken.
    pub fn register(
        &mut self,
        id: impl Into<StrategyId>,
        facet: FacetStatus,
    ) -> Result<(), HostError> {
        let id = id.into();
        if self.drivers.contains_key(&id) {
            return Err(HostError::AlreadyRegistered(id));
        }
        self.drivers.insert(
            id,
            DriverEntry {
                facet,
                lifecycle: DriverLifecycle::Registered(SpawnSlot::Unwired),
            },
        );
        Ok(())
    }

    /// Enable a registered, configured driver.
    ///
    /// # Errors
    ///
    /// [`HostError::UnknownStrategy`] if the name is not registered;
    /// [`HostError::UnconfiguredStrategy`] if no facet was booted;
    /// [`HostError::Transition`] if the lifecycle refuses the move.
    pub fn enable(&mut self, id: &StrategyId) -> Result<DriverPose, HostError> {
        let entry = self.entry_mut(id)?;
        if entry.facet == FacetStatus::Unconfigured {
            return Err(HostError::UnconfiguredStrategy(id.clone()));
        }
        entry
            .lifecycle
            .on_enable()
            .map_err(|decline| HostError::Transition {
                id: id.clone(),
                decline,
            })?;
        Ok(entry.lifecycle.pose())
    }

    /// Move an enabled driver to running. Called by the driver once its loop is
    /// live, never by the operator.
    ///
    /// A record the host is holding a loop factory for is refused: the start
    /// boundary consumes that factory, so this verb belongs only to a loop that
    /// is already live and has no host boot behind it.
    ///
    /// # Errors
    ///
    /// [`HostError::UnknownStrategy`] / [`HostError::Transition`].
    pub fn start(&mut self, id: &StrategyId) -> Result<DriverPose, HostError> {
        let entry = self.entry_mut(id)?;
        entry
            .lifecycle
            .on_start()
            .map_err(|decline| HostError::Transition {
                id: id.clone(),
                decline,
            })?;
        Ok(entry.lifecycle.pose())
    }

    /// Tombstone a running driver with the cause its loop reported, releasing
    /// every nonce reservation the strategy still holds.
    ///
    /// # Errors
    ///
    /// [`HostError::UnknownStrategy`] / [`HostError::Transition`].
    pub fn halt(&mut self, id: &StrategyId, detail: impl Into<String>) -> Result<(), HostError> {
        {
            let entry = self.entry_mut(id)?;
            entry
                .lifecycle
                .on_halt(HaltCause::new(detail))
                .map_err(|decline| HostError::Transition {
                    id: id.clone(),
                    decline,
                })?;
        }
        self.nonce.release_strategy(id);
        Ok(())
    }

    /// Disable a non-terminal driver, releasing every nonce reservation the
    /// strategy still holds.
    ///
    /// # Errors
    ///
    /// [`HostError::UnknownStrategy`] / [`HostError::Transition`].
    pub fn disable(&mut self, id: &StrategyId) -> Result<(), HostError> {
        {
            let entry = self.entry_mut(id)?;
            entry
                .lifecycle
                .on_disable()
                .map_err(|decline| HostError::Transition {
                    id: id.clone(),
                    decline,
                })?;
        }
        self.nonce.release_strategy(id);
        Ok(())
    }

    /// Install the state root lane namespaces resolve under.
    pub fn set_state_root(&mut self, root: impl Into<PathBuf>) {
        self.state_root = Some(root.into());
    }

    /// The lane namespace for a driver that has been enabled.
    ///
    /// A registered-but-never-enabled driver (or a disabled one) owns no lane
    /// resources, so naming a namespace for it is refused. A halted tombstone
    /// still names its namespace: the artifacts outlive the tombstone and the
    /// operator may need to inspect them.
    ///
    /// # Errors
    ///
    /// [`HostError::UnknownStrategy`] for an unregistered name;
    /// [`HostError::StrategyNotEnabled`] for a driver owning no lane;
    /// [`HostError::StateRootUnset`] when no state root was installed.
    pub fn lane_namespace(&self, id: &StrategyId) -> Result<LaneNamespace, HostError> {
        let entry = self.entry(id)?;
        if matches!(
            entry.lifecycle.pose(),
            DriverPose::Registered | DriverPose::Disabled
        ) {
            return Err(HostError::StrategyNotEnabled(id.clone()));
        }
        let root = self.state_root.clone().ok_or(HostError::StateRootUnset)?;
        Ok(LaneNamespace::under(root, id))
    }

    /// Attach the once-only spawn factory a driver's loop is booted from.
    ///
    /// Provisioning is legal on both pre-start poses: a boot may wire a
    /// `Registered` driver or one the operator has already enabled. The factory
    /// lives in the record's provisioning slot, and the start boundary consumes
    /// it, so a booted record holds none.
    ///
    /// # Errors
    ///
    /// [`HostError::UnknownStrategy`] for an unregistered name, and
    /// [`HostError::Spawn`] when the record already holds a factory or has left
    /// the pre-start window.
    pub fn attach_spawn(
        &mut self,
        id: &StrategyId,
        spawn: DriverSpawnFactory,
    ) -> Result<(), HostError> {
        let entry = self.entry_mut(id)?;
        entry
            .lifecycle
            .attach_spawn(spawn)
            .map_err(|decline| HostError::Spawn {
                id: id.clone(),
                decline,
            })
    }

    /// Whether a driver holds a spawn factory waiting for the start boundary.
    ///
    /// Every facet the boot admits carries one: a hosted arm's factory comes
    /// from its arm row, and the settlement arm's comes from the loop the boot
    /// composes over the pump session it minted.
    #[must_use]
    pub fn has_spawn(&self, id: &StrategyId) -> bool {
        self.drivers
            .get(id)
            .is_some_and(|entry| entry.lifecycle.has_spawn())
    }

    /// Boot every enabled driver's loop, on the shared runtime, and claim it
    /// [`DriverPose::Running`].
    ///
    /// This is the host's one driving edge, and the one move that consumes a
    /// record's provisioning slot: a `Running` record is proof its factory was
    /// taken. Each returned [`DriverTask`] is the lane boundary — the caller
    /// awaits it and folds the exit through [`Self::drive_and_fold`] or hands it
    /// to a [`HostSupervisor`], so a lane panic becomes a tombstone rather than
    /// a host unwind.
    ///
    /// The fleet is pre-flighted before any loop boots: an enabled driver with
    /// no factory is a boot that promised a `Running` record it never wired, and
    /// refusing the whole start leaves no half-started fleet behind.
    ///
    /// # Errors
    ///
    /// [`HostError::MissingSpawn`] for an enabled driver the boot never wired a
    /// loop for.
    pub fn start_driving(&mut self) -> Result<Vec<DriverTask>, HostError> {
        let runtime = degenbot_core::runtime::get_runtime();
        let ids: Vec<StrategyId> = self
            .drivers
            .iter()
            .filter(|(_, entry)| entry.lifecycle.pose() == DriverPose::Enabled)
            .map(|(id, _)| id.clone())
            .collect();
        if let Some(unwired) = ids.iter().find(|id| !self.has_spawn(id)) {
            return Err(HostError::MissingSpawn(unwired.clone()));
        }
        // Every factory is taken before any loop boots: each take is that
        // record's own move to `Running`, so no task starts over a fleet that
        // was only half claimed.
        let mut booted = Vec::with_capacity(ids.len());
        for id in ids {
            let lane = self.lane_namespace(&id).ok();
            let Some(spawn) = self.entry_mut(&id)?.lifecycle.begin_running() else {
                // Unreachable: the pre-flight proved every enabled record holds
                // a factory, and nothing took one since.
                debug_assert!(false, "an enabled driver lost its spawn factory");
                return Err(HostError::MissingSpawn(id));
            };
            booted.push((id, spawn, lane));
        }
        let mut tasks = Vec::with_capacity(booted.len());
        for (id, spawn, lane) in booted {
            // A lane's replay stack is single-threaded, so the loop future is
            // `!Send` and must be polled inline on one thread, never spawned.
            // Drive it under the AMBIENT multi-thread runtime (via `block_on`
            // from a blocking thread) so the lane sees the same runtime
            // ambience a dedicated process entrypoint provides: the evm2 sync
            // DB bridge (`BlockingAsyncDb::new`) captures the current handle only
            // when that runtime is multi-threaded, and its layer reads then use
            // `block_in_place`. A dedicated current-thread runtime captures no
            // handle and forbids block-in-place, so every
            // `Evm2BlockSimHandle::build` would fail.
            let handle = runtime.spawn_blocking(move || {
                let future = spawn(lane);
                runtime.block_on(future)
            });
            tasks.push(DriverTask { id, handle });
        }
        Ok(tasks)
    }

    /// Fold a started driver's terminal exit into the FSM: a self-halt is a
    /// tombstone carrying the cause; a clean stop moves the record to
    /// [`DriverPose::Stopped`] so a dead loop is never reported as
    /// [`DriverPose::Running`].
    ///
    /// Host-internal: the fold is reachable only through [`Self::drive_and_fold`]
    /// (the standalone consumer) or [`HostSupervisor`] (the hosted consumers),
    /// so no external caller can re-implement or bypass it.
    ///
    /// # Errors
    ///
    /// [`HostError::UnknownStrategy`] / [`HostError::Transition`] from the
    /// resulting lifecycle move.
    fn record_driver_exit(&mut self, id: &StrategyId, exit: DriverExit) -> Result<(), HostError> {
        match exit {
            DriverExit::Halted(detail) => self.halt(id, detail),
            DriverExit::Stopped => {
                let entry = self.entry_mut(id)?;
                entry
                    .lifecycle
                    .on_stop()
                    .map_err(|decline| HostError::Transition {
                        id: id.clone(),
                        decline,
                    })
            }
        }
    }

    /// Await one started driver's terminal exit and fold it into the FSM.
    ///
    /// This is the host's supervision edge: a caller that booted [`Self::start_driving`]
    /// hands each returned [`DriverTask`] here instead of re-implementing the
    /// await-then-[`Self::record_driver_exit`] ritual. The fold stays host-owned,
    /// so a clean stop is [`DriverPose::Stopped`] and a self-halt is a tombstone.
    ///
    /// # Errors
    ///
    /// [`HostError::UnknownStrategy`] / [`HostError::Transition`] from the
    /// resulting lifecycle move.
    pub async fn drive_and_fold(&mut self, task: DriverTask) -> Result<(), HostError> {
        let id = task.id().clone();
        let exit = task.wait().await;
        self.record_driver_exit(&id, exit)
    }

    /// Every registered driver, in registration order, as its record
    /// projection.
    #[must_use]
    pub fn list(&self) -> Vec<DriverRecord> {
        self.drivers
            .iter()
            .map(|(id, entry)| entry.record(id))
            .collect()
    }

    /// One driver's record projection, or `None` if the name is unknown.
    #[must_use]
    pub fn record(&self, id: &StrategyId) -> Option<DriverRecord> {
        self.drivers.get(id).map(|entry| entry.record(id))
    }

    /// One driver's lifecycle state, or `None` if the name is unknown.
    #[must_use]
    pub fn state_of(&self, id: &StrategyId) -> Option<DriverPose> {
        self.drivers.get(id).map(|entry| entry.lifecycle.pose())
    }

    fn entry(&self, id: &StrategyId) -> Result<&DriverEntry, HostError> {
        self.drivers
            .get(id)
            .ok_or_else(|| HostError::UnknownStrategy(id.clone()))
    }

    fn entry_mut(&mut self, id: &StrategyId) -> Result<&mut DriverEntry, HostError> {
        self.drivers
            .get_mut(id)
            .ok_or_else(|| HostError::UnknownStrategy(id.clone()))
    }
}

/// The host's driver supervisor: owns the supervision tasks for every driver
/// the host started, and folds each terminal exit through the host FSM.
///
/// This composes the await-then-fold ritual inside the host crate, so
/// [`StrategyHost::record_driver_exit`] stays private and no external caller
/// can re-implement — or bypass — the fold. A self-halt becomes a tombstone; a
/// clean stop lands [`DriverPose::Stopped`]; neither is ever reported as
/// [`DriverPose::Running`].
///
/// The supervisor, not the record, owns the running tasks: a record is a
/// copyable value a caller reads, and a task's death is observable only by
/// awaiting its [`DriverTask`], so the supervision handle lives where that await
/// is composed. Nothing folds an exit at drop time:
/// a dropped handle detaches its task but cannot report, so a pose claiming a
/// synchronous drop observation would lie about the loop it names.
pub struct HostSupervisor {
    host: Arc<parking_lot::Mutex<StrategyHost>>,
    tasks: parking_lot::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl HostSupervisor {
    /// A supervisor over the shared host handle. The supervisor holds its own
    /// `Arc` clone; the host stays the single owner of the FSM records.
    #[must_use]
    pub fn new(host: Arc<parking_lot::Mutex<StrategyHost>>) -> Self {
        Self {
            host,
            tasks: parking_lot::Mutex::new(Vec::new()),
        }
    }

    /// Spawn one supervision task per started driver. Each awaits its driver's
    /// lane boundary and folds the terminal exit through the host FSM.
    pub fn supervise(&self, tasks: Vec<DriverTask>) {
        let runtime = degenbot_core::runtime::get_runtime();
        for task in tasks {
            let id = task.id().clone();
            let host = Arc::clone(&self.host);
            let supervision = runtime.spawn(async move {
                let exit = task.wait().await;
                if let Err(error) = host.lock().record_driver_exit(&id, exit) {
                    tracing::warn!(
                        strategy = %id,
                        %error,
                        "driver exit not folded into the strategy FSM"
                    );
                }
            });
            self.tasks.lock().push(supervision);
        }
    }

    /// The number of supervision tasks the supervisor owns.
    #[must_use]
    pub fn live_count(&self) -> usize {
        self.tasks.lock().len()
    }
}

impl fmt::Debug for StrategyHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StrategyHost")
            .field("drivers", &self.list())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "unit tests: a malformed fixture must fail the test loudly, including a lane that panics on purpose"
)]
mod tests {
    use super::*;
    use crate::connector_index::V2ConnectorIndex;
    use std::path::Path;

    fn sid(name: &str) -> StrategyId {
        StrategyId::new(name)
    }

    fn host() -> StrategyHost {
        StrategyHost::new(
            Arc::new(Hub::new()),
            Arc::new(RouteRegistry::new(V2ConnectorIndex::default())),
            Arc::new(NonceAuthority::new(7)),
        )
    }

    fn register(host: &mut StrategyHost, name: &str) -> StrategyId {
        let id = sid(name);
        host.register(id.clone(), FacetStatus::Configured)
            .expect("register");
        id
    }

    const POSES: [DriverPose; 6] = [
        DriverPose::Registered,
        DriverPose::Enabled,
        DriverPose::Running,
        DriverPose::Stopped,
        DriverPose::Halted,
        DriverPose::Disabled,
    ];

    /// One representative lifecycle value per pose, all with an idle
    /// provisioning slot.
    fn lifecycle(pose: DriverPose) -> DriverLifecycle {
        match pose {
            DriverPose::Registered => DriverLifecycle::Registered(SpawnSlot::Unwired),
            DriverPose::Enabled => DriverLifecycle::Enabled(SpawnSlot::Unwired),
            DriverPose::Running => DriverLifecycle::Running,
            DriverPose::Stopped => DriverLifecycle::Stopped,
            DriverPose::Halted => DriverLifecycle::Halted(HaltCause::new("recorded cause")),
            DriverPose::Disabled => DriverLifecycle::Disabled,
        }
    }

    fn a_factory() -> DriverSpawnFactory {
        Box::new(|_lane| Box::pin(async { DriverExit::Stopped }))
    }

    #[test]
    fn the_lifecycle_transition_table_is_total_and_closed() {
        for pose in POSES {
            let mut value = lifecycle(pose);
            if pose == DriverPose::Registered {
                assert_eq!(value.on_enable(), Ok(()));
                assert_eq!(value.pose(), DriverPose::Enabled);
            } else {
                assert_eq!(value.on_enable(), Err(FsmDecline::EnableRequiresRegistered));
                assert_eq!(value.pose(), pose, "a refused move leaves the value alone");
            }

            let mut value = lifecycle(pose);
            if pose == DriverPose::Enabled {
                assert_eq!(value.on_start(), Ok(()));
                assert_eq!(value.pose(), DriverPose::Running);
            } else {
                assert_eq!(value.on_start(), Err(FsmDecline::RunningRequiresEnabled));
                assert_eq!(value.pose(), pose, "a refused move leaves the value alone");
            }

            let mut value = lifecycle(pose);
            if pose == DriverPose::Running {
                assert_eq!(value.on_halt(HaltCause::new("halted here")), Ok(()));
                assert_eq!(value.pose(), DriverPose::Halted);
                assert_eq!(
                    value.projection().1.map(HaltCause::as_str),
                    Some("halted here"),
                    "the tombstone carries the cause it halted on"
                );
            } else {
                assert_eq!(
                    value.on_halt(HaltCause::new("halted here")),
                    Err(FsmDecline::HaltRequiresRunning)
                );
                assert_eq!(value.pose(), pose, "a refused move leaves the value alone");
            }

            let mut value = lifecycle(pose);
            if pose == DriverPose::Running {
                assert_eq!(value.on_stop(), Ok(()));
                assert_eq!(value.pose(), DriverPose::Stopped);
            } else {
                assert_eq!(value.on_stop(), Err(FsmDecline::StopRequiresRunning));
                assert_eq!(value.pose(), pose, "a refused move leaves the value alone");
            }

            let mut value = lifecycle(pose);
            if pose.is_terminal() {
                assert_eq!(value.on_disable(), Err(FsmDecline::DisableRejectsTerminal));
                assert_eq!(value.pose(), pose);
            } else {
                assert_eq!(value.on_disable(), Ok(()));
                assert_eq!(value.pose(), DriverPose::Disabled);
            }
        }
    }

    /// A cause is a payload of the halted variant, so the pose and the cause a
    /// record reports come from one value and cannot disagree.
    #[test]
    fn only_a_tombstone_projects_a_halt_cause() {
        for pose in POSES {
            let value = lifecycle(pose);
            let (projected, cause) = value.projection();
            assert_eq!(projected, pose);
            assert_eq!(
                cause.is_some(),
                pose == DriverPose::Halted,
                "a cause is present exactly on a halted tombstone"
            );
        }
    }

    /// The provisioning slot is once-only and lives only where a boot can still
    /// take it: it travels across the enable move, is handed over exactly once
    /// by the start boundary, and is unreachable afterwards.
    #[test]
    fn the_provisioning_slot_is_once_only_and_pre_start() {
        let mut value = lifecycle(DriverPose::Registered);
        assert!(!value.has_spawn(), "a fresh record holds no factory");
        value.attach_spawn(a_factory()).expect("attach the factory");
        assert!(value.has_spawn());
        assert_eq!(
            value.attach_spawn(a_factory()),
            Err(SpawnDecline::AlreadyAttached),
            "a second attach never replaces the factory the record promises"
        );
        assert!(
            value.has_spawn(),
            "the refusal left the first factory in place"
        );

        value.on_enable().expect("enable");
        assert!(value.has_spawn(), "the enable move carries the slot");
        assert_eq!(
            value.on_start(),
            Err(FsmDecline::RunningRequiresUnbooted),
            "a provisioned record is claimed running by the host's start boundary"
        );
        assert!(
            value.begin_running().is_some(),
            "the start boundary consumes the factory"
        );
        assert_eq!(value.pose(), DriverPose::Running);
        assert!(
            !value.has_spawn(),
            "a running record holds no provisioning slot"
        );
        assert_eq!(
            value.attach_spawn(a_factory()),
            Err(SpawnDecline::PastProvisioning),
            "a booted record cannot take a factory nothing would ever boot"
        );
        assert!(
            value.begin_running().is_none(),
            "the boot edge claims a record once"
        );
        assert_eq!(value.pose(), DriverPose::Running);
    }

    #[test]
    fn the_session_phase_table_is_total_and_closed() {
        let phases = [
            SessionPhase::New,
            SessionPhase::Started,
            SessionPhase::Running,
            SessionPhase::Closed,
        ];
        for phase in phases {
            if matches!(phase, SessionPhase::New | SessionPhase::Started) {
                assert_eq!(phase.on_start(), Ok(SessionPhase::Started));
            } else {
                assert_eq!(phase.on_start(), Err(SessionDecline::StartRequiresNew));
            }

            if phase == SessionPhase::Started {
                assert_eq!(phase.on_run(), Ok(SessionPhase::Running));
            } else {
                assert_eq!(phase.on_run(), Err(SessionDecline::RunRequiresStarted));
            }

            if phase == SessionPhase::Running {
                assert_eq!(phase.on_query(), Ok(SessionPhase::Running));
            } else {
                assert_eq!(phase.on_query(), Err(SessionDecline::QueryRequiresRunning));
            }

            assert_eq!(phase.on_shutdown(), SessionPhase::Closed);
        }
    }

    #[test]
    fn the_session_phase_names_are_the_python_facing_vocabulary() {
        assert_eq!(SessionPhase::New.as_str(), "new");
        assert_eq!(SessionPhase::Started.as_str(), "started");
        assert_eq!(SessionPhase::Running.as_str(), "running");
        assert_eq!(SessionPhase::Closed.as_str(), "closed");
    }

    #[test]
    fn a_driver_walks_registered_enabled_running() {
        let mut host = host();
        let id = register(&mut host, "backrun");
        assert_eq!(host.state_of(&id), Some(DriverPose::Registered));
        assert_eq!(host.enable(&id), Ok(DriverPose::Enabled));
        assert_eq!(host.start(&id), Ok(DriverPose::Running));
    }

    #[test]
    fn enabling_an_unknown_strategy_errors_loudly() {
        let mut host = host();
        let err = host.enable(&sid("ghost")).unwrap_err();
        assert_eq!(err, HostError::UnknownStrategy(sid("ghost")));
        assert!(err.to_string().contains("unknown strategy"));
    }

    #[test]
    fn enabling_an_unconfigured_strategy_errors_loudly() {
        let mut host = host();
        let id = sid("backrun");
        host.register(id.clone(), FacetStatus::Unconfigured)
            .expect("register");
        let err = host.enable(&id).unwrap_err();
        assert_eq!(err, HostError::UnconfiguredStrategy(id));
        assert!(err.to_string().contains("unconfigured"));
    }

    #[test]
    fn registering_the_same_name_twice_is_refused() {
        let mut host = host();
        register(&mut host, "backrun");
        assert_eq!(
            host.register(sid("backrun"), FacetStatus::Configured),
            Err(HostError::AlreadyRegistered(sid("backrun")))
        );
    }

    #[test]
    fn invalid_lifecycle_moves_are_typed_declines() {
        let mut host = host();
        let id = register(&mut host, "backrun");
        // start before enable
        assert_eq!(
            host.start(&id),
            Err(HostError::Transition {
                id: id.clone(),
                decline: FsmDecline::RunningRequiresEnabled,
            })
        );
        // halt before running
        assert_eq!(
            host.halt(&id, "nope"),
            Err(HostError::Transition {
                id: id.clone(),
                decline: FsmDecline::HaltRequiresRunning,
            })
        );
        host.enable(&id).expect("enable");
        // enable twice
        assert_eq!(
            host.enable(&id),
            Err(HostError::Transition {
                id: id.clone(),
                decline: FsmDecline::EnableRequiresRegistered,
            })
        );
    }

    #[test]
    fn a_halt_is_a_frozen_tombstone() {
        let mut host = host();
        let id = register(&mut host, "settlement");
        host.enable(&id).expect("enable");
        host.start(&id).expect("start");
        host.halt(&id, "solver invariant").expect("halt");
        assert_eq!(host.state_of(&id), Some(DriverPose::Halted));
        let record = host.record(&id).expect("record");
        assert_eq!(record.halt_detail(), Some("solver invariant"));
        // A tombstone never restarts and never disables.
        assert_eq!(
            host.enable(&id),
            Err(HostError::Transition {
                id: id.clone(),
                decline: FsmDecline::EnableRequiresRegistered,
            })
        );
        assert_eq!(
            host.disable(&id),
            Err(HostError::Transition {
                id,
                decline: FsmDecline::DisableRejectsTerminal,
            })
        );
    }

    #[test]
    fn disable_is_terminal() {
        let mut host = host();
        let id = register(&mut host, "backrun");
        host.disable(&id).expect("disable");
        assert_eq!(host.state_of(&id), Some(DriverPose::Disabled));
        assert_eq!(
            host.disable(&id),
            Err(HostError::Transition {
                id: id.clone(),
                decline: FsmDecline::DisableRejectsTerminal,
            })
        );
        assert!(host.enable(&id).is_err());
    }

    #[test]
    fn halt_releases_the_strategys_nonce_reservations() {
        let mut host = host();
        let id = register(&mut host, "backrun");
        host.enable(&id).expect("enable");
        host.start(&id).expect("start");
        let lease = host.nonce().lease(&id).expect("lease");
        host.nonce().record_broadcast(&lease).expect("broadcast");
        assert_eq!(host.nonce().outstanding_nonces(), vec![7]);
        host.halt(&id, "local violation").expect("halt");
        assert!(host.nonce().outstanding_nonces().is_empty());
    }

    #[test]
    fn disable_releases_the_strategys_nonce_reservations() {
        let mut host = host();
        let id = register(&mut host, "settlement");
        host.enable(&id).expect("enable");
        let lease = host.nonce().lease(&id).expect("lease");
        host.nonce().record_broadcast(&lease).expect("broadcast");
        assert_eq!(host.nonce().outstanding_nonces(), vec![7]);
        host.disable(&id).expect("disable");
        assert!(host.nonce().outstanding_nonces().is_empty());
    }

    #[test]
    fn the_host_lends_out_the_shared_handles() {
        let hub = Arc::new(Hub::new());
        let registry = Arc::new(RouteRegistry::new(V2ConnectorIndex::default()));
        let nonce = Arc::new(NonceAuthority::new(3));
        let host = StrategyHost::new(Arc::clone(&hub), Arc::clone(&registry), Arc::clone(&nonce));
        assert!(Arc::ptr_eq(host.hub(), &hub));
        assert!(Arc::ptr_eq(host.registry(), &registry));
        assert!(Arc::ptr_eq(host.nonce(), &nonce));
    }

    #[test]
    fn list_reports_every_driver_in_registration_order() {
        let mut host = host();
        register(&mut host, "settlement");
        register(&mut host, "backrun");
        let records = host.list();
        let names: Vec<&str> = records.iter().map(|r| r.id().as_str()).collect();
        assert_eq!(names, vec!["settlement", "backrun"]);
        assert!(host
            .list()
            .iter()
            .all(|r| r.state() == DriverPose::Registered));
    }

    #[test]
    fn lane_namespace_is_refused_until_the_driver_is_enabled() {
        let mut host = host();
        host.set_state_root("/var/state");
        let id = register(&mut host, "backrun");
        assert_eq!(
            host.lane_namespace(&id),
            Err(HostError::StrategyNotEnabled(id.clone())),
            "a registered-but-never-enabled driver owns no lane"
        );
        host.enable(&id).expect("enable");
        let lane = host.lane_namespace(&id).expect("lane");
        assert_eq!(lane.root(), Path::new("/var/state/backrun"));
        assert_eq!(
            lane.session_dir(),
            PathBuf::from("/var/state/backrun/session")
        );
        assert_eq!(
            lane.quarantine_dir(),
            PathBuf::from("/var/state/backrun/quarantine")
        );
    }

    #[test]
    fn lane_namespace_needs_an_installed_state_root() {
        let mut host = host();
        let id = register(&mut host, "backrun");
        host.enable(&id).expect("enable");
        assert_eq!(host.lane_namespace(&id), Err(HostError::StateRootUnset));
    }

    #[test]
    fn a_disabled_driver_owns_no_lane() {
        let mut host = host();
        host.set_state_root("/var/state");
        let id = register(&mut host, "backrun");
        host.enable(&id).expect("enable");
        host.disable(&id).expect("disable");
        assert_eq!(
            host.lane_namespace(&id),
            Err(HostError::StrategyNotEnabled(id))
        );
    }

    #[tokio::test]
    async fn start_driving_boots_enabled_spawns_and_tombstones_a_halt() {
        let mut host = host();
        let id = register(&mut host, "backrun");
        let (exit_tx, exit_rx) = tokio::sync::oneshot::channel::<DriverExit>();
        let spawn: DriverSpawnFactory = Box::new(move |_lane| {
            Box::pin(async move { exit_rx.await.unwrap_or(DriverExit::Stopped) })
        });
        host.attach_spawn(&id, spawn).expect("attach spawn");
        host.enable(&id).expect("enable");

        let tasks = host.start_driving().expect("start driving");
        assert_eq!(tasks.len(), 1);
        assert_eq!(host.state_of(&id), Some(DriverPose::Running));

        exit_tx
            .send(DriverExit::Halted("lane-local violation".to_string()))
            .expect("send exit");
        let task = tasks.into_iter().next().expect("task");
        assert_eq!(task.id(), &id);
        let exit = task.wait().await;
        host.record_driver_exit(&id, exit).expect("record exit");
        let record = host.record(&id).expect("record");
        assert_eq!(record.state(), DriverPose::Halted);
        assert_eq!(record.halt_detail(), Some("lane-local violation"));
        assert!(host.enable(&id).is_err(), "a tombstone never restarts");
    }

    #[tokio::test]
    async fn start_driving_hands_the_lane_namespace_to_the_factory() {
        let mut host = host();
        host.set_state_root("/var/state");
        let id = register(&mut host, "backrun");
        let seen: Arc<std::sync::Mutex<Option<PathBuf>>> = Arc::new(std::sync::Mutex::new(None));
        let writer = Arc::clone(&seen);
        let spawn: DriverSpawnFactory = Box::new(move |lane| {
            *writer.lock().expect("lane slot") =
                lane.map(|namespace| namespace.root().to_path_buf());
            Box::pin(async { DriverExit::Stopped })
        });
        host.attach_spawn(&id, spawn).expect("attach spawn");
        host.enable(&id).expect("enable");

        let tasks = host.start_driving().expect("start driving");
        assert_eq!(tasks.len(), 1);
        let task = tasks.into_iter().next().expect("task");
        assert_eq!(task.wait().await, DriverExit::Stopped);
        assert_eq!(
            seen.lock().expect("lane slot").as_deref(),
            Some(Path::new("/var/state/backrun")),
            "the factory is handed the driver's lane namespace"
        );
    }

    /// A hosted lane must observe the multi-thread ambient runtime the
    /// dedicated process entrypoint provides: the evm2 sync DB bridge
    /// (`BlockingAsyncDb::new`, the layer `Evm2BlockSimHandle::build` stacks)
    /// returns
    /// `None` under a current-thread runtime, so a dedicated current-thread lane
    /// runtime leaves every frame with `replay_unavailable`.
    #[tokio::test]
    async fn start_driving_boots_the_lane_under_the_multi_thread_ambient_runtime() {
        use tokio::runtime::RuntimeFlavor;

        let mut host = host();
        let id = register(&mut host, "backrun");
        let seen: Arc<std::sync::Mutex<Option<RuntimeFlavor>>> =
            Arc::new(std::sync::Mutex::new(None));
        let writer = Arc::clone(&seen);
        let spawn: DriverSpawnFactory = Box::new(move |_lane| {
            Box::pin(async move {
                *writer.lock().expect("flavor slot") = Some(
                    tokio::runtime::Handle::try_current()
                        .expect("lane runtime")
                        .runtime_flavor(),
                );
                DriverExit::Stopped
            })
        });
        host.attach_spawn(&id, spawn).expect("attach spawn");
        host.enable(&id).expect("enable");

        let tasks = host.start_driving().expect("start driving");
        let task = tasks.into_iter().next().expect("task");
        assert_eq!(task.wait().await, DriverExit::Stopped);
        assert_eq!(
            *seen.lock().expect("flavor slot"),
            Some(RuntimeFlavor::MultiThread),
            "a hosted lane must run under the multi-thread ambient runtime"
        );
    }

    #[tokio::test]
    async fn a_clean_driver_exit_moves_running_to_stopped() {
        let mut host = host();
        let id = register(&mut host, "backrun");
        host.attach_spawn(
            &id,
            Box::new(|_lane| Box::pin(async { DriverExit::Stopped })),
        )
        .expect("attach spawn");
        host.enable(&id).expect("enable");

        let tasks = host.start_driving().expect("start driving");
        assert_eq!(host.state_of(&id), Some(DriverPose::Running));
        let task = tasks.into_iter().next().expect("task");
        host.drive_and_fold(task)
            .await
            .expect("the host owns the fold");

        assert_eq!(
            host.state_of(&id),
            Some(DriverPose::Stopped),
            "a loop that returned is never reported as Running"
        );
        host.disable(&id)
            .expect("a stopped record is still disableable");
        assert_eq!(host.state_of(&id), Some(DriverPose::Disabled));
    }

    #[tokio::test]
    async fn the_supervisor_folds_a_clean_exit_through_the_host() {
        let shared = Arc::new(parking_lot::Mutex::new(host()));
        let id = {
            let mut guard = shared.lock();
            let id = register(&mut guard, "backrun");
            guard
                .attach_spawn(
                    &id,
                    Box::new(|_lane| Box::pin(async { DriverExit::Stopped })),
                )
                .expect("attach spawn");
            guard.enable(&id).expect("enable");
            id
        };

        let tasks = shared.lock().start_driving().expect("start driving");
        let supervisor = HostSupervisor::new(Arc::clone(&shared));
        supervisor.supervise(tasks);
        assert_eq!(supervisor.live_count(), 1);

        let mut state = shared.lock().state_of(&id);
        for _ in 0..1_000 {
            if state == Some(DriverPose::Stopped) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            state = shared.lock().state_of(&id);
        }
        assert_eq!(
            state,
            Some(DriverPose::Stopped),
            "the supervisor folds the clean exit; the record is never left Running"
        );
    }

    /// An enabled record with no loop behind it is a miswired boot: the start
    /// boundary refuses it, and the refusal leaves the record untouched (no
    /// half-started fleet, no `Running` claim over a loop nobody wired).
    #[test]
    fn start_driving_refuses_an_enabled_driver_with_no_spawn() {
        let mut host = host();
        let id = register(&mut host, "backrun");
        host.enable(&id).expect("enable");
        assert_eq!(
            host.start_driving().map(|tasks| tasks.len()),
            Err(HostError::MissingSpawn(id.clone()))
        );
        assert_eq!(host.state_of(&id), Some(DriverPose::Enabled));
    }

    /// A factory is once-only provisioning: the second attach is refused and
    /// the first factory is the one the start boundary boots.
    #[tokio::test]
    async fn a_second_spawn_attach_is_refused_and_the_first_factory_boots() {
        let mut host = host();
        let id = register(&mut host, "backrun");
        let booted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = Arc::clone(&booted);
        host.attach_spawn(
            &id,
            Box::new(move |_lane| {
                let observed = Arc::clone(&observed);
                Box::pin(async move {
                    observed.store(true, std::sync::atomic::Ordering::SeqCst);
                    DriverExit::Halted("first factory ran".to_string())
                })
            }),
        )
        .expect("attach the first factory");
        let refused_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let refused = Arc::clone(&refused_ran);
        assert_eq!(
            host.attach_spawn(
                &id,
                Box::new(move |_lane| {
                    let refused = Arc::clone(&refused);
                    Box::pin(async move {
                        refused.store(true, std::sync::atomic::Ordering::SeqCst);
                        DriverExit::Stopped
                    })
                }),
            ),
            Err(HostError::Spawn {
                id: id.clone(),
                decline: SpawnDecline::AlreadyAttached,
            }),
            "the second attach is a typed refusal, never a silent replacement"
        );
        host.enable(&id).expect("enable");

        let tasks = host.start_driving().expect("start driving");
        let task = tasks.into_iter().next().expect("one driver task");
        assert_eq!(
            task.wait().await,
            DriverExit::Halted("first factory ran".to_string()),
            "the factory the record promised is the one that boots"
        );
        assert!(booted.load(std::sync::atomic::Ordering::SeqCst));
        assert!(
            !refused_ran.load(std::sync::atomic::Ordering::SeqCst),
            "the refused factory never reaches a thread"
        );
    }

    /// Provisioning stays legal after the enable move: a boot may wire a driver
    /// the operator already admitted, which is the pose a runtime start finds.
    #[test]
    fn a_pre_start_record_takes_its_factory_in_either_pre_start_pose() {
        let mut host = host();
        let early = register(&mut host, "early");
        host.attach_spawn(&early, a_factory())
            .expect("attach registered");
        assert!(host.has_spawn(&early));

        let late = register(&mut host, "late");
        host.enable(&late).expect("enable");
        host.attach_spawn(&late, a_factory())
            .expect("attach enabled");
        assert!(
            host.has_spawn(&late),
            "an enabled record still holds its provisioning slot"
        );
    }

    /// A record past the pre-start window holds no slot, so the attach is
    /// refused rather than leaking a factory nothing would ever boot.
    #[tokio::test]
    async fn a_booted_or_terminal_record_cannot_take_a_factory() {
        let mut host = host();
        let id = register(&mut host, "backrun");
        host.attach_spawn(&id, a_factory()).expect("attach");
        host.enable(&id).expect("enable");
        let tasks = host.start_driving().expect("start driving");
        assert!(
            !host.has_spawn(&id),
            "the start boundary consumed the factory"
        );
        assert_eq!(
            host.attach_spawn(&id, a_factory()),
            Err(HostError::Spawn {
                id: id.clone(),
                decline: SpawnDecline::PastProvisioning,
            })
        );
        let task = tasks.into_iter().next().expect("one driver task");
        host.drive_and_fold(task).await.expect("fold the stop");

        let peer = register(&mut host, "peer");
        host.attach_spawn(&peer, a_factory()).expect("attach");
        host.enable(&peer).expect("enable");
        host.disable(&peer).expect("disable");
        assert!(
            !host.has_spawn(&peer),
            "a terminal record releases its slot"
        );
        assert_eq!(
            host.attach_spawn(&peer, a_factory()),
            Err(HostError::Spawn {
                id: peer,
                decline: SpawnDecline::PastProvisioning,
            })
        );
    }

    /// The fleet pre-flight is atomic: one enabled record with no loop refuses
    /// the whole start, so no wired driver's loop boots and every record keeps
    /// its provisioning.
    #[tokio::test]
    async fn the_start_boundary_refuses_the_whole_fleet_before_booting_a_loop() {
        let mut host = host();
        let wired = register(&mut host, "wired");
        let unwired = register(&mut host, "unwired");
        let booted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = Arc::clone(&booted);
        host.attach_spawn(
            &wired,
            Box::new(move |_lane| {
                let observed = Arc::clone(&observed);
                Box::pin(async move {
                    observed.store(true, std::sync::atomic::Ordering::SeqCst);
                    DriverExit::Stopped
                })
            }),
        )
        .expect("attach spawn");
        host.enable(&wired).expect("enable");
        host.enable(&unwired).expect("enable");

        assert_eq!(
            host.start_driving().map(|tasks| tasks.len()),
            Err(HostError::MissingSpawn(unwired.clone()))
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !booted.load(std::sync::atomic::Ordering::SeqCst),
            "the refusal lands before any loop boots"
        );
        assert_eq!(host.state_of(&wired), Some(DriverPose::Enabled));
        assert!(
            host.has_spawn(&wired),
            "the refused start freed nothing: the wired record keeps its factory"
        );

        host.attach_spawn(&unwired, a_factory())
            .expect("provision the missing record");
        let tasks = host.start_driving().expect("start driving");
        assert_eq!(tasks.len(), 2, "the completed fleet boots whole");
        assert_eq!(host.state_of(&wired), Some(DriverPose::Running));
        assert_eq!(host.state_of(&unwired), Some(DriverPose::Running));
    }

    /// A lane that panics is a self-halt: the supervisor folds the poisoned task
    /// into a tombstone carrying the panic, so a dead loop is never left
    /// `Running`.
    #[tokio::test]
    async fn the_supervisor_folds_a_panicked_lane_into_a_tombstone() {
        let shared = Arc::new(parking_lot::Mutex::new(host()));
        let id = {
            let mut guard = shared.lock();
            let id = register(&mut guard, "backrun");
            guard
                .attach_spawn(
                    &id,
                    Box::new(|_lane| {
                        Box::pin(async {
                            panic!("lane-local explosion");
                        })
                    }),
                )
                .expect("attach spawn");
            guard.enable(&id).expect("enable");
            id
        };

        let tasks = shared.lock().start_driving().expect("start driving");
        assert_eq!(tasks.len(), 1);
        let supervisor = HostSupervisor::new(Arc::clone(&shared));
        supervisor.supervise(tasks);

        let mut record = shared.lock().record(&id);
        for _ in 0..1_000 {
            if record
                .as_ref()
                .is_some_and(|record| record.state() == DriverPose::Halted)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            record = shared.lock().record(&id);
        }
        let record = record.expect("the panicked lane folded into a record");
        assert_eq!(
            record.state(),
            DriverPose::Halted,
            "a panicked loop is a tombstone, never a Running claim"
        );
        assert!(
            record
                .halt_detail()
                .is_some_and(|detail| detail.contains("driver task failed")),
            "the tombstone names the failed task: {:?}",
            record.halt_detail()
        );
    }

    /// A tombstone carries a cause on every path that reaches it, and no other
    /// pose carries one.
    #[tokio::test]
    async fn a_halt_always_carries_its_cause() {
        let mut host = host();
        let id = register(&mut host, "backrun");
        assert!(host.record(&id).expect("record").halt_detail().is_none());
        host.enable(&id).expect("enable");
        host.attach_spawn(
            &id,
            Box::new(|_lane| {
                Box::pin(async { DriverExit::Halted("lane-local violation".to_string()) })
            }),
        )
        .expect("attach spawn");
        let tasks = host.start_driving().expect("start driving");
        let task = tasks.into_iter().next().expect("one driver task");
        host.drive_and_fold(task).await.expect("fold the halt");
        let record = host.record(&id).expect("record");
        assert_eq!(record.state(), DriverPose::Halted);
        assert_eq!(
            record.halt_detail(),
            Some("lane-local violation"),
            "the tombstone carries the cause the loop reported"
        );
    }

    /// A reconciler that reports a fixed notice set, so the host's delivery
    /// path can be exercised without the submission crate.
    struct FakeReconciler {
        notices: Vec<HeadNotice>,
        outstanding: bool,
    }

    impl HeadReconciler for FakeReconciler {
        fn reconcile_head(&self, _confirmed: u64, _outstanding: &[u64]) -> Vec<HeadNotice> {
            self.notices.clone()
        }

        fn has_outstanding(&self) -> bool {
            self.outstanding
        }
    }

    #[test]
    fn a_head_update_delivers_each_notice_only_to_its_owner() {
        let mut host = host();
        let settlement = register(&mut host, "settlement");
        let backrun = register(&mut host, "backrun");
        let (settlement_tx, mut settlement_rx) = tokio::sync::mpsc::unbounded_channel();
        let (backrun_tx, mut backrun_rx) = tokio::sync::mpsc::unbounded_channel();
        host.subscribe_head(&settlement, settlement_tx)
            .expect("subscribe settlement");
        host.subscribe_head(&backrun, backrun_tx)
            .expect("subscribe backrun");
        host.attach_reconciler(Arc::new(FakeReconciler {
            notices: vec![HeadNotice::new(
                settlement.clone(),
                7,
                StrategyNotice::Orphaned { fillable_nonce: 6 },
            )],
            outstanding: false,
        }));

        let notices = host.on_head(7);

        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].strategy(), &settlement);
        let delivered = settlement_rx.try_recv().expect("settlement notice");
        assert_eq!(delivered, notices[0]);
        assert!(
            backrun_rx.try_recv().is_err(),
            "the other strategy receives nothing"
        );
    }

    #[test]
    fn a_head_update_refreshes_the_authority_reorg_safely() {
        let mut host = host();
        let id = register(&mut host, "backrun");
        host.enable(&id).expect("enable");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        host.subscribe_head(&id, tx).expect("subscribe");

        let lease = host.nonce().lease(&id).expect("lease");
        assert_eq!(lease.nonce(), 7);
        assert!(host.on_head(8).is_empty(), "a forward head lands nothing");

        let notices = host.on_head(7);

        assert_eq!(
            notices,
            vec![HeadNotice::new(
                id.clone(),
                7,
                StrategyNotice::LeaseRevoked { nonce: 7 },
            )]
        );
        assert!(
            host.nonce().lease_of(&id).is_none(),
            "the rewind revoked the lease stamped inside the rewound window"
        );
        assert_eq!(rx.try_recv().expect("delivered"), notices[0]);
    }

    #[test]
    fn hosted_activity_is_false_until_a_lease_or_record_exists() {
        let mut host = host();
        assert!(!host.has_hosted_activity(), "a fresh host has no activity");
        let id = register(&mut host, "settlement");
        host.enable(&id).expect("enable");
        assert!(
            !host.has_hosted_activity(),
            "registration alone is not hosted activity"
        );
        let lease = host.nonce().lease(&id).expect("lease");
        assert!(host.has_hosted_activity(), "a lease is hosted activity");
        host.nonce().record_broadcast(&lease).expect("broadcast");
        assert!(host.has_hosted_activity(), "a broadcast is hosted activity");
    }

    #[test]
    fn hosted_activity_sees_a_reconciler_with_non_terminal_records() {
        let mut host = host();
        assert!(!host.has_hosted_activity());
        host.attach_reconciler(Arc::new(FakeReconciler {
            notices: Vec::new(),
            outstanding: true,
        }));
        assert!(
            host.has_hosted_activity(),
            "a non-terminal submission record is hosted activity"
        );
    }

    /// One head update moves broadcast occupants into their retained landed
    /// form; a rewind restores exactly the rewound window and leaves the rest
    /// retained. A revival is no notice — a caller diffs the occupancy.
    #[test]
    fn a_head_update_retains_occupancy_and_rewinds_exactly_the_window() {
        use degenbot_substrate::nonce::NonceOccupant;

        let mut host = host();
        let id = register(&mut host, "backrun");
        host.enable(&id).expect("enable");
        for _ in 0..2 {
            let lease = host.nonce().lease(&id).expect("lease");
            host.nonce().record_broadcast(&lease).expect("broadcast");
        }

        assert!(
            host.on_head(9).is_empty(),
            "a forward head folds no notices"
        );
        assert_eq!(
            host.nonce().occupancy(),
            vec![
                (
                    7,
                    NonceOccupant::Landed {
                        strategy: id.clone()
                    }
                ),
                (
                    8,
                    NonceOccupant::Landed {
                        strategy: id.clone()
                    }
                ),
            ],
            "confirmation retains both occupants"
        );

        assert!(
            host.on_head(8).is_empty(),
            "a revival is no notice: a caller diffs the occupancy"
        );
        assert_eq!(
            host.nonce().occupancy(),
            vec![
                (
                    7,
                    NonceOccupant::Landed {
                        strategy: id.clone()
                    }
                ),
                (
                    8,
                    NonceOccupant::Broadcast {
                        strategy: id.clone()
                    }
                ),
            ],
            "the rewind restores exactly the rewound window [8, 9)"
        );
        assert_eq!(host.nonce().outstanding_nonces(), vec![8]);
    }

    #[test]
    fn subscribing_an_unknown_strategy_is_refused() {
        let host = host();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert_eq!(
            host.subscribe_head(&sid("ghost"), tx),
            Err(HostError::UnknownStrategy(sid("ghost")))
        );
    }
}
