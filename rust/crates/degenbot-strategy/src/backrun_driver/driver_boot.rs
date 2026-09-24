//! The backrun driver's boot handoff: the artifacts a host mints for the
//! driver, and the one recipe that turns them into a running loop.
//!
//! Invariant surface: this module never owns process boot. It resolves the
//! node join from the environment, opens the connector DB, freezes the route
//! registry, and packages everything as a `BackrunBoot` whose only exit is
//! `BackrunBoot::into_driver_future`. A hosted
//! driver call the same resolvers, so the two runtime shapes cannot drift;
//! the spawn factory defers resolution to host-drive time, so a boot that
//! never enables backrun pays for no node connection or DB handle.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::backrun::{BackrunConfig, MevblockerBackrun, TxpoolBackrun};
use crate::execution_context::ExecutionContext;
use crate::strategy_kit::StrategyKit;
use degenbot_bot::bot_core::pool_ingress::{AlloyLiquidityLogSource, AlloySampleVerifier, DbArm};
use degenbot_bot::bot_core::RouteRegistry;
use degenbot_bot::connector_index::{OnChainLiquidityRanker, V2ConnectorIndex};
use degenbot_bot::strategy_host::{DriverExit, DriverFuture, DriverSpawnFactory};
use degenbot_db::connection::DegenbotDb;
use degenbot_eventhub::Hub;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_rpc::AlloyTickBootstrapRpc;

use degenbot_submission::submission_ledger::NonceLane;

use super::driver_loop::BackrunDriver;

/// The chain the driver operates on; the connector index and the per-chain
/// `DEGENBOT_RPC_HTTP_CHAINID_<id>` / `DEGENBOT_RPC_WS_CHAINID_<id>` resolver
/// suffixes both read it.
pub const CHAIN_ID: u64 = 1;

/// The `eth_getLogs` chunk size for the ingress's per-pool backfill fetch:
/// a long Db-to-head lag is closed in ~1000-block requests.
const BACKFILL_LOG_CHUNK_BLOCKS: u64 = 1_000;

/// The strategy-owned boot product consumed by both runtime shapes.
///
/// It carries the held connector DB, frozen registry, discovery graph,
/// `PoolIngress`, and the facet-selected verification policy as one concrete
/// value. The generic host supplies only hub and nonce-lane runtime handles.
pub struct BackrunStrategyBoot {
    /// The one execution deployment built from the operator-configured
    /// executor and the canonical Ethereum V4/WETH identities.
    pub(crate) ecosystem: BackrunEcosystem,
    pub(crate) cfg: BackrunConfig,
    pub(crate) registry: Arc<RouteRegistry>,
    pub(crate) execution: ExecutionContext,
    /// The boot DB handle behind the registry's token joins; `None` leaves
    /// the discovery lane shut. The same held connection the kit's ingress
    /// was built over.
    pub(crate) connector_db: Option<Arc<DegenbotDb>>,
    /// The boot-resolved strategy kit: the provisioning ingress with its
    /// chain arm + chain-sample policy attached, and the discovery handles.
    pub(crate) kit: StrategyKit,
    /// The chain node's `newHeads` WS endpoint; `None` polls.
    pub(crate) head_ws_url: Option<String>,
    pub(crate) provider: Option<Arc<AlloyProvider>>,
    /// The driver's run-artifact root inside a multi-strategy host, so this
    /// driver's journal never collides with another strategy's. `None` keeps
    /// the process-global state root for the standalone single-strategy
    /// driver (strict parity with any direct caller).

    /// The sign-time nonce seam: the one issuer every runtime shape stamps
    /// through. The boot is a host of size N around its
    /// own authority; a hosted driver receives the host's shared nonce lane.
    pub(crate) boot_error: Option<BackrunBootError>,
}

/// Why a backrun driver could not be booted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackrunBootError {
    /// The chain-node HTTP endpoint did not resolve from the process
    /// environment.
    #[error("backrun node join unresolved: {0}")]
    NodeJoin(String),
}

/// The driver's node join: the resolved chain-node HTTP endpoint and the provider
/// built over it. Every boot resolves this the same
/// way, so the boot ranker and the driver read one connection pool.
pub struct BackrunNodeJoin {
    /// The `DEGENBOT_RPC_HTTP_CHAINID_<id>` endpoint the driver signs against.
    pub rpc_url: String,
    /// The shared node join.
    pub provider: Arc<AlloyProvider>,
}

impl BackrunStrategyBoot {
    /// The held connector DB used by token joins and the ingress DB arm.
    #[must_use]
    pub fn connector_db(&self) -> Option<&Arc<DegenbotDb>> {
        self.connector_db.as_ref()
    }

    /// The one frozen route registry used by discovery and membership.
    #[must_use]
    pub fn registry(&self) -> &Arc<RouteRegistry> {
        &self.registry
    }

    /// The concrete strategy kit carried by this product.
    #[must_use]
    pub fn kit(&self) -> &StrategyKit {
        &self.kit
    }

    /// The startup discovery graph derived from the registry.
    #[must_use]
    pub fn dfs(&self) -> Option<&crate::anchored_dfs::AnchoredGraph> {
        self.kit.dfs()
    }

    /// The concrete provisioning ingress, including its verification policy.
    #[must_use]
    pub fn ingress(&self) -> &degenbot_bot::bot_core::pool_ingress::PoolIngress {
        self.kit.ingress()
    }

    /// The verification policy selected by this strategy facet.
    #[must_use]
    pub fn verify_level(&self) -> degenbot_bot::bot_core::pool_ingress::VerifyLevel {
        self.ingress().verify_level()
    }
}

/// The shared, ecosystem-neutral facts resolved once before any driver starts.
///
/// Both concrete backrun compositions are built from this value. It owns the
/// only connector DB handle and the only route registry for the process.
pub struct BackrunBootResources {
    config: Arc<degenbot_config::BotConfig>,
    join: Option<BackrunNodeJoin>,
    connector_db: Option<Arc<DegenbotDb>>,
    registry: Arc<RouteRegistry>,
    boot_error: Option<BackrunBootError>,
}

impl BackrunBootResources {
    /// The registry the generic `StrategyHost` mints over.
    #[must_use]
    pub fn registry(&self) -> &Arc<RouteRegistry> {
        &self.registry
    }

    /// The held DB shared by every concrete strategy product.
    #[must_use]
    pub fn connector_db(&self) -> Option<&Arc<DegenbotDb>> {
        self.connector_db.as_ref()
    }

    /// Build one concrete ecosystem product over the shared facts.
    ///
    /// # Panics
    ///
    /// Panics when the selected facet has a malformed executor address. The
    /// boot preserves the existing loud-failure behavior for invalid config.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "invalid executor config remains a fatal boot error"
    )]
    pub fn strategy_boot(&self, ecosystem: BackrunEcosystem) -> BackrunStrategyBoot {
        let rpc_url = self
            .join
            .as_ref()
            .map_or_else(String::new, |join| join.rpc_url.clone());
        let cfg = ecosystem.config(&self.config, rpc_url);
        let provider = self.join.as_ref().map(|join| Arc::clone(&join.provider));
        let db_arm = self
            .connector_db
            .clone()
            .zip(provider.as_ref())
            .map(|(db, provider)| {
                DbArm::new(
                    db,
                    Arc::new(AlloyLiquidityLogSource::new(
                        Arc::clone(provider),
                        BACKFILL_LOG_CHUNK_BLOCKS,
                    )),
                )
            });
        let kit = StrategyKit::resolve(
            Some(Arc::clone(&self.registry)),
            db_arm,
            provider
                .as_ref()
                .map(|provider| Arc::new(AlloyTickBootstrapRpc::new(Arc::clone(provider))) as _),
            cfg.verify_ticks,
            provider
                .as_ref()
                .map(|provider| Arc::new(AlloySampleVerifier::new(Arc::clone(provider))) as _),
        );
        let head_ws_url =
            degenbot_config::resolve_node_ws_uri(&degenbot_config::ProcessEnv, CHAIN_ID, None)
                .ok()
                .map(|resolved| resolved.value);

        let executor = cfg
            .executor
            .parse()
            .expect("the facet's executor is a valid address");

        BackrunStrategyBoot {
            ecosystem,
            cfg,
            registry: Arc::clone(&self.registry),
            execution: ExecutionContext::ethereum(executor),
            connector_db: self.connector_db.clone(),
            kit,
            head_ws_url,
            provider,
            boot_error: self.boot_error.clone(),
        }
    }
}

/// Resolve the driver's node join from the process environment.
///
/// # Errors
///
/// [`BackrunBootError::NodeJoin`] when the chain-node HTTP endpoint has no
/// layer; the message is the resolver's, which names every layer consulted.
pub fn resolve_backrun_node_join() -> Result<BackrunNodeJoin, BackrunBootError> {
    let rpc_url =
        degenbot_config::resolve_node_http_uri(&degenbot_config::ProcessEnv, CHAIN_ID, None)
            .map_err(|error| BackrunBootError::NodeJoin(error.to_string()))?
            .value;
    let url = rpc_url
        .parse()
        .map_err(|error| BackrunBootError::NodeJoin(format!("{error}")))?;
    let client = alloy::rpc::client::ClientBuilder::default().http(url);
    let provider = Arc::new(AlloyProvider::from_provider(Arc::new(
        alloy::providers::ProviderBuilder::default().connect_client(client),
    )));
    Ok(BackrunNodeJoin { rpc_url, provider })
}

/// The driver's boot recipe: the process config, the host-owned hub, and the
/// boot-resolved strategy kit + run-artifact scope.
///
/// The driver has exactly ONE boot path — [`Self::into_driver_future`]. The
/// boot may poll it inline (a driver panic still unwinds the process)
/// and a `StrategyHost` spawns it as a driver task (a panic becomes a tombstone
/// at the task boundary), so two boots cannot
/// drift apart.
pub struct BackrunBoot {
    strategy: BackrunStrategyBoot,
    hub: Arc<Hub>,
    namespace_root: Option<PathBuf>,
    nonce_lane: Arc<NonceLane>,
}

impl BackrunBoot {
    /// The driver's loop future: it drives [`BackrunDriver::start`] to its
    /// terminal return and reports a clean stop to the host.
    #[must_use]
    pub fn into_driver_future(self) -> DriverFuture {
        let Self {
            strategy,
            hub,
            namespace_root,
            nonce_lane,
        } = self;
        Box::pin(async move {
            if let Some(error) = strategy.boot_error.clone() {
                return DriverExit::Halted(format!("backrun driver boot refused: {error}"));
            }
            let handle = BackrunDriver::start(strategy, hub, namespace_root, nonce_lane).await;
            handle.wait().await;
            DriverExit::Stopped
        })
    }
}

/// Resolve every backrun boot fact once for a process.
///
/// The connector DB is opened exactly once. The returned value owns the
/// held handle and the frozen registry; concrete ecosystem products only
/// derive their kit and policy over those shared facts.
#[must_use]
pub async fn resolve_backrun_boot(
    config: Arc<degenbot_config::BotConfig>,
    db_path: PathBuf,
    join: BackrunNodeJoin,
) -> BackrunBootResources {
    let empty_registry = || Arc::new(RouteRegistry::new(V2ConnectorIndex::default()));
    let mut connector_db = None;
    let mut registry = empty_registry();

    if db_path.is_file() {
        match DegenbotDb::open(&db_path) {
            Ok((db, _)) => {
                match V2ConnectorIndex::load(&db, 1).and_then(|mut ix| {
                    ix.load_v3(&db, 1)?;
                    ix.load_v4(&db, 1)?;
                    ix.load_unsupported(&db, 1)?;
                    Ok(ix)
                }) {
                    Ok(mut ix) => {
                        if let Err(probe) = ix.verify_sampled_layouts(&join.provider).await {
                            tracing::error!(
                                probe = %probe,
                                "V3 fork layout probe FAILED - lane disabled until the fork table is fixed"
                            );
                        } else {
                            tracing::info!(
                                "v3 fork layout probe: sampled layouts agree with the chain"
                            );
                            ix.set_ranker(Arc::new(OnChainLiquidityRanker::new(Arc::clone(
                                &join.provider,
                            ))));
                            let next_registry = Arc::new(RouteRegistry::new(ix));
                            tracing::info!(
                                edges = next_registry.index().len(),
                                "connector index loaded"
                            );
                            if config.strategy.mevblocker_backrun.rank_evidence
                                || config.strategy.txpool_backrun.rank_evidence
                            {
                                match degenbot_bot::connector_index::deep_pair_ranking_evidence(
                                    next_registry.index(),
                                    &db,
                                )
                                .await
                                {
                                    Ok(()) => tracing::info!(
                                        "rank evidence: deep USDC/WETH pair tops the ranking"
                                    ),
                                    Err(error) => {
                                        tracing::warn!(evidence = %error, "rank evidence FAILED");
                                    }
                                }
                            }
                            registry = next_registry;
                            connector_db = Some(Arc::new(db));
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            "connector index load failed - lane disabled"
                        );
                    }
                }
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    path = %db_path.display(),
                    "DEGENBOT_DB_PATH unopenable - lane disabled"
                );
            }
        }
    } else {
        tracing::debug!(path = %db_path.display(), "connector DB absent - lane disabled");
    }

    BackrunBootResources {
        config,
        join: Some(join),
        connector_db,
        registry,
        boot_error: None,
    }
}

impl BackrunBootResources {
    /// Build an unresolved product for a host whose node join is absent.
    /// The host still receives a coherent empty registry, and the driver
    /// reports the same typed node-join halt at its driving edge.
    #[must_use]
    pub fn unresolved(config: Arc<degenbot_config::BotConfig>, error: BackrunBootError) -> Self {
        Self {
            config,
            join: None,
            connector_db: None,
            registry: Arc::new(RouteRegistry::new(V2ConnectorIndex::default())),
            boot_error: Some(error),
        }
    }
}

/// Which per-ecosystem backrun composition a boot constructs. The two
/// compositions are distinct concrete types; this value only routes a boot to
/// the right facet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackrunEcosystem {
    /// The `MEVBlocker` private-auction composition.
    Mevblocker,
    /// The builder-relay composition: the chain node txpool feed plus the
    /// Flashbots-compatible relay fan-out.
    Txpool,
}

impl BackrunEcosystem {
    /// Build the composition's driver config from its facet.
    #[must_use]
    fn config(self, cfg: &degenbot_config::BotConfig, rpc_url: String) -> BackrunConfig {
        match self {
            Self::Mevblocker => MevblockerBackrun::from_config(cfg, rpc_url).into_config(),
            Self::Txpool => TxpoolBackrun::from_config(cfg, rpc_url).into_config(),
        }
    }
}

/// Install the process-global frame-trace sink: one session artifact
/// directory under `logging.runs_dir` per driver boot. The JSONL records
/// every frame's replay/extract/admit/decision story — without this the
/// live run's frame evidence is a silent no-op (`set_trace_jsonl_default`
/// was only ever called by tests). At-most-once per process: with two
/// hosted ecosystems the SECOND boot's call is the no-op, so both facets
/// share one session's trace rather than fighting over the sink.
///
/// A failure to mint the directory degrades capture only — the boot
/// proceeds, `trace_jsonl` stays absent, and the WARN says so (the
/// per-frame INFO + counter still carry the signals).
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "ecosystem names the engine directory without consuming it"
)]
fn install_frame_trace_sink(ecosystem: &BackrunEcosystem) {
    install_frame_trace_sink_under(None, ecosystem);
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "ecosystem names the engine directory without consuming it"
)]
fn install_frame_trace_sink_under(root: Option<&Path>, ecosystem: &BackrunEcosystem) {
    if degenbot_runs::trace_jsonl_default().is_some() {
        return; // first driver in the process owns the session trace
    }
    let engine = match ecosystem {
        BackrunEcosystem::Mevblocker => "backrun-mevblocker",
        BackrunEcosystem::Txpool => "backrun-peer",
    };
    let run = match root {
        Some(root) => degenbot_runs::RunDirectory::create_in(root, engine),
        None => degenbot_runs::RunDirectory::create(engine),
    };
    match run {
        Ok(run) => {
            let _ = degenbot_runs::set_trace_jsonl_default(run.trace_jsonl_path().to_path_buf());
            tracing::info!(path = %run.session_dir().display(), "frame trace artifact session created");
        }
        Err(error) => {
            tracing::warn!(%error, "frame-trace artifact directory failed - JSONL capture disabled");
        }
    }
}

/// Assemble a runnable driver from the strategy-owned boot product.
///
/// The standalone and hosted paths call this same function. It performs no
/// registry, DB, node, or per-ecosystem resolution; those facts were already
/// composed by [`resolve_backrun_boot`] and [`BackrunBootResources`].
#[must_use]
pub fn backrun_boot(
    strategy: BackrunStrategyBoot,
    hub: Arc<Hub>,
    namespace_root: Option<PathBuf>,
    nonce_lane: Arc<NonceLane>,
) -> BackrunBoot {
    install_frame_trace_sink(&strategy.ecosystem);
    BackrunBoot {
        strategy,
        hub,
        namespace_root,
        nonce_lane,
    }
}

/// The spawn factory a `StrategyHost` registers for a concrete backrun
/// strategy. The product is moved into the factory once; starting the driver
/// never reopens the DB or reconstructs its registry.
#[must_use]
pub fn backrun_spawn_factory(
    strategy: BackrunStrategyBoot,
    hub: Arc<Hub>,
    nonce_lane: Arc<NonceLane>,
) -> DriverSpawnFactory {
    Box::new(move |namespace| {
        let namespace_root = namespace.map(|ns| ns.root().to_path_buf());
        Box::pin(async move {
            backrun_boot(strategy, hub, namespace_root, nonce_lane)
                .into_driver_future()
                .await
        })
    })
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod trace_install_tests {
    use super::*;

    /// The install mints a session directory + seeds `trace.jsonl`, and the
    /// second ecosystem's boot cannot steal the at-most-once sink.
    #[test]
    fn install_marks_the_session_and_is_first_driver_wins() {
        let root = tempfile::tempdir().unwrap();
        install_frame_trace_sink_under(Some(root.path()), &BackrunEcosystem::Mevblocker);
        let first = degenbot_runs::trace_jsonl_default()
            .map(std::path::Path::to_path_buf)
            .expect("install installs a session trace default");
        assert!(
            first.is_file(),
            "trace.jsonl seeded on disk: {}",
            first.display()
        );
        assert!(first.to_string_lossy().contains("backrun-mevblocker"));

        install_frame_trace_sink_under(Some(root.path()), &BackrunEcosystem::Txpool);
        let after = degenbot_runs::trace_jsonl_default()
            .map(std::path::Path::to_path_buf)
            .expect("the default stays installed");
        assert_eq!(
            first, after,
            "second ecosystem's boot must not steal the sink"
        );
    }
}
