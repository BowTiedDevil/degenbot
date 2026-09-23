//! Strategy-host operator verbs on `PyArbEngine`.
//!
//! The engine boot attaches to a host-minted hub (the C2 attach signature) and
//! registers the backrun lane's spawn factory; the engine's start flow boots
//! the enabled drivers and supervises their lane boundaries. These are the
//! operator's runtime verbs over the host's driver FSM. The Python surface is
//! deliberately thin — every decision (admission, configured-ness, the
//! transition table, tombstone semantics) lives in the Rust host, and this
//! module only translates calls and maps typed refusals onto typed Python
//! exceptions.

use super::{Arc, PyArbEngine, StrategyHostError, UnconfiguredStrategyError, UnknownStrategyError};
use crate::prelude::*;

use degenbot_bot::arb_engine::EngineChannelHandles;
use degenbot_bot::bot_core::route_registry::RouteRegistry;
use degenbot_bot::connector_index::V2ConnectorIndex;
use degenbot_bot::nonce_authority::{NonceAuthority, StrategyId};
use degenbot_bot::strategy_host::{FacetStatus, HostError, HostHub, StrategyHost};

/// The Python-facing name of a driver's FSM state.
fn state_name(state: degenbot_bot::strategy_host::DriverPose) -> &'static str {
    use degenbot_bot::strategy_host::DriverPose;
    match state {
        DriverPose::Registered => "registered",
        DriverPose::Enabled => "enabled",
        DriverPose::Running => "running",
        DriverPose::Stopped => "stopped",
        DriverPose::Halted => "halted",
        DriverPose::Disabled => "disabled",
    }
}

/// Map a host refusal onto the typed Python exception family.
pub(crate) fn map_host_error(error: HostError) -> pyo3::PyErr {
    match error {
        HostError::UnknownStrategy(id) => UnknownStrategyError::new_err(format!(
            "unknown strategy \"{id}\": the host has no registered driver with that name"
        )),
        HostError::UnconfiguredStrategy(id) => UnconfiguredStrategyError::new_err(format!(
            "strategy \"{id}\" is unconfigured: no facet with its required keys was booted"
        )),
        other => StrategyHostError::new_err(other.to_string()),
    }
}

/// The cockpit session-phase table, exposed so the Python `_Phase` translates
/// the host's verdict instead of authoring legality.
///
/// `current` is one of `new`/`started`/`running`/`closed`; `operation` is one
/// of `start`/`run`/`query`/`shutdown`. Returns the next phase name, or `None`
/// when the host refuses the move. The Python `_Phase` maps `None` onto its
/// `PhaseError`.
///
/// # Errors
///
/// `ValueError` for a phase or operation the host table does not name.
#[pyfunction]
pub(crate) fn session_phase_next(current: &str, operation: &str) -> PyResult<Option<&'static str>> {
    use degenbot_bot::strategy_host::SessionPhase;

    let phase = match current {
        "new" => SessionPhase::New,
        "started" => SessionPhase::Started,
        "running" => SessionPhase::Running,
        "closed" => SessionPhase::Closed,
        other => {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "unknown cockpit session phase {other:?}"
            )));
        }
    };

    let next = match operation {
        "start" => phase.on_start(),
        "run" => phase.on_run(),
        "query" => phase.on_query(),
        "shutdown" => Ok(phase.on_shutdown()),
        other => {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "unknown cockpit session operation {other:?}"
            )));
        }
    };
    Ok(next.ok().map(SessionPhase::as_str))
}

/// Boot the process-wide strategy host: mint the hub with the engine's two
/// named source channels, the frozen route registry, and the nonce authority,
/// register the two strategy facets this process configures, install the state
/// root a hosted lane scopes its artifacts under, and attach the two
/// per-ecosystem backrun
/// lane's spawn factory.
///
/// The settlement facet is always configured because this boot IS the
/// settlement engine, and it registers no spawn factory (the engine's pump arm
/// drives it). The backrun facet is configured when the operator named it as
/// the active arm or set one of its required keys (bid mode or the operator
/// key file); a boot that never names it registers the facet as unconfigured,
/// so enabling it fails loudly instead of starting a lane with no operator
/// intent behind it. The factory resolves the lane's node join only when the
/// lane actually starts.
/// The per-strategy sign-time lanes the hosted head feed folds notices
/// through, keyed by the owning strategy.
#[cfg(feature = "submission")]
pub(crate) type HeadLanes =
    std::collections::HashMap<StrategyId, Arc<degenbot_submission::NonceLane>>;

/// The host boot's product: the host, the hub/attachment pair, and (when the
/// submission feature is on) the per-strategy nonce lanes.
pub(crate) struct BootedHost {
    pub(crate) host: StrategyHost,
    pub(crate) attached: HostHub<EngineChannelHandles>,
    #[cfg(feature = "submission")]
    pub(crate) head_lanes: HeadLanes,
}

/// The route registry a hosted boot mints the strategy host over.
///
/// Delegates to the shared submission resolver, so a hosted backrun lane
/// discovers over the same DB-backed snapshot a hosted boot builds.
/// A process with no connector DB (or no resolvable node join) mints an empty
/// snapshot; the lane then observes with discovery shut rather than guessing
/// connectors.
#[cfg(feature = "submission")]
fn hosted_route_registry() -> Arc<RouteRegistry> {
    let config = degenbot_config::holder::config();
    let db_path = degenbot_config::resolve_database_path(&degenbot_config::ProcessEnv, None).value;
    match degenbot_strategy::backrun_driver::resolve_backrun_node_join() {
        Ok(join) => degenbot_core::runtime::get_runtime().block_on(
            degenbot_strategy::backrun_driver::resolve_backrun_host_registry(
                config,
                &db_path,
                &join.provider,
            ),
        ),
        Err(error) => {
            tracing::debug!(
                %error,
                "backrun node join unresolved - hosted registry discovery shut"
            );
            Arc::new(RouteRegistry::new(V2ConnectorIndex::default()))
        }
    }
}

/// The registry a build without the submission feature mints: no hosted
/// pending-transaction lane exists, so an empty snapshot answers membership.
#[cfg(not(feature = "submission"))]
fn hosted_route_registry() -> Arc<RouteRegistry> {
    Arc::new(RouteRegistry::new(V2ConnectorIndex::default()))
}

/// The host's admission status for one strategy: configured iff its facet is
/// active in the resolved typed config. Stance-independent — the runner's
/// posture gate refuses the boot in BOTH stances when the arm it hosts is
/// inactive, so no stance-specific waiver lives at the boot's facet table.
#[must_use]
fn facet_status(
    name: degenbot_strategy::StrategyName,
    cfg: &degenbot_config::schema::BotConfig,
) -> FacetStatus {
    if name.is_active(cfg) {
        FacetStatus::Configured
    } else {
        FacetStatus::Unconfigured
    }
}

#[expect(
    clippy::expect_used,
    reason = "a freshly minted host has no registered strategies, so both registers are infallible"
)]
pub(crate) fn boot_host() -> BootedHost {
    // An engine construction IS the ambient runtime's first use (FF-T5):
    // the fleet boot stamps the process with a fleet, so the shared io
    // runtime must exist - the worker census reports at least the
    // io_runtime_workers row - regardless of whether the backrun lane's
    // node join resolved (a joinless host keeps the registry EMPTY below,
    // but nothing may make the runtime's existence host-dependent). The
    // binding also pins pyo3-async to the shared runtime BEFORE any async
    // seam runs, the GOQWCL second-runtime obligation.
    crate::ambient_runtime::ensure_async_runtime_bound();
    let (mut host, attached) = StrategyHost::mint(
        hosted_route_registry(),
        Arc::new(NonceAuthority::new(0)),
        EngineChannelHandles::register_on,
    );

    let cfg = degenbot_config::holder::config();
    // Register every strategy under its plane name, in plane order, derived
    // from its `strategy.<facet>.active` key — including settlement, whose
    // retired boot-time waiver ("a dry-run boot must still enable the arm")
    // is gone: activation isn't a live-only concern.
    for name in degenbot_strategy::StrategyName::ALL {
        host.register(StrategyId::new(name.as_str()), facet_status(name, cfg))
            .expect("a freshly minted host registers every strategy");
    }

    #[cfg(feature = "submission")]
    let head_lanes = {
        // A hosted driver scopes its run-artifacts under the host state root; a
        // boot with no resolvable root leaves `namespace_root` unset, so the
        // driver keeps its process-global path.
        if degenbot_config::holder::installed() {
            if let Some(root) = degenbot_submission::resolve_state_root() {
                host.set_state_root(root);
            }
        }
        // The per-strategy submission ledger is the head feed's
        // submission-truth arm: the host refreshes the authority per head and
        // asks this ledger to close outstanding records out, delivering the
        // typed notices to the owning strategy. Every lane stamps through the
        // same authority and records into the same ledger.
        let ledger = Arc::new(degenbot_submission::SubmissionLedger::new());
        host.attach_reconciler(
            Arc::clone(&ledger) as Arc<dyn degenbot_bot::strategy_host::HeadReconciler>
        );

        let settlement_id = StrategyId::new("settlement");
        let settlement_lane = Arc::new(degenbot_submission::NonceLane::new(
            Arc::clone(host.nonce()),
            Arc::clone(&ledger),
            settlement_id.clone(),
        ));
        // The settlement seam is the Python-driven arm of this one hosted
        // process, so its lane is process-global: every settlement submission
        // stamps through the shared authority once the boot installs it.
        crate::submission::submit::install_settlement_lane(Arc::clone(&settlement_lane));

        // Each per-ecosystem backrun gets its own lane and spawn factory; the
        // two are independently activatable and may run together.
        let mut lanes = HeadLanes::new();
        lanes.insert(settlement_id, settlement_lane);
        for (name, ecosystem) in [
            (
                "mevblocker_backrun",
                degenbot_strategy::backrun_driver::BackrunEcosystem::Mevblocker,
            ),
            (
                "peer_backrun",
                degenbot_strategy::backrun_driver::BackrunEcosystem::Peer,
            ),
        ] {
            let id = StrategyId::new(name);
            let lane = Arc::new(degenbot_submission::NonceLane::new(
                Arc::clone(host.nonce()),
                Arc::clone(&ledger),
                id.clone(),
            ));
            host.attach_spawn(
                &id,
                degenbot_strategy::backrun_driver::backrun_spawn_factory(
                    Arc::clone(degenbot_config::holder::config_arc()),
                    ecosystem,
                    Arc::clone(host.hub()),
                    Some(Arc::clone(host.registry())),
                    Arc::clone(&lane),
                ),
            )
            .expect("fresh host registers the backrun spawn");
            lanes.insert(id, lane);
        }
        lanes
    };

    BootedHost {
        host,
        attached,
        #[cfg(feature = "submission")]
        head_lanes,
    }
}

impl PyArbEngine {
    /// Run an operator closure under the host lock with the GIL released, so a
    /// slow host call never parks the interpreter.
    fn with_host<T>(&self, py: Python<'_>, f: impl FnOnce(&mut StrategyHost) -> T + Send) -> T
    where
        T: Send,
    {
        py.detach(|| f(&mut self.host.lock()))
    }

    /// Boot every enabled strategy that registered a spawn factory. The
    /// host-owned supervisor owns one supervision task per driver; each awaits
    /// its driver's lane boundary and folds the terminal exit through the host
    /// FSM, so a self-halt becomes a tombstone `strategies()` reports.
    ///
    /// A facet with no factory is skipped by the host: the settlement engine's
    /// pump arm already drives it, so it is not a hosted loop.
    ///
    /// # Errors
    ///
    /// [`HostError::Transition`] when the lifecycle refuses a driver's start.
    pub(crate) fn start_hosted_strategies(&self) -> Result<usize, HostError> {
        let tasks = self.host.lock().start_driving()?;
        let count = tasks.len();
        self.supervisor.supervise(tasks);
        Ok(count)
    }
}

#[pymethods]
impl PyArbEngine {
    /// Enable a registered, configured strategy. Returns the resulting FSM
    /// state name.
    ///
    /// Raises `UnknownStrategyError` when the name was never registered, and
    /// `UnconfiguredStrategyError` when the name is known but its config facet
    /// was not booted. A lifecycle refusal raises `StrategyHostError`.
    fn enable_strategy(&self, py: Python<'_>, name: &str) -> PyResult<String> {
        let id = StrategyId::new(name);
        let state = self
            .with_host(py, |host| host.enable(&id))
            .map_err(map_host_error)?;
        Ok(state_name(state).to_string())
    }

    /// Disable a non-terminal strategy. Its held nonce reservations are
    /// released. Raises the same typed refusals as `enable_strategy`.
    fn disable_strategy(&self, py: Python<'_>, name: &str) -> PyResult<()> {
        let id = StrategyId::new(name);
        self.with_host(py, |host| host.disable(&id))
            .map_err(map_host_error)
    }

    /// Drive the host's per-head reconciliation from the engine's head feed.
    ///
    /// The head feed calls this once per accepted header. When any strategy
    /// holds a nonce reservation or any submission record is still
    /// non-terminal, the host refreshes the confirmed chain nonce, reconciles
    /// outstanding submission records, and folds each typed notice into the
    /// owning strategy's default policy. A boot with no hosted activity
    /// short-circuits before the chain read, so the settlement-only default
    /// boot pays no new RPC.
    ///
    /// # Errors
    ///
    /// `ValueError` for an unparseable operator address. A chain-read failure
    /// is logged and tolerated: the next head retries.
    #[cfg(feature = "submission")]
    #[pyo3(signature = (provider, operator_address))]
    fn reconcile_hosted_head<'py>(
        &self,
        py: Python<'py>,
        provider: &crate::rpc::async_provider::PyAsyncAlloyProvider,
        operator_address: &str,
    ) -> PyResult<Bound<'py, pyo3::types::PyAny>> {
        let provider_arc = provider.provider_arc();
        let address = crate::address_utils::parse_address(operator_address).map_err(|error| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid operator address: {error}"))
        })?;
        let host = Arc::clone(&self.host);
        let lanes = Arc::clone(&self.head_lanes);
        crate::ambient_runtime::future_into_py(py, async move {
            if !host.lock().has_hosted_activity() {
                return Ok(0u64);
            }
            let confirmed = match provider_arc.get_transaction_count(&address, None).await {
                Ok(nonce) => nonce,
                Err(error) => {
                    tracing::warn!(
                        target: "degenbot.strategy.head",
                        %error,
                        "per-head chain nonce read failed; reconciliation deferred"
                    );
                    return Ok(0u64);
                }
            };
            let notices = host.lock().on_head(confirmed);
            let mut folded = 0u64;
            {
                let lanes = lanes.lock();
                for notice in &notices {
                    let Some(lane) = lanes.get(notice.strategy()) else {
                        continue;
                    };
                    let policy = degenbot_submission::HeadPolicy::new(Arc::clone(lane));
                    match policy.on_notice(notice) {
                        Ok(action) => {
                            folded += 1;
                            tracing::info!(
                                target: "degenbot.strategy.head",
                                strategy = %notice.strategy(),
                                nonce = notice.nonce(),
                                action = ?action,
                                "hosted head notice folded into the strategy policy"
                            );
                        }
                        Err(decline) => {
                            tracing::warn!(
                                target: "degenbot.strategy.head",
                                strategy = %notice.strategy(),
                                nonce = notice.nonce(),
                                %decline,
                                "head policy could not re-stamp"
                            );
                        }
                    }
                }
            }
            Ok(folded)
        })
    }

    /// Every registered strategy as `(name, state, halt_reason)`, in
    /// registration order. A halted tombstone carries its cause; every other
    /// state carries `None`.
    fn strategies(&self, py: Python<'_>) -> Vec<(String, String, Option<String>)> {
        self.with_host(py, |host| {
            host.list()
                .into_iter()
                .map(|record| {
                    (
                        record.id().as_str().to_string(),
                        state_name(record.state()).to_string(),
                        record.halt_detail().map(str::to_string),
                    )
                })
                .collect()
        })
    }
}

#[cfg(all(test, feature = "submission"))]
mod tests {
    #![expect(clippy::expect_used, reason = "test assertions fail loudly")]

    use super::*;
    use degenbot_bot::strategy_host::{DriverExit, DriverPose};

    /// The engine's start flow boots a driver that registered a factory and
    /// folds its terminal exit into the FSM, so a self-halt is a queryable
    /// tombstone carried by `strategies()`.
    #[test]
    fn the_start_flow_boots_a_registered_factory_and_folds_its_exit() {
        // The facet's start flow needs an active settlement facet: the boot's
        // facet table reads `strategy.settlement.active` for every strategy,
        // so a defaulted (inactive) holder refuses the enable like any other
        // facet. The install is first-wins, and no other test in this binary
        // installs — a second install unittesting HERE would fail loudly.
        let mut cfg = degenbot_config::schema::BotConfig::default();
        cfg.strategy.settlement.active = true;
        assert!(
            degenbot_config::holder::install(std::sync::Arc::new(cfg)),
            "this test owns the binary's holder install"
        );
        Python::attach(|py| {
            let engine = PyArbEngine::new(py, None);
            assert!(
                engine
                    .host
                    .lock()
                    .has_spawn(&StrategyId::new("mevblocker_backrun")),
                "the engine boot registers the mevblocker backrun lane's spawn factory"
            );
            assert!(
                engine
                    .host
                    .lock()
                    .has_spawn(&StrategyId::new("peer_backrun")),
                "the engine boot registers the peer backrun lane's spawn factory"
            );
            let id = StrategyId::new("settlement");
            engine.enable_strategy(py, "settlement").expect("enable");

            let (exit_tx, exit_rx) = tokio::sync::oneshot::channel::<DriverExit>();
            engine
                .host
                .lock()
                .attach_spawn(
                    &id,
                    Box::new(move |_lane| {
                        Box::pin(async move { exit_rx.await.unwrap_or(DriverExit::Stopped) })
                    }),
                )
                .expect("attach spawn");

            let started = engine.start_hosted_strategies().expect("start driving");
            assert_eq!(started, 1, "the enabled driver with a factory starts");
            assert_eq!(engine.strategies(py)[0].1, "running");

            exit_tx
                .send(DriverExit::Halted("lane-local violation".to_string()))
                .expect("send exit");

            let record = degenbot_core::runtime::get_runtime().block_on(async {
                for _ in 0..1_000 {
                    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                    let record = engine.host.lock().record(&id).cloned();
                    if record
                        .as_ref()
                        .is_some_and(|record| record.state() == DriverPose::Halted)
                    {
                        return record;
                    }
                }
                None
            });
            let record = record.expect("the driver exit folds into the FSM");
            assert_eq!(record.halt_detail(), Some("lane-local violation"));
            assert_eq!(
                engine.strategies(py)[0],
                (
                    "settlement".to_string(),
                    "halted".to_string(),
                    Some("lane-local violation".to_string())
                )
            );
        });
    }

    /// The engine boot wires the head feed's per-strategy lanes and leaves the
    /// reconcile guard closed, so a settlement-only boot pays no per-head work.
    #[test]
    fn the_boot_installs_head_lanes_and_starts_with_the_guard_closed() {
        Python::attach(|py| {
            let engine = PyArbEngine::new(py, None);
            assert!(
                !engine.host.lock().has_hosted_activity(),
                "a fresh boot has no lease and no record, so the head feed short-circuits"
            );
            let lanes = engine.head_lanes.lock();
            let settlement_lane = lanes
                .get(&StrategyId::new("settlement"))
                .cloned()
                .expect("the boot installs the settlement head lane");
            assert!(
                lanes.contains_key(&StrategyId::new("mevblocker_backrun")),
                "the boot installs the mevblocker backrun head lane"
            );
            assert!(
                lanes.contains_key(&StrategyId::new("peer_backrun")),
                "the boot installs the peer backrun head lane"
            );
            drop(lanes);

            settlement_lane.stamp().expect("settlement lane stamps");
            assert!(
                engine.host.lock().has_hosted_activity(),
                "a live lease opens the reconcile guard"
            );
        });
    }

    /// Both per-ecosystem backrun facets are hosted side by side: the boot
    /// registers each under its own id, so either (or both) can be enabled
    /// independently in the same process.
    #[test]
    fn the_boot_hosts_both_backrun_facets_independently() {
        Python::attach(|py| {
            let engine = PyArbEngine::new(py, None);
            let names: Vec<String> = engine
                .strategies(py)
                .into_iter()
                .map(|(name, _state, _halt)| name)
                .collect();
            assert!(
                names.contains(&"mevblocker_backrun".to_string()),
                "the mevblocker facet is registered on the host: {names:?}"
            );
            assert!(
                names.contains(&"peer_backrun".to_string()),
                "the peer facet is registered on the host: {names:?}"
            );
        });
    }

    /// The stance-independent posture rule at the host's facet table: every
    /// strategy — settlement included — registers configured iff its
    /// `strategy.<facet>.active` key is on. The boot-time settlement waiver
    /// (a dry-run boot must still enable the arm) is retired: activation
    /// isn't a live-only concern, and the stance-independent runner gate
    /// refuses the boot in both stances before the host serves a session.
    #[test]
    fn each_facet_is_configured_iff_its_active_key_is_on() {
        #[expect(clippy::type_complexity)]
        let name_and_active: [(
            degenbot_strategy::StrategyName,
            fn(&mut degenbot_config::schema::BotConfig),
        ); 3] = [
            (degenbot_strategy::StrategyName::Settlement, |cfg| {
                cfg.strategy.settlement.active = true;
            }),
            (degenbot_strategy::StrategyName::MevblockerBackrun, |cfg| {
                cfg.strategy.mevblocker_backrun.active = true;
            }),
            (degenbot_strategy::StrategyName::PeerBackrun, |cfg| {
                cfg.strategy.peer_backrun.active = true;
            }),
        ];
        for (name, activate) in name_and_active {
            let mut cfg = degenbot_config::schema::BotConfig::default();
            assert_eq!(
                facet_status(name, &cfg),
                FacetStatus::Unconfigured,
                "a defaulted config activates no facet"
            );
            activate(&mut cfg);
            assert_eq!(
                facet_status(name, &cfg),
                FacetStatus::Configured,
                "the activated facet registers configured"
            );
        }
    }
}
