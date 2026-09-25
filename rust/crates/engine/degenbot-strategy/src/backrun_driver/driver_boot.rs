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
    /// No layer named the chain this session runs against. The endpoint
    /// tables and the connector index are both keyed by chain id, so a boot
    /// that cannot name its chain has nothing to select and refuses rather
    /// than assuming one.
    #[error("backrun session chain unresolved: {0}")]
    SessionChain(String),
    /// The endpoint the join names serves a different chain than the join was
    /// resolved for. A node pointed at another chain answers every later read
    /// with that other chain, so the boot refuses here — before the connector
    /// DB is opened and before any lane is built.
    #[error("node endpoint {endpoint} reports chain id {actual}, not the resolved session chain {chain_id}")]
    ChainMismatch {
        /// The chain the join resolved for.
        chain_id: u64,
        /// The chain the endpoint actually serves.
        actual: u64,
        /// The endpoint the join named.
        endpoint: String,
    },

    /// The connector DB yielded no usable roster for the configured chain. A
    /// database built for another chain reads as an EMPTY index (every load
    /// filters by chain), so this refusal is the only thing a mismatched
    /// boot would otherwise never see.
    #[error("connector index for chain {chain_id} unusable at {path}: {reason}")]
    ConnectorIndex {
        /// The chain the boot needed rows for.
        chain_id: u64,
        /// The database the boot read.
        path: String,
        /// Why that database cannot serve the chain.
        reason: String,
    },
}

/// The driver's node join: the resolved chain-node HTTP endpoint and the provider
/// built over it. Every boot resolves this the same
/// way, so the boot ranker and the driver read one connection pool.
pub struct BackrunNodeJoin {
    /// The `DEGENBOT_RPC_HTTP_CHAINID_<id>` endpoint the driver signs against.
    pub rpc_url: String,
    /// The shared node join.
    pub provider: Arc<AlloyProvider>,
    /// The chain this join was resolved for. The join carries it so every
    /// downstream artifact (the connector index, the head feed) reads ONE
    /// chain: a consumer that chose its own could disagree with the node the
    /// driver signs against.
    pub chain_id: u64,
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
    /// The typed refusal this boot carries, if any. A refusing boot still
    /// hands the host a coherent product; the driver reports the error at its
    /// driving edge instead of running on facts the boot could not resolve.
    #[must_use]
    pub fn boot_error(&self) -> Option<&BackrunBootError> {
        self.boot_error.as_ref()
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
        // The head feed reads the chain the join named, so the feed and the
        // endpoint the driver signs against can never be two chains. A
        // joinless boot names no chain and so carries no feed; it halts on
        // its boot error regardless.
        let head_ws_url = self.join.as_ref().and_then(|join| {
            degenbot_config::load_process_config()
                .ok()
                .and_then(|loaded| head_ws_url(&loaded, join.chain_id))
        });

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

/// The `newHeads` feed the subscription scope names for `chain_id`, or `None`
/// when no layer supplies a feed-capable transport (the driver polls).
pub(crate) fn head_ws_url(loaded: &degenbot_config::LoadedConfig, chain_id: u64) -> Option<String> {
    degenbot_config::resolve_node_subscription_uri(
        loaded,
        chain_id,
        &degenbot_config::NodeOverrides::new(),
    )
    .ok()
    .map(|resolved| resolved.value)
}

/// Resolve the driver's node join, and the chain it is FOR, from the layers the
/// host booted with: the operator file the host selected, the per-chain env
/// families, and the declared defaults, plus the host's explicit chain argument
/// when it has one.
///
/// The chain is resolved ONCE here, ahead of any endpoint, because both the
/// endpoint tables and the connector index are keyed by it: a boot that chose a
/// chain here would resolve another chain's node and then read a database for
/// that same other chain. The resolved value rides out on the join, so no
/// later consumer picks a second one.
///
/// The scope is `request`, so an operator's local `nodes.ipc` entry is the
/// endpoint this consumer gets. The client below is still the hardcoded HTTP
/// one; taking an injected capability-scoped provider at construction is the
/// separate change that makes the ipc entry dialable here.
///
/// # Errors
///
/// [`BackrunBootError::SessionChain`] when no layer named the session chain
/// (the message names every layer consulted), or
/// [`BackrunBootError::NodeJoin`] when the resolved chain's request endpoint
/// has no layer at all (the message is the resolver's, which names the chain,
/// every layer, and every transport it consulted).
pub fn resolve_backrun_node_join(
    loaded: &degenbot_config::LoadedConfig,
    cli_chain_id: Option<&str>,
) -> Result<BackrunNodeJoin, BackrunBootError> {
    let chain_id = degenbot_config::resolve_chain_id(loaded, cli_chain_id)
        .map_err(|error| BackrunBootError::SessionChain(error.to_string()))?
        .value;
    let rpc_url = degenbot_config::resolve_node_request_uri(
        loaded,
        chain_id,
        &degenbot_config::NodeOverrides::new(),
    )
    .map_err(|error| BackrunBootError::NodeJoin(error.to_string()))?
    .value;
    let url = rpc_url
        .parse()
        .map_err(|error| BackrunBootError::NodeJoin(format!("{error}")))?;
    let client = alloy::rpc::client::ClientBuilder::default().http(url);
    let provider = Arc::new(AlloyProvider::from_provider(Arc::new(
        alloy::providers::ProviderBuilder::default().connect_client(client),
    )));
    Ok(BackrunNodeJoin {
        rpc_url,
        provider,
        chain_id,
    })
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
/// The connector DB is opened exactly once, for the chain its node join named.
/// The returned value owns the held handle and the frozen registry; concrete
/// ecosystem products only derive their kit and policy over those shared facts.
///
/// A database that cannot serve that chain yields a product carrying a
/// [`BackrunBootError::ConnectorIndex`] rather than an empty registry the
/// driver would run on.
#[must_use]
pub async fn resolve_backrun_boot(
    config: Arc<degenbot_config::BotConfig>,
    db_path: PathBuf,
    join: BackrunNodeJoin,
) -> BackrunBootResources {
    let chain_id = join.chain_id;
    // The endpoint is bound to the chain the join already resolved, so the
    // guard reads `join.chain_id` rather than resolving a chain of its own:
    // a second resolution could name a different chain than the connector
    // index below loads. Only a disagreement refuses — an endpoint that
    // cannot be reached is the same inconclusive-at-boot case the layout
    // probe treats as "unverified this boot", and a hermetic boot has no
    // node at all.
    let binding = join.provider.bind_to_chain(chain_id).await;
    match binding {
        Ok(()) => {}
        Err(degenbot_core::errors::ProviderError::ChainMismatch { actual, .. }) => {
            return BackrunBootResources {
                boot_error: Some(BackrunBootError::ChainMismatch {
                    chain_id,
                    actual,
                    endpoint: join.rpc_url.clone(),
                }),
                config,
                join: Some(join),
                connector_db: None,
                registry: empty_registry(),
            };
        }
        Err(error) => {
            tracing::warn!(
                chain_id,
                %error,
                "chain binding unverified this boot - the node could not be asked which chain it serves"
            );
        }
    }
    // The roster is read for the chain the join resolved, and every load
    // filters by it: a database built for another chain yields an EMPTY
    // index rather than an error, so the emptiness check in the roster is
    // what turns a wrong-chain database into a refusal instead of a
    // well-formed, wrong boot.
    let Ok(chain_column) = i64::try_from(chain_id) else {
        return BackrunBootResources {
            config,
            join: Some(join),
            connector_db: None,
            registry: empty_registry(),
            boot_error: Some(BackrunBootError::ConnectorIndex {
                chain_id,
                path: db_path.display().to_string(),
                reason: OVERSIZED_CHAIN_COLUMN_REASON.to_string(),
            }),
        };
    };
    let roster = load_chain_roster(&db_path, chain_id, chain_column, &join, &config).await;
    BackrunBootResources {
        config,
        join: Some(join),
        connector_db: roster.connector_db,
        registry: roster.registry,
        boot_error: roster.boot_error,
    }
}

/// The empty registry a refusing or database-less boot hands the host: a
/// coherent product, never a fabricated roster.
fn empty_registry() -> Arc<RouteRegistry> {
    Arc::new(RouteRegistry::new(V2ConnectorIndex::default()))
}

/// Why an index with no rows is a refusal rather than an empty lane: the
/// database answered, and what it holds is not this chain's connectors.
const EMPTY_CHAIN_ROSTER_REASON: &str =
    "the database holds no pools for this chain (a database built for another chain, or one \
     whose discovery has not run)";

/// Why a chain id the DB cannot address is a refusal rather than a truncated
/// read: a partial chain is not a chain.
const OVERSIZED_CHAIN_COLUMN_REASON: &str =
    "the connector DB stores chain ids in a signed column that cannot hold this chain id";

/// One chain's connector roster: the frozen registry, the held DB, and the
/// refusal when the database cannot serve the chain. Every load filters by the
/// same chain, so this value can never merge two chains' rows.
struct ChainRoster {
    registry: Arc<RouteRegistry>,
    connector_db: Option<Arc<DegenbotDb>>,
    boot_error: Option<BackrunBootError>,
}

/// Read `chain_id`'s roster out of the boot's database.
///
/// An absent or unopenable database degrades to the discovery-shut lane (a
/// process that has not run discovery yet is a normal boot); a database that
/// opens and then cannot answer for the configured chain is a refusal, because
/// the only ways to get there are a wrong-chain database or a database whose
/// schema the load cannot read.
async fn load_chain_roster(
    db_path: &Path,
    chain_id: u64,
    chain_column: i64,
    join: &BackrunNodeJoin,
    config: &degenbot_config::BotConfig,
) -> ChainRoster {
    let mut connector_db = None;
    let mut registry = empty_registry();
    let mut boot_error = None;
    if db_path.is_file() {
        match DegenbotDb::open(db_path) {
            Ok((db, _)) => match load_connector_roster(&db, chain_column) {
                Ok(ix) if ix.is_empty() => {
                    tracing::error!(
                        chain_id,
                        path = %db_path.display(),
                        "connector index has no rows for the configured chain - lane refused"
                    );
                    boot_error = Some(BackrunBootError::ConnectorIndex {
                        chain_id,
                        path: db_path.display().to_string(),
                        reason: EMPTY_CHAIN_ROSTER_REASON.to_string(),
                    });
                }
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
                Err(reason) => {
                    tracing::error!(
                        chain_id,
                        reason,
                        path = %db_path.display(),
                        "connector index load failed - lane refused"
                    );
                    boot_error = Some(BackrunBootError::ConnectorIndex {
                        chain_id,
                        path: db_path.display().to_string(),
                        reason,
                    });
                }
            },
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

    ChainRoster {
        registry,
        connector_db,
        boot_error,
    }
}

/// The whole connector roster for `chain_id`: the V2 scan, then its V3, V4 and
/// unsupported-family additions. Every load filters by the same chain, so the
/// roster is a single chain's view of the database and never a merge of two.
fn load_connector_roster(db: &DegenbotDb, chain_id: i64) -> Result<V2ConnectorIndex, String> {
    let mut ix = V2ConnectorIndex::load(db, chain_id).map_err(|error| error.to_string())?;
    ix.load_v3(db, chain_id)
        .map_err(|error| error.to_string())?;
    ix.load_v4(db, chain_id)
        .map_err(|error| error.to_string())?;
    ix.load_unsupported(db, chain_id)
        .map_err(|error| error.to_string())?;
    Ok(ix)
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
