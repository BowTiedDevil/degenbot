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
use degenbot_bot::strategy_host::{FacetStatus, HostError, HostHub, StrategyHost};
use degenbot_substrate::nonce::{NonceAuthority, StrategyId};

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
/// through, keyed by the owning strategy. The type lives on the head
/// reconciliation module (its fold's owner map); this is the shell's view.
#[cfg(feature = "submission")]
pub(crate) use degenbot_submission::head_reconciliation::HeadLanes;

/// The host boot's product: the shared host handle and the hub/attachment
/// pair.
///
/// The host is already shared: the boot builds the backrun lanes' head
/// reconciliation over the same `Arc`, so the drivers' hosted head edge and
/// the shell fold through one host.
pub(crate) struct BootedHost {
    pub(crate) host: Arc<parking_lot::Mutex<StrategyHost>>,
    pub(crate) attached: HostHub<EngineChannelHandles>,
}

/// Resolve the strategy-owned boot product once for the hosted process.
///
/// This Rust binding only routes the resolved product into the generic host;
/// it does not choose a registry, reopen a DB, or author a boot policy.
#[cfg(feature = "submission")]
fn backrun_boot_resources(
    config: Arc<degenbot_config::schema::BotConfig>,
) -> degenbot_strategy::backrun_driver::BackrunBootResources {
    // The four layers this process booted with, so the database path is the
    // same value the console and a pure-Rust consumer resolve (ADR-062 D7).
    let loaded = match degenbot_config::load_process_config() {
        Ok(loaded) => loaded,
        Err(error) => {
            tracing::debug!(
                %error,
                "config layers unresolved - hosted backrun boot shut"
            );
            return degenbot_strategy::backrun_driver::BackrunBootResources::unresolved(
                config,
                degenbot_strategy::backrun_driver::BackrunBootError::NodeJoin(error.to_string()),
            );
        }
    };
    let db_path = degenbot_config::resolve_database_path(&loaded, None).value;
    // The same loaded layers the database path came from: the join resolves
    // the session chain once and carries it to the connector index and the
    // head feed, so a hosted backrun lane cannot run against a chain its
    // operator never named. The capability is the host's transport decision:
    // this host lets the resolved endpoint name its own scheme, so an
    // `ipc://` entry in the operator file reaches the hosted driver.
    let capability = Arc::new(degenbot_strategy::backrun_driver::AnyRequestTransport);
    let join = degenbot_core::runtime::get_runtime().block_on(
        degenbot_strategy::backrun_driver::resolve_backrun_node_join(&loaded, None, capability),
    );
    match join {
        Ok(join) => degenbot_core::runtime::get_runtime().block_on(
            degenbot_strategy::backrun_driver::resolve_backrun_boot(
                Arc::clone(&config),
                db_path,
                join,
            ),
        ),
        Err(error) => {
            tracing::debug!(
                %error,
                "backrun node join unresolved - hosted registry discovery shut"
            );
            degenbot_strategy::backrun_driver::BackrunBootResources::unresolved(config, error)
        }
    }
}

/// The registry a build without the submission feature mints: no hosted
/// pending-transaction lane exists, so an empty snapshot answers membership.
#[cfg(not(feature = "submission"))]
fn hosted_route_registry() -> Arc<degenbot_substrate::RouteRegistry> {
    Arc::new(degenbot_substrate::RouteRegistry::new(
        degenbot_bot::connector_index::V2ConnectorIndex::default(),
    ))
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
    // seam runs, the second-runtime obligation.
    crate::ambient_runtime::ensure_async_runtime_bound();
    let cfg = degenbot_config::holder::config();

    #[cfg(feature = "submission")]
    let backrun_resources = backrun_boot_resources(degenbot_config::holder::config_arc().clone());
    #[cfg(feature = "submission")]
    let registry = Arc::clone(backrun_resources.registry());
    #[cfg(not(feature = "submission"))]
    let registry = hosted_route_registry();

    let (host, attached) = StrategyHost::mint(
        registry,
        Arc::new(NonceAuthority::new(0)),
        EngineChannelHandles::register_on,
    );
    // The host is shared from birth: the backrun lanes' head reconciliation
    // and the shell's hosted head feed hold the same `Arc`, so a driver loop
    // and the engine's head tick reconcile one shared host.
    let host = Arc::new(parking_lot::Mutex::new(host));

    // Register every strategy under its plane name, in plane order, derived
    // from its `strategy.<facet>.active` key — including settlement, whose
    // retired boot-time waiver ("a dry-run boot must still enable the arm")
    // is gone: activation isn't a live-only concern.
    for name in degenbot_strategy::StrategyName::ALL {
        host.lock()
            .register(StrategyId::new(name.as_str()), facet_status(name, cfg))
            .expect("a freshly minted host registers every strategy");
    }

    #[cfg(feature = "submission")]
    let _head_lanes = {
        use degenbot_bot::hosted_sources::HostedHeadClock;
        use degenbot_eventhub::PendingTxSource;
        use degenbot_rpc::backrun_feed::BackrunFeedConfig;
        use degenbot_rpc::pending_tx_stream::PendingTxFeedConfig;
        use degenbot_rpc::txpool_feed::TxpoolFeedConfig;
        use degenbot_strategy::backrun_driver::BackrunEcosystem;
        // A hosted driver scopes its run-artifacts under the host state root; a
        // boot with no resolvable root leaves `namespace_root` unset, so the
        // driver keeps its process-global path.
        if degenbot_config::holder::installed() {
            if let Some(root) = degenbot_submission::resolve_state_root() {
                host.lock().set_state_root(root);
            }
        }
        // The per-strategy submission ledger is the head feed's
        // submission-truth arm: the host refreshes the authority per head and
        // asks this ledger to close outstanding records out, delivering the
        // typed notices to the owning strategy. Every lane stamps through the
        // same authority and records into the same ledger.
        let ledger = Arc::new(degenbot_submission::SubmissionLedger::new());
        host.lock().attach_reconciler(
            Arc::clone(&ledger) as Arc<dyn degenbot_bot::strategy_host::HeadReconciler>
        );

        let settlement_id = StrategyId::new("settlement");
        let settlement_lane = Arc::new(degenbot_submission::NonceLane::new(
            Arc::clone(host.lock().nonce()),
            Arc::clone(&ledger),
            settlement_id.clone(),
        ));
        // The settlement seam is the Python-driven arm of this one hosted
        // process, so its lane is process-global: every settlement submission
        // stamps through the shared authority once the boot installs it.
        crate::submission::submit::install_settlement_lane(Arc::clone(&settlement_lane));

        // Each per-ecosystem backrun gets its own lane and product. The
        // products share the resolver's held DB and frozen registry. The lanes
        // are all built first so the head reconciliation owns the full map
        // before any driver starts.
        let mut lanes = HeadLanes::new();
        lanes.insert(settlement_id, settlement_lane);
        let mut backruns = Vec::new();
        for (name, ecosystem) in [
            (
                "mevblocker_backrun",
                degenbot_strategy::backrun_driver::BackrunEcosystem::Mevblocker,
            ),
            (
                "txpool_backrun",
                degenbot_strategy::backrun_driver::BackrunEcosystem::Txpool,
            ),
        ] {
            let id = StrategyId::new(name);
            let lane = Arc::new(degenbot_submission::NonceLane::new(
                Arc::clone(host.lock().nonce()),
                Arc::clone(&ledger),
                id.clone(),
            ));
            backruns.push((id.clone(), ecosystem, Arc::clone(&lane)));
            lanes.insert(id, lane);
        }

        // The drivers' once-per-head reconciliation: one instance per
        // process, built over the resolved node join's provider (a joinless
        // boot drives no head-feed reconciliation) and the operator address
        // the signing key names. No configured key is the old loop's
        // `signer` gate: a boot with no signer reconciles nothing. The
        // hosted sources' head edge fires it once per observed head through
        // the trigger closure below — the trigger is the only thing this
        // shell wires; the drivers never see the reconciliation.
        let operator = cfg
            .strategy
            .mevblocker_backrun
            .key_file
            .as_ref()
            .or(cfg.strategy.txpool_backrun.key_file.as_ref())
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|hex| degenbot_submission::TxSigner::from_key_hex(hex.trim(), 1).ok())
            .map(|signer| signer.address());
        let head_reconciliation = match (backrun_resources.node_provider().cloned(), operator) {
            (Some(provider), Some(operator)) => Some(Arc::new(
                degenbot_submission::head_reconciliation::HeadReconciliation::new(
                    Arc::clone(&host),
                    lanes.clone(),
                    Arc::new(
                        degenbot_submission::head_reconciliation::AlloyChainNonceRead(provider),
                    ),
                    operator,
                ),
            )),
            _ => None,
        };
        let head_trigger: Option<degenbot_bot::hosted_sources::HeadTrigger> =
            head_reconciliation.as_ref().map(|reconciliation| {
                let reconciliation = Arc::clone(reconciliation);
                Arc::new(move |head: u64| {
                    let reconciliation = Arc::clone(&reconciliation);
                    Box::pin(async move {
                        reconciliation.reconcile_head(head).await;
                    })
                        as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
                }) as _
            });

        // Host the sources once per process: one pending-tx pump per ACTIVE
        // arm's source kind (both arms can run simultaneously — each kind
        // registers its own hub class), the one head watch + fallback
        // poller, and the one feed sampler. Each arm's spawn factory
        // receives its own kind's typed stream.
        let mut arm_boots = Vec::new();
        for (id, ecosystem, lane) in backruns {
            let boot = backrun_resources.strategy_boot(ecosystem);
            arm_boots.push((id, ecosystem, lane, boot));
        }
        let head_ws_url = arm_boots
            .iter()
            .filter_map(|(_, _, _, boot)| boot.head_ws_url().map(str::to_string))
            .next();
        let provider = backrun_resources.node_provider().cloned();
        let clock = match (head_ws_url, provider) {
            (Some(ws_url), Some(provider)) => Some(HostedHeadClock::Watch { ws_url, provider }),
            (None, Some(provider)) => Some(HostedHeadClock::Poll { provider }),
            _ => None,
        };
        let mut arms = Vec::new();
        for (id, ecosystem, _lane, boot) in &arm_boots {
            let name = match ecosystem {
                BackrunEcosystem::Mevblocker => degenbot_strategy::StrategyName::MevblockerBackrun,
                BackrunEcosystem::Txpool => degenbot_strategy::StrategyName::TxpoolBackrun,
            };
            let _ = id;
            if !name.is_active(cfg) {
                continue;
            }
            let arm = match ecosystem {
                BackrunEcosystem::Mevblocker => (
                    PendingTxSource::Mevblocker,
                    PendingTxFeedConfig::Mevblocker(BackrunFeedConfig {
                        url: boot.feed_url().to_string(),
                        ..BackrunFeedConfig::for_mainnet()
                    }),
                ),
                BackrunEcosystem::Txpool => {
                    let Some(ws_url) = boot.head_ws_url().map(str::to_string) else {
                        // No subscription endpoint: the arm cannot host a
                        // pump; its live gate reports the same refusal a
                        // feed-less boot always did.
                        continue;
                    };
                    (
                        PendingTxSource::Txpool,
                        PendingTxFeedConfig::Txpool(TxpoolFeedConfig {
                            ws_url,
                            ..TxpoolFeedConfig::defaults()
                        }),
                    )
                }
            };
            arms.push(arm);
        }
        // The hub is cloned OUTSIDE the guarded statement: the receiver's
        // lock guard is alive while the factory argument evaluates, so a
        // nested `host.lock()` there would deadlock the non-reentrant mutex.
        let hub = Arc::clone(host.lock().hub());
        let mut hosted = degenbot_core::runtime::get_runtime()
            .block_on(degenbot_bot::hosted_sources::HostedSources::mint(
                Arc::clone(&hub),
                arms,
                clock,
                head_trigger,
            ))
            .expect("a fresh hub hosts the process sources");
        for (id, ecosystem, lane, _boot) in arm_boots {
            let kind = match ecosystem {
                BackrunEcosystem::Mevblocker => PendingTxSource::Mevblocker,
                BackrunEcosystem::Txpool => PendingTxSource::Txpool,
            };
            let stream = hosted.take_stream(kind);
            host.lock()
                .attach_spawn(
                    &id,
                    degenbot_strategy::backrun_driver::backrun_spawn_factory(
                        backrun_resources.strategy_boot(ecosystem),
                        Arc::clone(&hub),
                        lane,
                        stream,
                    ),
                )
                .expect("fresh host registers the backrun spawn");
        }
        lanes
    };

    BootedHost { host, attached }
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

    /// Enable each named facet through the host's admission gate.
    ///
    /// The engine's start flow owns the enable-then-resume ordering: a hosted
    /// driver's loop starts only for an ENABLED facet, so the facet set and
    /// the pump resume are one ordered operation. Refusals map exactly as
    /// [`Self::enable_strategy`]'s.
    pub(crate) fn enable_facets(&self, py: Python<'_>, facets: &[String]) -> PyResult<()> {
        for facet in facets {
            let id = StrategyId::new(facet.as_str());
            self.with_host(py, |host| host.enable(&id))
                .map_err(map_host_error)?;
        }
        Ok(())
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
    use degenbot_bot::strategy_host::DriverPose;
    use degenbot_substrate::driver_spawn::DriverExit;

    /// Point the boot's database layer at an absent path, so the engine boot
    /// skips the operator's real connector roster. The boot loads the whole
    /// roster from the ambient DB (~380 MB here) on every construction, and
    /// nextest's process-per-test model pays that once per boot test — the
    /// whole cost of these tests. The wiring these tests assert (facet table,
    /// spawn factories, head lanes) is identical for the empty registry an
    /// absent DB mints, so the roster scan buys them nothing.
    ///
    /// The single `Once` write keeps the env mutation to one event, issued
    /// before any test boots an engine.
    fn hermetic_db_layer() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let absent = std::env::temp_dir().join("degenbot-boot-test-absent.db");
            let _ = std::fs::remove_file(&absent);
            std::env::set_var(degenbot_config::DB_PATH_ENV, absent);
        });
    }

    /// The engine's start flow boots a driver that registered a factory and
    /// folds its terminal exit into the FSM, so a self-halt is a queryable
    /// tombstone carried by `strategies()`.
    #[test]
    fn the_start_flow_boots_a_registered_factory_and_folds_its_exit() {
        hermetic_db_layer();
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
                    .has_spawn(&StrategyId::new("txpool_backrun")),
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
        hermetic_db_layer();
        Python::attach(|py| {
            let engine = PyArbEngine::new(py, None);
            assert!(
                !engine.host.lock().has_hosted_activity(),
                "a fresh boot has no lease and no record, so the head feed short-circuits"
            );
            // The boot installs the process-global settlement seam lane, the
            // one the hosted head reconciliation folds notices through.
            let settlement_lane = crate::submission::submit::settlement_lane()
                .expect("the boot installs the settlement head lane");

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
        hermetic_db_layer();
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
                names.contains(&"txpool_backrun".to_string()),
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
            (degenbot_strategy::StrategyName::TxpoolBackrun, |cfg| {
                cfg.strategy.txpool_backrun.active = true;
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
