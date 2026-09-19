//! The standalone backrun sidecar binary.
//!
//! A thin single-driver host: it owns the process boot (config load, the arm
//! gate, tracing, the shared node join, the connector registry, and one hub)
//! and hands the lane to [`BackrunDriver`]. Everything frame-bound — the
//! feed, the head watch, the replay runtime, the quarantine FSM, and the loop
//! — lives in the driver module. The external behavior (config reading,
//! journal root, systemd unit, liveness conventions, kill-switch path) is
//! unchanged.

#![expect(
    clippy::expect_used,
    reason = "bin: fatal config failures exit the process loudly"
)]
#![expect(
    clippy::print_stderr,
    reason = "bin: boot/config failures must reach the operator before tracing is installed"
)]

use std::sync::Arc;

use degenbot_bot::sidecar::SidecarConfig;
use degenbot_eventhub::Hub;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_submission::backrun_driver::{BackrunContext, BackrunDriver, CHAIN_ID};

// Session telemetry: the fmt subscriber appends to the session's
// `stdout.log` (file-only by default) so a run's console output survives the
// process, while `logging.log_stderr` duplicates it to stderr for
// interactive runs. The session's `trace.jsonl` becomes the trace helpers'
// default capture path; an explicit `logging.trace_jsonl` still wins. A run
// directory that cannot be created degrades to stderr — capturing logs must
// never abort the bot. Structured (OTel) export stays the operator's
// layering choice via the bot crate.
fn init_tracing(mirror_stderr: bool) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    match degenbot_runs::RunDirectory::create("backrun-sidecar") {
        Ok(run) => {
            let _ = degenbot_runs::set_trace_jsonl_default(run.trace_jsonl_path().to_path_buf());
            let writer = if mirror_stderr {
                run.stdout_writer_tee()
            } else {
                run.stdout_writer()
            };
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(mirror_stderr)
                .with_writer(writer)
                .try_init();
            tracing::info!(
                session_dir = %run.session_dir().display(),
                stdout = %run.stdout_path().display(),
                trace_jsonl = %run.trace_jsonl_path().display(),
                "sidecar run artifacts"
            );
        }
        Err(error) => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .try_init();
            tracing::warn!(error = %error, "run directory unavailable - logging to stderr");
        }
    }
}

/// The strategy-arm boot gate: this binary is the pending-transaction
/// backrun arm, so `strategy.name` must be `backrun` or unset. An explicit
/// `settlement` selection names the wrong invocation and is refused.
fn strategy_arm_refusal(arm: Option<degenbot_config::StrategyName>) -> Option<String> {
    match arm {
        None | Some(degenbot_config::StrategyName::Backrun) => None,
        Some(degenbot_config::StrategyName::Settlement) => Some(String::from(
            "strategy.name=settlement: this binary is the pending-transaction backrun sidecar; \
             boot `backrun_sidecar` itself with a typed config carrying `[strategy] name = \
             \"backrun\"` (or DEGENBOT_STRATEGY_NAME=backrun), or run the settled-block arm instead",
        )),
    }
}

#[tokio::main]
async fn main() {
    let loaded = degenbot_config::BotConfigLoader::new()
        .with_standard_file_paths()
        .load()
        .unwrap_or_else(|error| {
            eprintln!("backrun sidecar config load failed: {error}");
            std::process::exit(2);
        });
    if let Some(refusal) = strategy_arm_refusal(loaded.config.strategy.name) {
        eprintln!("{refusal}");
        std::process::exit(2);
    }
    let config = std::sync::Arc::new(loaded.config.clone());
    let _ = degenbot_config::holder::install(std::sync::Arc::clone(&config));
    init_tracing(config.logging.log_stderr);

    let env = degenbot_config::ProcessEnv;
    let rpc_url = degenbot_config::resolve_node_http_uri(&env, CHAIN_ID, None)
        .unwrap_or_else(|error| {
            eprintln!("{error}");
            std::process::exit(2);
        })
        .value;
    let cfg = SidecarConfig::from_config(&config, rpc_url);

    // The host's node join: built once here so the boot ranker and the lane
    // share one connection pool.
    let client = alloy::rpc::client::ClientBuilder::default().http(
        cfg.rpc_url
            .parse()
            .expect("DEGENBOT_RPC_HTTP_CHAINID_1 is a valid http url"),
    );
    let provider = Arc::new(AlloyProvider::from_provider(Arc::new(
        alloy::providers::ProviderBuilder::default().connect_client(client),
    )));

    // The DB-backed connector index -- ONE startup scan, never a
    // per-frame query. The path comes from the shared `DEGENBOT_DB_PATH`
    // resolver; a missing file leaves the discovery fan shut and frames
    // observe (connectors are never guessed).
    let db_path = degenbot_config::resolve_database_path(&env, None).value;
    let (route_registry, connector_db): (
        Option<std::sync::Arc<degenbot_bot::bot_core::RouteRegistry>>,
        Option<degenbot_db::connection::DegenbotDb>,
    ) = if db_path.is_file() {
        match degenbot_db::connection::DegenbotDb::open(&db_path) {
            Ok((db, _)) => match degenbot_bot::sidecar_paths::V2ConnectorIndex::load(&db, 1)
                .and_then(|mut ix| ix.load_v3(&db, 1).map(|()| ix))
            {
                Ok(mut ix) => {
                    ix.set_ranker(Arc::new(
                        degenbot_bot::sidecar_paths::OnChainLiquidityRanker::new(Arc::clone(
                            &provider,
                        )),
                    ));
                    let registry = Arc::new(degenbot_bot::bot_core::RouteRegistry::new(ix));
                    tracing::info!(edges = registry.index().len(), "connector index loaded");
                    // Evidence mode (`strategy.backrun.rank_evidence`): a LIVE
                    // sanity probe before any frame trusts the depth
                    // truncation -- the canonical deep USDC/WETH pair must
                    // top the ranking.
                    if config.strategy.backrun.rank_evidence {
                        match degenbot_bot::sidecar_paths::deep_pair_ranking_evidence(
                            registry.index(),
                            &db,
                        )
                        .await
                        {
                            Ok(()) => {
                                tracing::info!(
                                    "rank evidence: deep USDC/WETH pair tops the ranking"
                                );
                            }
                            Err(e) => tracing::warn!(evidence = %e, "rank evidence FAILED"),
                        }
                    }
                    (Some(registry), Some(db))
                }
                Err(e) => {
                    tracing::warn!(error = %e, "connector index load failed - lane disabled");
                    (None, None)
                }
            },
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %db_path.display(),
                    "DEGENBOT_DB_PATH unopenable - lane disabled"
                );
                (None, None)
            }
        }
    } else {
        tracing::debug!(path = %db_path.display(), "connector DB absent - lane disabled");
        (None, None)
    };

    // Head source for the live loop: a `newHeads` subscription over a dedicated
    // WS endpoint (the MEVBlocker frame feed and the chain node are different
    // hosts, so the head WS is its own URL). The hub owns the latest head and
    // its staleness; `HeadWatch` only performs the subscribe + reconnect and
    // publishes into the hub. The 200ms `eth_blockNumber` poll is the FALLBACK,
    // not the primary source.
    let head_ws_url = degenbot_config::resolve_node_ws_uri(&env, CHAIN_ID, None)
        .ok()
        .map(|resolved| resolved.value);

    let hub = Arc::new(Hub::new());
    let ctx = BackrunContext {
        connector_db,
        head_ws_url,
        provider,
    };
    let driver = BackrunDriver::start(cfg, hub, route_registry, ctx).await;
    driver.wait().await;
}

#[cfg(test)]
mod tests {
    use super::strategy_arm_refusal;

    #[test]
    fn backrun_arm_is_own_binary_and_settlement_refused() {
        use degenbot_config::StrategyName;

        assert!(strategy_arm_refusal(None).is_none());
        assert!(strategy_arm_refusal(Some(StrategyName::Backrun)).is_none());
        let refusal = strategy_arm_refusal(Some(StrategyName::Settlement)).expect("refused");
        assert!(refusal.contains("backrun"), "{refusal}");
        assert!(refusal.contains("strategy.name"), "{refusal}");
    }
}
