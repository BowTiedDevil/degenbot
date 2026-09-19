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
use degenbot_bot::nonce_authority::{NonceAuthority, StrategyId};
use degenbot_bot::sidecar_paths::V2ConnectorIndex;
use degenbot_bot::strategy_host::{FacetStatus, HostError, HostHub, StrategyHost};

/// The Python-facing name of a driver's FSM state.
fn state_name(state: degenbot_bot::strategy_host::DriverState) -> &'static str {
    use degenbot_bot::strategy_host::DriverState;
    match state {
        DriverState::Registered => "registered",
        DriverState::Enabled => "enabled",
        DriverState::Running => "running",
        DriverState::Halted => "halted",
        DriverState::Disabled => "disabled",
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

/// Boot the process-wide strategy host: mint the hub with the engine's two
/// named source channels, the frozen route registry, and the nonce authority,
/// register the two strategy facets this process configures, install the state
/// root a hosted lane scopes its artifacts under, and attach the backrun
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
/// discovers over the same DB-backed snapshot the standalone sidecar builds.
/// A process with no connector DB (or no resolvable node join) mints an empty
/// snapshot; the lane then observes with discovery shut rather than guessing
/// connectors.
#[cfg(feature = "submission")]
fn hosted_route_registry() -> Arc<RouteRegistry> {
    let config = degenbot_config::holder::config();
    let db_path = degenbot_config::resolve_database_path(&degenbot_config::ProcessEnv, None).value;
    match degenbot_submission::backrun_driver::resolve_backrun_node_join() {
        Ok(join) => degenbot_core::runtime::get_runtime().block_on(
            degenbot_submission::backrun_driver::resolve_backrun_host_registry(
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

#[expect(
    clippy::expect_used,
    reason = "a freshly minted host has no registered strategies, so both registers are infallible"
)]
pub(crate) fn boot_host() -> BootedHost {
    let (mut host, attached) = StrategyHost::mint(
        hosted_route_registry(),
        Arc::new(NonceAuthority::new(0)),
        EngineChannelHandles::register_on,
    );

    let cfg = degenbot_config::holder::config();
    let backrun_configured = cfg.strategy.name == Some(degenbot_config::StrategyName::Backrun)
        || cfg.strategy.backrun.bid_mode
        || cfg.strategy.backrun.key_file.is_some();

    host.register(StrategyId::new("settlement"), FacetStatus::Configured)
        .expect("fresh host registers settlement");
    host.register(
        StrategyId::new("backrun"),
        if backrun_configured {
            FacetStatus::Configured
        } else {
            FacetStatus::Unconfigured
        },
    )
    .expect("fresh host registers backrun");

    #[cfg(feature = "submission")]
    let head_lanes = {
        // A hosted lane scopes its run-artifacts under the host state root; a
        // boot with no resolvable root leaves `lane_root` unset, so the lane
        // keeps its process-global path.
        if degenbot_config::holder::installed() {
            if let Some(root) = degenbot_submission::resolve_state_root() {
                host.set_state_root(root);
            }
        }
        // The per-strategy submission ledger is the head feed's
        // submission-truth arm: the host refreshes the authority per head and
        // asks this ledger to close outstanding records out, delivering the
        // typed notices to the owning strategy. Both lanes stamp through the
        // same authority and record into the same ledger.
        let ledger = Arc::new(degenbot_submission::SubmissionLedger::new());
        host.attach_reconciler(
            Arc::clone(&ledger) as Arc<dyn degenbot_bot::strategy_host::HeadReconciler>
        );

        let settlement_id = StrategyId::new("settlement");
        let backrun_id = StrategyId::new("backrun");
        let settlement_lane = Arc::new(degenbot_submission::NonceLane::new(
            Arc::clone(host.nonce()),
            Arc::clone(&ledger),
            settlement_id.clone(),
        ));
        // The settlement seam is the Python-driven arm of this one hosted
        // process, so its lane is process-global: every settlement submission
        // stamps through the shared authority once the boot installs it.
        crate::submission::submit::install_settlement_lane(Arc::clone(&settlement_lane));

        let backrun_lane = Arc::new(degenbot_submission::NonceLane::new(
            Arc::clone(host.nonce()),
            Arc::clone(&ledger),
            backrun_id.clone(),
        ));
        let hub = Arc::clone(host.hub());
        let registry = Arc::clone(host.registry());
        host.attach_spawn(
            &backrun_id,
            degenbot_submission::backrun_driver::backrun_spawn_factory(
                Arc::clone(degenbot_config::holder::config_arc()),
                hub,
                Some(registry),
                Arc::clone(&backrun_lane),
            ),
        )
        .expect("fresh host registers the backrun spawn");

        let mut lanes = HeadLanes::new();
        lanes.insert(settlement_id, settlement_lane);
        lanes.insert(backrun_id, backrun_lane);
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

    /// Boot every enabled strategy that registered a spawn factory and retain
    /// one supervision task per driver. Each supervisor awaits its driver's
    /// lane boundary and folds the terminal exit into the host FSM, so a
    /// self-halt becomes a tombstone `strategies()` reports.
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
            self.driver_supervision.lock().push(supervision);
        }
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
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
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
    use degenbot_bot::strategy_host::{DriverExit, DriverState};

    /// The engine's start flow boots a driver that registered a factory and
    /// folds its terminal exit into the FSM, so a self-halt is a queryable
    /// tombstone carried by `strategies()`.
    #[test]
    fn the_start_flow_boots_a_registered_factory_and_folds_its_exit() {
        Python::attach(|py| {
            let engine = PyArbEngine::new(py, None);
            assert!(
                engine.host.lock().has_spawn(&StrategyId::new("backrun")),
                "the engine boot registers the backrun lane's spawn factory"
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
                        .is_some_and(|record| record.state() == DriverState::Halted)
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
                lanes.contains_key(&StrategyId::new("backrun")),
                "the boot installs the backrun head lane"
            );
            drop(lanes);

            settlement_lane.stamp().expect("settlement lane stamps");
            assert!(
                engine.host.lock().has_hosted_activity(),
                "a live lease opens the reconcile guard"
            );
        });
    }
}
