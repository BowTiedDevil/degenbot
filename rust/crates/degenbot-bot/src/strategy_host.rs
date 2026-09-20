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
//! The host owns the shared handles and lends them out; it deliberately starts
//! no driver loop in this module, so the host's only job is admission and
//! lifecycle. Enabling a name the host never registered, or a name registered
//! without a configured facet, fails loudly.
//!
//! A driver that owns process-lifetime artifacts is lent a [`LaneNamespace`]
//! under the host state root, and a driver that runs a loop registers a spawn
//! factory the host boots at [`StrategyHost::start_driving`]. The host itself
//! never boots a loop inline. A strategy whose loop is already driven by the
//! engine's own pump (the settlement arm) registers no factory: it is not
//! host-driven, so `start_driving` skips it rather than failing it.

use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use degenbot_eventhub::Hub;
use indexmap::IndexMap;
use tokio::sync::mpsc::UnboundedSender;

use crate::bot_core::route_registry::RouteRegistry;
use crate::nonce_authority::{NonceAuthority, StrategyId};

/// The lifecycle state of one registered driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverState {
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

impl DriverState {
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
    /// `enable` is only legal from [`DriverState::Registered`].
    #[error("enable requires the Registered state")]
    EnableRequiresRegistered,
    /// `start` is only legal from [`DriverState::Enabled`].
    #[error("running requires the Enabled state")]
    RunningRequiresEnabled,
    /// `halt` is only legal from [`DriverState::Running`].
    #[error("halt requires the Running state")]
    HaltRequiresRunning,
    /// `stop` is only legal from [`DriverState::Running`].
    #[error("stop requires the Running state")]
    StopRequiresRunning,
    /// `disable` refuses the terminal states: a tombstone is frozen.
    #[error("disable rejects terminal states (Halted/Disabled are frozen)")]
    DisableRejectsTerminal,
}

impl DriverState {
    /// The operator enable move: [`DriverState::Registered`] -> [`DriverState::Enabled`].
    ///
    /// # Errors
    ///
    /// [`FsmDecline::EnableRequiresRegistered`] from any other state.
    pub fn on_enable(self) -> Result<Self, FsmDecline> {
        if self == Self::Registered {
            Ok(Self::Enabled)
        } else {
            Err(FsmDecline::EnableRequiresRegistered)
        }
    }

    /// The driver's own start move: [`DriverState::Enabled`] -> [`DriverState::Running`].
    ///
    /// # Errors
    ///
    /// [`FsmDecline::RunningRequiresEnabled`] from any other state.
    pub fn on_start(self) -> Result<Self, FsmDecline> {
        if self == Self::Enabled {
            Ok(Self::Running)
        } else {
            Err(FsmDecline::RunningRequiresEnabled)
        }
    }

    /// The driver's own halt move: [`DriverState::Running`] -> [`DriverState::Halted`].
    ///
    /// # Errors
    ///
    /// [`FsmDecline::HaltRequiresRunning`] from any other state.
    pub fn on_halt(self) -> Result<Self, FsmDecline> {
        if self == Self::Running {
            Ok(Self::Halted)
        } else {
            Err(FsmDecline::HaltRequiresRunning)
        }
    }

    /// The driver's own clean-stop move: [`DriverState::Running`] ->
    /// [`DriverState::Stopped`].
    ///
    /// # Errors
    ///
    /// [`FsmDecline::StopRequiresRunning`] from any other state.
    pub fn on_stop(self) -> Result<Self, FsmDecline> {
        if self == Self::Running {
            Ok(Self::Stopped)
        } else {
            Err(FsmDecline::StopRequiresRunning)
        }
    }

    /// The operator disable move: any non-terminal state -> [`DriverState::Disabled`].
    ///
    /// # Errors
    ///
    /// [`FsmDecline::DisableRejectsTerminal`] from Halted or Disabled.
    pub fn on_disable(self) -> Result<Self, FsmDecline> {
        if self.is_terminal() {
            Err(FsmDecline::DisableRejectsTerminal)
        } else {
            Ok(Self::Disabled)
        }
    }
}

/// The cockpit session's operator-facing lifecycle: the four-pose shell the
/// Python `BotRunner` presents over the host-owned engine.
///
/// The host owns one operator lifecycle per driver ([`DriverState`]); the
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

/// The host's queryable record for one driver FSM instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverRecord {
    id: StrategyId,
    facet: FacetStatus,
    state: DriverState,
    halt_detail: Option<String>,
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
    pub fn state(&self) -> DriverState {
        self.state
    }

    /// The halt cause, present exactly on a halted tombstone.
    #[must_use]
    pub fn halt_detail(&self) -> Option<&str> {
        self.halt_detail.as_deref()
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
}

/// A lane's run-artifact namespace under the host state root.
///
/// A strategy that owns process-lifetime artifacts writes them under its own
/// name so two drivers on one host cannot collide on a shared path. The value
/// carries only the root; each consumer appends its own file names, so the
/// host stays free of the submission crate's file vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneNamespace {
    root: PathBuf,
}

impl LaneNamespace {
    /// The namespace `<state_root>/<id>`.
    #[must_use]
    pub fn under(state_root: impl Into<PathBuf>, id: &StrategyId) -> Self {
        Self {
            root: state_root.into().join(id.as_str()),
        }
    }

    /// The lane's root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The lane's session directory.
    #[must_use]
    pub fn session_dir(&self) -> PathBuf {
        self.root.join("session")
    }

    /// The lane's quarantine directory.
    #[must_use]
    pub fn quarantine_dir(&self) -> PathBuf {
        self.root.join("quarantine")
    }
}

/// Why a driver's loop returned, in the host's vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverExit {
    /// The loop stopped on request; no tombstone.
    Stopped,
    /// The loop halted itself; the cause becomes the tombstone detail.
    Halted(String),
}

/// A driver's loop future, minted by its spawn factory.
///
/// Not required to be `Send`: a lane whose replay stack is single-threaded (the
/// backrun lane's `Rc`-backed buffers) is polled inline on one blocking thread
/// using the ambient multi-thread runtime by [`StrategyHost::start_driving`],
/// while the factory itself must be `Send` so it can travel to that thread.
/// The ambient multi-thread runtime is what `revm`'s `WrapDatabaseAsync::new`
/// requires; a dedicated current-thread runtime captures no handle and forbids
/// block-in-place, failing every `BlockSimHandle::build`.
pub type DriverFuture = Pin<Box<dyn Future<Output = DriverExit> + 'static>>;

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
    /// A reorg rewind revoked the strategy's live lease for `nonce`; the lease
    /// must be re-obtained at the recovered head.
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

/// The once-only factory that boots a driver's loop.
///
/// The host hands the factory the lane namespace for the driver it is starting
/// (`None` when no state root is installed), so a lane that writes
/// run-artifacts under its own name learns its scope at the driving edge and a
/// lane that keeps the process-global root is told so explicitly.
pub type DriverSpawnFactory = Box<dyn FnOnce(Option<LaneNamespace>) -> DriverFuture + Send>;

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
    drivers: IndexMap<StrategyId, DriverRecord>,
    spawns: IndexMap<StrategyId, DriverSpawnFactory>,
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

/// A host-minted hub paired with the source-channel handles registered during
/// its exclusive construction window.
///
/// [`StrategyHost::mint`] builds the pair together; the driver consumes both,
/// so the hub and its channels travel as one value. The host cannot inspect a
/// caller's mint closure, so the pairing rests on that closure being the
/// channel set's sole registrar (for the settlement engine,
/// `EngineChannelHandles::register_on`).
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

    /// Split the pair into its hub and the caller's registered handles.
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
            spawns: IndexMap::new(),
            state_root: None,
            reconciler: None,
            head_sinks: parking_lot::Mutex::new(IndexMap::new()),
        }
    }

    /// Mint the process hub with caller-registered source channels, then own
    /// it.
    ///
    /// Named source channels are minted through `&mut Hub`, so registration
    /// must finish before the hub is shared; this constructor is the
    /// host-owned window that provides the exclusive `&mut`. `register` returns
    /// whatever channel handles the caller needs (for the settlement engine,
    /// its producers); the host itself stays generic and never names them. The
    /// returned [`HostHub`] carries the hub and those handles as one pair, so
    /// a driver consumes them together. `register` must register onto the hub
    /// it is handed — it is the sole registrar for that channel set, since the
    /// host cannot inspect what the closure did.
    #[must_use]
    pub fn mint<T>(
        registry: Arc<RouteRegistry>,
        nonce: Arc<NonceAuthority>,
        register: impl FnOnce(&mut Hub) -> T,
    ) -> (Self, HostHub<T>) {
        let mut hub = Hub::new();
        let attachment = register(&mut hub);
        let hub = Arc::new(hub);
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
    /// The per-head feed gates its one chain-nonce read on this, so a
    /// settlement-only boot with no hosted activity stays byte-identical.
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

    /// Admit a strategy as a driver FSM instance in [`DriverState::Registered`].
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
            id.clone(),
            DriverRecord {
                id,
                facet,
                state: DriverState::Registered,
                halt_detail: None,
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
    pub fn enable(&mut self, id: &StrategyId) -> Result<DriverState, HostError> {
        let record = self.record_mut(id)?;
        if record.facet == FacetStatus::Unconfigured {
            return Err(HostError::UnconfiguredStrategy(id.clone()));
        }
        record.state = record
            .state
            .on_enable()
            .map_err(|decline| HostError::Transition {
                id: id.clone(),
                decline,
            })?;
        Ok(record.state)
    }

    /// Move an enabled driver to running. Called by the driver once its loop is
    /// live, never by the operator.
    ///
    /// # Errors
    ///
    /// [`HostError::UnknownStrategy`] / [`HostError::Transition`].
    pub fn start(&mut self, id: &StrategyId) -> Result<DriverState, HostError> {
        let record = self.record_mut(id)?;
        record.state = record
            .state
            .on_start()
            .map_err(|decline| HostError::Transition {
                id: id.clone(),
                decline,
            })?;
        Ok(record.state)
    }

    /// Tombstone a running driver with the cause its loop reported, releasing
    /// every nonce reservation the strategy still holds.
    ///
    /// # Errors
    ///
    /// [`HostError::UnknownStrategy`] / [`HostError::Transition`].
    pub fn halt(&mut self, id: &StrategyId, detail: impl Into<String>) -> Result<(), HostError> {
        {
            let record = self.record_mut(id)?;
            record.state = record
                .state
                .on_halt()
                .map_err(|decline| HostError::Transition {
                    id: id.clone(),
                    decline,
                })?;
            record.halt_detail = Some(detail.into());
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
            let record = self.record_mut(id)?;
            record.state = record
                .state
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
        let record = self
            .drivers
            .get(id)
            .ok_or_else(|| HostError::UnknownStrategy(id.clone()))?;
        if matches!(
            record.state,
            DriverState::Registered | DriverState::Disabled
        ) {
            return Err(HostError::StrategyNotEnabled(id.clone()));
        }
        let root = self.state_root.clone().ok_or(HostError::StateRootUnset)?;
        Ok(LaneNamespace::under(root, id))
    }

    /// Attach the once-only spawn factory a driver's loop is booted from.
    ///
    /// # Errors
    ///
    /// [`HostError::UnknownStrategy`] for an unregistered name.
    pub fn attach_spawn(
        &mut self,
        id: &StrategyId,
        spawn: DriverSpawnFactory,
    ) -> Result<(), HostError> {
        self.record_mut(id)?;
        self.spawns.insert(id.clone(), spawn);
        Ok(())
    }

    /// Whether a driver has a spawn factory attached. A facet the engine's own
    /// pump drives (the settlement arm) deliberately has none.
    #[must_use]
    pub fn has_spawn(&self, id: &StrategyId) -> bool {
        self.spawns.contains_key(id)
    }

    /// Boot every enabled driver that registered a spawn factory, on the
    /// shared runtime, and move it to [`DriverState::Running`].
    ///
    /// This is the host's driving edge: the settlement pump arms on its own
    /// `resume`, while a host-managed driver's loop starts here. Each returned
    /// [`DriverTask`] is the lane boundary — the caller awaits it and feeds the
    /// exit back through [`Self::record_driver_exit`], so a lane panic becomes
    /// a tombstone rather than a host unwind.
    ///
    /// An enabled driver with no registered factory is skipped, not failed: the
    /// settlement facet registers none because the engine's pump arm already
    /// drives it, so only strategies that own a loop the host must start
    /// advance to [`DriverState::Running`] here.
    ///
    /// # Errors
    ///
    /// [`HostError::Transition`] if the FSM refuses a driver's start.
    pub fn start_driving(&mut self) -> Result<Vec<DriverTask>, HostError> {
        let runtime = degenbot_core::runtime::get_runtime();
        let ids: Vec<StrategyId> = self
            .drivers
            .iter()
            .filter(|(_, record)| record.state == DriverState::Enabled)
            .map(|(id, _)| id.clone())
            .collect();
        let mut tasks = Vec::new();
        for id in ids {
            let Some(spawn) = self.spawns.shift_remove(&id) else {
                continue;
            };
            let lane = self.lane_namespace(&id).ok();
            // A lane's replay stack is single-threaded, so the loop future is
            // `!Send` and must be polled inline on one thread, never spawned.
            // Drive it under the AMBIENT multi-thread runtime (via `block_on`
            // from a blocking thread) so the lane sees the same runtime
            // ambience as the standalone sidecar: `revm`'s
            // `WrapDatabaseAsync::new` captures the current handle only when
            // that runtime is multi-threaded, and its layer reads then use
            // `block_in_place`. A dedicated current-thread runtime captures no
            // handle and forbids block-in-place, so every
            // `BlockSimHandle::build` would fail.
            let handle = runtime.spawn_blocking(move || {
                let future = spawn(lane);
                runtime.block_on(future)
            });
            self.start(&id)?;
            tasks.push(DriverTask { id, handle });
        }
        Ok(tasks)
    }

    /// Fold a started driver's terminal exit into the FSM: a self-halt is a
    /// tombstone carrying the cause; a clean stop moves the record to
    /// [`DriverState::Stopped`] so a dead loop is never reported as
    /// [`DriverState::Running`].
    ///
    /// # Errors
    ///
    /// [`HostError::UnknownStrategy`] / [`HostError::Transition`] from the
    /// resulting lifecycle move.
    pub fn record_driver_exit(
        &mut self,
        id: &StrategyId,
        exit: DriverExit,
    ) -> Result<(), HostError> {
        match exit {
            DriverExit::Halted(detail) => self.halt(id, detail),
            DriverExit::Stopped => {
                let record = self.record_mut(id)?;
                record.state = record
                    .state
                    .on_stop()
                    .map_err(|decline| HostError::Transition {
                        id: id.clone(),
                        decline,
                    })?;
                Ok(())
            }
        }
    }

    /// Await one started driver's terminal exit and fold it into the FSM.
    ///
    /// This is the host's supervision edge: a caller that booted [`Self::start_driving`]
    /// hands each returned [`DriverTask`] here instead of re-implementing the
    /// await-then-[`Self::record_driver_exit`] ritual. The fold stays host-owned,
    /// so a clean stop is [`DriverState::Stopped`] and a self-halt is a tombstone.
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

    /// Every registered driver, in registration order.
    #[must_use]
    pub fn list(&self) -> Vec<DriverRecord> {
        self.drivers.values().cloned().collect()
    }

    /// One driver's record, or `None` if the name is unknown.
    #[must_use]
    pub fn record(&self, id: &StrategyId) -> Option<&DriverRecord> {
        self.drivers.get(id)
    }

    /// One driver's lifecycle state, or `None` if the name is unknown.
    #[must_use]
    pub fn state_of(&self, id: &StrategyId) -> Option<DriverState> {
        self.drivers.get(id).map(DriverRecord::state)
    }

    fn record_mut(&mut self, id: &StrategyId) -> Result<&mut DriverRecord, HostError> {
        self.drivers
            .get_mut(id)
            .ok_or_else(|| HostError::UnknownStrategy(id.clone()))
    }
}

impl fmt::Debug for StrategyHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StrategyHost")
            .field("drivers", &self.drivers)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "unit tests: a malformed fixture must fail the test loudly"
)]
mod tests {
    use super::*;
    use crate::sidecar_paths::V2ConnectorIndex;

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

    #[test]
    fn the_fsm_transition_table_is_total_and_closed() {
        let states = [
            DriverState::Registered,
            DriverState::Enabled,
            DriverState::Running,
            DriverState::Stopped,
            DriverState::Halted,
            DriverState::Disabled,
        ];
        for state in states {
            if state == DriverState::Registered {
                assert_eq!(state.on_enable(), Ok(DriverState::Enabled));
            } else {
                assert_eq!(state.on_enable(), Err(FsmDecline::EnableRequiresRegistered));
            }

            if state == DriverState::Enabled {
                assert_eq!(state.on_start(), Ok(DriverState::Running));
            } else {
                assert_eq!(state.on_start(), Err(FsmDecline::RunningRequiresEnabled));
            }

            if state == DriverState::Running {
                assert_eq!(state.on_halt(), Ok(DriverState::Halted));
                assert_eq!(state.on_stop(), Ok(DriverState::Stopped));
            } else {
                assert_eq!(state.on_halt(), Err(FsmDecline::HaltRequiresRunning));
                assert_eq!(state.on_stop(), Err(FsmDecline::StopRequiresRunning));
            }

            if state.is_terminal() {
                assert_eq!(state.on_disable(), Err(FsmDecline::DisableRejectsTerminal));
            } else {
                assert_eq!(state.on_disable(), Ok(DriverState::Disabled));
            }
        }
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
        assert_eq!(host.state_of(&id), Some(DriverState::Registered));
        assert_eq!(host.enable(&id), Ok(DriverState::Enabled));
        assert_eq!(host.start(&id), Ok(DriverState::Running));
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
        assert_eq!(host.state_of(&id), Some(DriverState::Halted));
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
        assert_eq!(host.state_of(&id), Some(DriverState::Disabled));
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
            .all(|r| r.state() == DriverState::Registered));
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
        assert_eq!(host.state_of(&id), Some(DriverState::Running));

        exit_tx
            .send(DriverExit::Halted("lane-local violation".to_string()))
            .expect("send exit");
        let task = tasks.into_iter().next().expect("task");
        assert_eq!(task.id(), &id);
        let exit = task.wait().await;
        host.record_driver_exit(&id, exit).expect("record exit");
        let record = host.record(&id).expect("record");
        assert_eq!(record.state(), DriverState::Halted);
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
    /// standalone sidecar provides: `revm`'s `WrapDatabaseAsync::new` (the
    /// layer `BlockSimHandle::build` stacks) returns `None` under a
    /// current-thread runtime, so a dedicated current-thread lane runtime
    /// leaves every frame with `replay_unavailable`.
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
        assert_eq!(host.state_of(&id), Some(DriverState::Running));
        let task = tasks.into_iter().next().expect("task");
        host.drive_and_fold(task)
            .await
            .expect("the host owns the fold");

        assert_eq!(
            host.state_of(&id),
            Some(DriverState::Stopped),
            "a loop that returned is never reported as Running"
        );
        host.disable(&id)
            .expect("a stopped record is still disableable");
        assert_eq!(host.state_of(&id), Some(DriverState::Disabled));
    }

    #[test]
    fn start_driving_leaves_a_driver_without_a_spawn_registered() {
        let mut host = host();
        let id = register(&mut host, "backrun");
        host.enable(&id).expect("enable");
        let tasks = host.start_driving().expect("start driving");
        assert!(tasks.is_empty());
        assert_eq!(host.state_of(&id), Some(DriverState::Enabled));
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

    /// The `mint` contract is a caller convention (the host cannot inspect the
    /// closure), so pin it the only way it is observable: the channels the
    /// closure registers on the hub it is handed must be the channels carried
    /// by the hub the host ends up owning. A closure that registered on a
    /// foreign hub would leave `host.hub()` empty.
    #[test]
    fn the_mint_closure_registers_on_the_host_hub() {
        use crate::arb_engine::EngineChannelHandles;
        use degenbot_eventhub::OverflowPolicy;

        let (host, _handles) = StrategyHost::mint(
            Arc::new(RouteRegistry::new(V2ConnectorIndex::default())),
            Arc::new(NonceAuthority::new(7)),
            EngineChannelHandles::register_on,
        );

        assert_eq!(
            host.hub().unbounded_flagged_count(),
            2,
            "the mint closure's two engine channels are registered on the host's hub"
        );
        assert_eq!(
            host.hub().named_policy("engine_result_batch"),
            Some(OverflowPolicy::UnboundedFlagged {
                name: "engine_result_batch"
            }),
            "the host hub carries the registration the closure made"
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
