//! Cross-crate hosting integration: the strategy plane's compositions admit
//! through the one host verb surface. These tests span `degenbot-bot`'s host
//! FSM and `degenbot-strategy`'s compositions, so they live behind the
//! umbrella crate where both are production dependencies.

#![expect(clippy::expect_used, reason = "test assertions fail loudly")]

use degenbot::bot::strategy_host::{DriverPose, FacetStatus, StrategyHost};
use degenbot::config::BotConfig;
use degenbot::eventhub::Hub;
use degenbot::strategy::{MevblockerBackrun, StrategyName, TxpoolBackrun};
use degenbot::substrate::connector_index::V2ConnectorIndex;
use degenbot::substrate::nonce::{NonceAuthority, StrategyId};
use degenbot::substrate::RouteRegistry;
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
