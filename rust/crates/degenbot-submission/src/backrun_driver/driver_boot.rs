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

use degenbot_bot::bot_core::RouteRegistry;
use degenbot_bot::backrun::BackrunConfig;
use degenbot_bot::connector_index::{OnChainLiquidityRanker, V2ConnectorIndex};
use degenbot_bot::strategy_host::{DriverExit, DriverFuture, DriverSpawnFactory};
use degenbot_db::connection::DegenbotDb;
use degenbot_eventhub::Hub;
use degenbot_rpc::provider::AlloyProvider;

use crate::submission_ledger::NonceLane;

use super::driver_loop::BackrunDriver;

/// The chain the driver operates on; the connector index and the per-chain
/// `DEGENBOT_RPC_HTTP_CHAINID_<id>` / `DEGENBOT_RPC_WS_CHAINID_<id>` resolver
/// suffixes both read it.
pub const CHAIN_ID: u64 = 1;

/// The host-shared handles a driver start needs beyond its own config.
pub struct BackrunContext {
    /// The boot DB handle behind the registry's token joins; `None` leaves
    /// the discovery lane shut.
    pub connector_db: Option<DegenbotDb>,
    /// The chain node's `newHeads` WS endpoint; `None` polls.
    pub head_ws_url: Option<String>,
    /// The host's node join, shared so the boot ranker and the driver read one
    /// connection pool.
    pub provider: Arc<AlloyProvider>,
    /// The driver's run-artifact root inside a multi-strategy host, so this
    /// driver's journal never collides with another strategy's. `None` keeps
    /// the process-global state root for the standalone single-strategy
    /// driver (strict parity with any direct caller).
    pub namespace_root: Option<PathBuf>,
    /// The sign-time nonce seam: the one issuer every runtime shape stamps
    /// through. The boot is a host of size N around its
    /// own authority; a hosted driver receives the host's shared nonce lane.
    pub nonce_lane: Arc<NonceLane>,
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

/// The driver's boot recipe: the process config, the host-owned hub and route
/// registry, and the driver's run-artifact scope.
///
/// The driver has exactly ONE boot path — [`Self::into_driver_future`]. The
/// boot may poll it inline (a driver panic still unwinds the process)
/// and a `StrategyHost` spawns it as a driver task (a panic becomes a tombstone
/// at the task boundary), so two boots cannot
/// drift apart.
pub struct BackrunBoot {
    cfg: BackrunConfig,
    hub: Arc<Hub>,
    route_registry: Option<Arc<RouteRegistry>>,
    context: BackrunContext,
}

impl BackrunBoot {
    /// The driver's loop future: it drives [`BackrunDriver::start`] to its
    /// terminal return and reports a clean stop to the host.
    #[must_use]
    pub fn into_driver_future(self) -> DriverFuture {
        let Self {
            cfg,
            hub,
            route_registry,
            context,
        } = self;
        Box::pin(async move {
            let handle = BackrunDriver::start(cfg, hub, route_registry, context).await;
            handle.wait().await;
            DriverExit::Stopped
        })
    }
}

/// Resolve the boot route registry every backrun runtime discovers over, and
/// the opened connector DB the driver's token joins borrow.
///
/// This is the ONE registry-construction seam: a standalone boot (a host
/// of size one) and a hosted driver both hand it the resolved DB path and the
/// driver's node join, so the two runtime shapes cannot drift apart. It opens
/// `db_path` once, loads the V2 connector scan plus its V3 additions, attaches
/// the on-chain ranker, freezes the [`RouteRegistry`], and runs the live
/// rank-evidence probe last when the operator asked for it. A missing or
/// unopenable DB, or a failed index load, answers `None` — the discovery fan
/// stays shut and frames observe, because connectors are never guessed.
#[must_use]
pub async fn resolve_backrun_registry(
    config: &degenbot_config::BotConfig,
    db_path: &Path,
    provider: &Arc<AlloyProvider>,
) -> Option<(Arc<RouteRegistry>, DegenbotDb)> {
    if !db_path.is_file() {
        tracing::debug!(path = %db_path.display(), "connector DB absent - lane disabled");
        return None;
    }
    let (db, _) = match DegenbotDb::open(db_path) {
        Ok(db) => db,
        Err(error) => {
            tracing::warn!(
                error = %error,
                path = %db_path.display(),
                "DEGENBOT_DB_PATH unopenable - lane disabled"
            );
            return None;
        }
    };
    let mut ix =
        match V2ConnectorIndex::load(&db, 1).and_then(|mut ix| ix.load_v3(&db, 1).map(|()| ix)) {
            Ok(ix) => ix,
            Err(error) => {
                tracing::warn!(error = %error, "connector index load failed - lane disabled");
                return None;
            }
        };
    ix.set_ranker(Arc::new(OnChainLiquidityRanker::new(Arc::clone(provider))));
    let registry = Arc::new(RouteRegistry::new(ix));
    tracing::info!(edges = registry.index().len(), "connector index loaded");
    if config.strategy.backrun.rank_evidence {
        match degenbot_bot::connector_index::deep_pair_ranking_evidence(registry.index(), &db).await {
            Ok(()) => tracing::info!("rank evidence: deep USDC/WETH pair tops the ranking"),
            Err(e) => tracing::warn!(evidence = %e, "rank evidence FAILED"),
        }
    }
    Some((registry, db))
}

/// The route registry a hosted boot mints the strategy host over.
///
/// Delegates to [`resolve_backrun_registry`], so a hosted driver discovers over
/// the same DB-backed snapshot a standalone boot builds. A process with
/// no connector DB mints an empty snapshot instead; the driver then observes
/// with discovery shut rather than guessing connectors.
#[must_use]
pub async fn resolve_backrun_host_registry(
    config: &degenbot_config::BotConfig,
    db_path: &Path,
    provider: &Arc<AlloyProvider>,
) -> Arc<RouteRegistry> {
    resolve_backrun_registry(config, db_path, provider)
        .await
        .map_or_else(
            || Arc::new(RouteRegistry::new(V2ConnectorIndex::default())),
            |(registry, _db)| registry,
        )
}

/// Open the connector DB behind the registry's token joins, exactly as the
/// standalone boot does. A missing file leaves the discovery fan shut and
/// frames observe — connectors are never guessed.
#[must_use]
pub fn resolve_backrun_connector_db() -> Option<DegenbotDb> {
    let db_path = degenbot_config::resolve_database_path(&degenbot_config::ProcessEnv, None).value;
    if !db_path.is_file() {
        tracing::debug!(path = %db_path.display(), "connector DB absent - lane discovery shut");
        return None;
    }
    match DegenbotDb::open(&db_path) {
        Ok((db, _)) => Some(db),
        Err(error) => {
            tracing::warn!(%error, path = %db_path.display(), "connector DB unopenable - lane discovery shut");
            None
        }
    }
}

/// Assemble a driver's boot from an already-resolved node join.
///
/// Both a standalone boot (which builds its DB-backed registry over the
/// join) and a hosted driver call this, so the driver's config derivation and
/// context shape have one definition. The chain node's `newHeads` WS endpoint
/// (the fallback when absent) resolves here. `namespace_root` scopes the driver's
/// run-artifacts under a multi-strategy host's state root; `None` keeps the
/// process-global root (standalone parity).
#[must_use]
pub fn backrun_boot(
    config: &degenbot_config::BotConfig,
    join: BackrunNodeJoin,
    hub: Arc<Hub>,
    route_registry: Option<Arc<RouteRegistry>>,
    connector_db: Option<DegenbotDb>,
    namespace_root: Option<PathBuf>,
    nonce_lane: Arc<NonceLane>,
) -> BackrunBoot {
    let cfg = BackrunConfig::from_config(config, join.rpc_url);
    let head_ws_url =
        degenbot_config::resolve_node_ws_uri(&degenbot_config::ProcessEnv, CHAIN_ID, None)
            .ok()
            .map(|resolved| resolved.value);
    let context = BackrunContext {
        connector_db,
        head_ws_url,
        provider: join.provider,
        namespace_root,
        nonce_lane,
    };
    BackrunBoot {
        cfg,
        hub,
        route_registry,
        context,
    }
}

/// The spawn factory a `StrategyHost` registers for the backrun driver.
///
/// The driver resolves its node join and connector DB when the host drives the
/// driver, not when the factory is attached, so a boot that never enables backrun
/// pays for no node connection or DB handle. A join that cannot resolve becomes
/// a `Halted` tombstone naming the missing layer rather than a host unwind; the
/// host-computed lane namespace scopes the driver's artifacts.
#[must_use]
pub fn backrun_spawn_factory(
    config: Arc<degenbot_config::BotConfig>,
    hub: Arc<Hub>,
    route_registry: Option<Arc<RouteRegistry>>,
    nonce_lane: Arc<NonceLane>,
) -> DriverSpawnFactory {
    Box::new(move |namespace| {
        Box::pin(async move {
            match resolve_backrun_node_join() {
                Ok(join) => {
                    let namespace_root = namespace.map(|ns| ns.root().to_path_buf());
                    backrun_boot(
                        &config,
                        join,
                        hub,
                        route_registry,
                        resolve_backrun_connector_db(),
                        namespace_root,
                        nonce_lane,
                    )
                    .into_driver_future()
                    .await
                }
                Err(error) => DriverExit::Halted(format!("backrun driver boot refused: {error}")),
            }
        })
    })
}
