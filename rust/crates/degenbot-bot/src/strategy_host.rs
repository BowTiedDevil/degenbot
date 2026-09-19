//! The strategy host: the process-wide owner of one event hub, one route
//! registry, one nonce authority, and the registered strategy drivers.
//!
//! A strategy is admitted to the host as a *driver FSM instance*: a named
//! record whose lifecycle is `Registered -> Enabled -> Running -> {Halted,
//! Disabled}`. The lifecycle is a total transition function — an operator verb
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
/// backrun lane's `Rc`-backed buffers) is booted on a dedicated single-thread
/// runtime by [`StrategyHost::start_driving`], while the factory itself must be
/// `Send` so it can travel to that thread.
pub type DriverFuture = Pin<Box<dyn Future<Output = DriverExit> + 'static>>;

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
    ///
    /// # Panics
    ///
    /// Panics if a driver's dedicated single-thread runtime cannot be built —
    /// process infrastructure whose failure is fatal, not recoverable.
    #[expect(
        clippy::expect_used,
        reason = "a driver's dedicated single-thread runtime is process infrastructure: failing to build it is fatal"
    )]
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
            // A lane's replay stack is single-threaded, so its loop runs on a
            // dedicated current-thread runtime off a blocking thread — the
            // future is minted and polled on that thread and never travels.
            let handle = runtime.spawn_blocking(move || {
                let future = spawn(lane);
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("driver lane runtime")
                    .block_on(future)
            });
            self.start(&id)?;
            tasks.push(DriverTask { id, handle });
        }
        Ok(tasks)
    }

    /// Fold a started driver's terminal exit into the FSM: a self-halt is a
    /// tombstone carrying the cause; a clean stop leaves the move to the
    /// operator's disable verb.
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
                if self.record(id).is_none() {
                    return Err(HostError::UnknownStrategy(id.clone()));
                }
                Ok(())
            }
        }
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
            } else {
                assert_eq!(state.on_halt(), Err(FsmDecline::HaltRequiresRunning));
            }

            if state.is_terminal() {
                assert_eq!(state.on_disable(), Err(FsmDecline::DisableRejectsTerminal));
            } else {
                assert_eq!(state.on_disable(), Ok(DriverState::Disabled));
            }
        }
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

    #[test]
    fn start_driving_leaves_a_driver_without_a_spawn_registered() {
        let mut host = host();
        let id = register(&mut host, "backrun");
        host.enable(&id).expect("enable");
        let tasks = host.start_driving().expect("start driving");
        assert!(tasks.is_empty());
        assert_eq!(host.state_of(&id), Some(DriverState::Enabled));
    }
}
