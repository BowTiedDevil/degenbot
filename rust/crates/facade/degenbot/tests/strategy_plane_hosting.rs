//! Cross-crate hosting integration: the strategy plane's compositions admit
//! through the one host verb surface. These tests span `degenbot-bot`'s host
//! FSM and `degenbot-strategy`'s compositions, so they live behind the
//! umbrella crate where both are production dependencies.

#![expect(clippy::expect_used, reason = "test assertions fail loudly")]

use degenbot::bot::arb_engine::EngineStages;
use degenbot::bot::hosted_sources::HostedHeadClock;
use degenbot::bot::strategy_host::{DriverPose, FacetStatus, StrategyHost};
use degenbot::bot_core::Bot;
use degenbot::config::BotConfig;
use degenbot::eventhub::{Hub, PendingTxSource};
use degenbot::rpc::backrun_feed::BackrunFeedConfig;
use degenbot::rpc::pending_tx_stream::{PendingTxFeedConfig, PendingTxStream};
use degenbot::rpc::txpool_feed::TxpoolFeedConfig;
use degenbot::strategy::{MevblockerBackrun, StrategyName, TxpoolBackrun};
use degenbot::substrate::connector_index::V2ConnectorIndex;
use degenbot::substrate::driver_spawn::DriverExit;
use degenbot::substrate::nonce::{NonceAuthority, StrategyId};
use degenbot::substrate::session_registry::SessionObjectRegistry;
use degenbot::substrate::state_lock::StateLock;
use degenbot::substrate::BotState;
use degenbot::substrate::RouteRegistry;
use degenbot::{EngineSession, FacetBoot, HostedArmBoot, HostedBoot, SessionConstruction};
use std::sync::Arc;

/// Both per-ecosystem compositions ride one `StrategyHost`: each is
/// independently activatable, and one config with both facets active binds
/// the divergent submission slots (the `MEVBlocker` arm's private endpoint,
/// the peer arm's public fan-out).
#[test]
fn both_backrun_compositions_host_independently_in_one_process() {
    let mut cfg = BotConfig::default();
    cfg.strategy.mevblocker_backrun.active = true;
    cfg.strategy.mevblocker_backrun.endpoints = Some(String::from("wss://searchers.mevblocker.io"));
    cfg.strategy.mevblocker_backrun.mevblocker_url =
        Some(String::from("http://private.local:8545"));
    cfg.strategy.txpool_backrun.active = true;
    cfg.strategy.txpool_backrun.endpoints = Some(String::from("http://relay.one:8545"));

    let mevblocker = MevblockerBackrun::from_config(&cfg, String::new());
    let peer = TxpoolBackrun::from_config(&cfg, String::new());
    assert_eq!(
        mevblocker.config().submission.raw_relay_urls(),
        vec![String::from("http://private.local:8545")]
    );
    assert!(mevblocker.config().submission.names_private_endpoint());
    assert!(
        peer.config().submission.raw_relay_urls().is_empty(),
        "the builder-relay slot names no raw fan-out: the bundle POST is the submission"
    );
    assert!(!peer.config().submission.names_private_endpoint());

    let mut host = StrategyHost::new(
        Arc::new(Hub::new()),
        Arc::new(RouteRegistry::new(V2ConnectorIndex::default())),
        Arc::new(NonceAuthority::new(1)),
    );
    let mevblocker_id = StrategyId::new("mevblocker_backrun");
    let peer_id = StrategyId::new("txpool_backrun");
    host.register(mevblocker_id.clone(), FacetStatus::Configured)
        .expect("register mevblocker");
    host.register(peer_id.clone(), FacetStatus::Configured)
        .expect("register peer");

    assert_eq!(host.enable(&mevblocker_id), Ok(DriverPose::Enabled));
    assert_eq!(
        host.state_of(&peer_id),
        Some(DriverPose::Registered),
        "enabling one facet leaves the other dormant"
    );
    assert_eq!(host.enable(&peer_id), Ok(DriverPose::Enabled));
    assert_eq!(host.state_of(&mevblocker_id), Some(DriverPose::Enabled));
}

/// The three strategies admit through the one registration surface: each
/// `StrategyName` registers under its own id, settlement and a backrun enable
/// independently in one process.
#[test]
fn all_three_strategies_register_and_enable_through_one_verb_surface() {
    let mut host = StrategyHost::new(
        Arc::new(Hub::new()),
        Arc::new(RouteRegistry::new(V2ConnectorIndex::default())),
        Arc::new(NonceAuthority::new(0)),
    );
    for name in StrategyName::ALL {
        host.register(StrategyId::new(name.as_str()), FacetStatus::Configured)
            .expect("register each plane name");
    }
    assert_eq!(
        host.list()
            .iter()
            .map(|record| record.id().as_str())
            .collect::<Vec<_>>(),
        StrategyName::ALL.map(StrategyName::as_str)
    );

    let settlement = StrategyId::new(StrategyName::Settlement.as_str());
    let peer = StrategyId::new(StrategyName::TxpoolBackrun.as_str());
    assert_eq!(host.enable(&settlement), Ok(DriverPose::Enabled));
    assert_eq!(
        host.state_of(&peer),
        Some(DriverPose::Registered),
        "enabling settlement leaves a backrun dormant"
    );
    assert_eq!(host.enable(&peer), Ok(DriverPose::Enabled));
    assert_eq!(host.state_of(&settlement), Some(DriverPose::Enabled));
}

/// A closed local port: every dial is refused immediately, so a feed's
/// reconnect counter witnesses the dial without a live endpoint.
const CLOSED_FEED: &str = "ws://127.0.0.1:1";

/// The construction products plus the in-memory route registry and nonce
/// authority the umbrella probe composes over — no database, no provider.
fn probe_construction() -> SessionConstruction {
    let core = Arc::new(StateLock::new(BotState::new()));
    let bot = Arc::new(Bot::with_core(
        Arc::clone(&core),
        Arc::new(SessionObjectRegistry::new(1)),
    ));
    let stages = Arc::new(EngineStages::with_core(core, bot.active_delta()));
    SessionConstruction {
        bot,
        stages,
        registry: Arc::new(RouteRegistry::new(V2ConnectorIndex::default())),
        nonce: Arc::new(NonceAuthority::new(1)),
        reconciler: None,
        state_root: None,
    }
}

/// A fake hosted arm whose driver loop reports what the start boundary handed
/// it: its own typed stream, and whether the hub head source was already up.
fn probe_arm(id: &str, source: PendingTxSource) -> HostedArmBoot {
    HostedArmBoot {
        id: StrategyId::new(id),
        source,
        feed: Some(match source {
            PendingTxSource::Mevblocker => PendingTxFeedConfig::Mevblocker(BackrunFeedConfig {
                url: CLOSED_FEED.to_string(),
                ..BackrunFeedConfig::for_mainnet()
            }),
            PendingTxSource::Txpool => PendingTxFeedConfig::Txpool(TxpoolFeedConfig {
                ws_url: CLOSED_FEED.to_string(),
                ..TxpoolFeedConfig::defaults()
            }),
        }),
        spawn: Box::new(move |hub: &Arc<Hub>, stream| {
            let hub = Arc::clone(hub);
            let stream_kind = stream.as_ref().map(PendingTxStream::source);
            Box::new(move |_lane| {
                let hub = Arc::clone(&hub);
                Box::pin(async move {
                    let head_source_live = hub.subscribe_head().is_ok();
                    DriverExit::Halted(format!(
                        "stream={stream_kind:?} head_source_live={head_source_live}"
                    ))
                })
            })
        }),
    }
}

/// Wait for a driver's tombstone and return its detail.
fn await_detail(session: &EngineSession, id: &StrategyId) -> String {
    for _ in 0..1_000 {
        let detail = session
            .host()
            .lock()
            .record(id)
            .and_then(|record| record.halt_detail().map(str::to_string));
        if let Some(detail) = detail {
            return detail;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    String::from("no tombstone")
}

/// `cargo add degenbot` boots the hosted strategy plane: the core boot owns the
/// mint → register → attach → start order, so a pure-Rust consumer composes the
/// hosted arms without the `PyO3` shell — and the composition dials nothing
/// until the consumer crosses the start boundary.
#[test]
fn the_umbrella_boots_the_hosted_strategy_plane() {
    let session = EngineSession::boot(
        probe_construction(),
        StrategyName::ALL
            .into_iter()
            .map(|name| FacetBoot {
                id: StrategyId::new(name.as_str()),
                facet: FacetStatus::Configured,
            })
            .collect(),
        Some(HostedBoot {
            arms: vec![
                probe_arm("mevblocker_backrun", PendingTxSource::Mevblocker),
                probe_arm("txpool_backrun", PendingTxSource::Txpool),
            ],
            clock: Some(HostedHeadClock::Hub),
            trigger: None,
        }),
    )
    .expect("the umbrella boots the hosted strategy plane");

    // Provisioned, not started: one pump per arm, no head edge, no driver loop.
    assert_eq!(
        session
            .hosted()
            .expect("the boot hosts the arms' sources")
            .pump_sources(),
        vec![PendingTxSource::Mevblocker, PendingTxSource::Txpool],
        "one pump per hosted arm"
    );
    let hub = Arc::clone(session.host().lock().hub());
    assert!(
        hub.subscribe_head().is_err(),
        "the mint registers no head source, so nothing dialed"
    );

    let arms = ["mevblocker_backrun", "txpool_backrun"];
    for name in arms {
        let id = StrategyId::new(name);
        assert_eq!(
            session.host().lock().state_of(&id),
            Some(DriverPose::Registered),
            "{name} waits for the operator's enable"
        );
        session
            .host()
            .lock()
            .enable(&id)
            .expect("a configured arm admits");
    }

    let started = degenbot::core::runtime::get_runtime()
        .block_on(session.start())
        .expect("the start boundary launches the session");
    assert_eq!(started, arms.len(), "one driver per hosted arm");
    for (name, source) in arms
        .into_iter()
        .zip([PendingTxSource::Mevblocker, PendingTxSource::Txpool])
    {
        let detail = await_detail(&session, &StrategyId::new(name));
        assert!(
            detail.contains("head_source_live=true"),
            "{name} starts only after the head edge registered its source: {detail}"
        );
        assert!(
            detail.contains(&format!("stream=Some({source:?})")),
            "{name} is handed its own arm's stream: {detail}"
        );
    }
}
