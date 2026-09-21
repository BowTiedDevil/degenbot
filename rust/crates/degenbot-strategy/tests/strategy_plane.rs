//! The strategy plane's shared selection surface.
//!
//! With three concrete strategies in hand, the plane contract is extracted by
//! subtraction: every strategy has a [`StrategyName`] (its facet name and
//! registration id) and is built through one [`StrategyName::select`] path.
//! The trait carries no speculative slot — this test pins that.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions fail loudly"
)]

use degenbot_config::BotConfig;
use degenbot_strategy::{
    MevblockerBackrun, PeerBackrun, SelectedStrategy, Settlement, Strategy, StrategyName,
};

#[test]
fn strategy_name_registration_order_is_settlement_then_backruns() {
    assert_eq!(
        StrategyName::ALL.map(StrategyName::as_str),
        ["settlement", "mevblocker_backrun", "peer_backrun"]
    );
    for name in StrategyName::ALL {
        assert_eq!(StrategyName::parse(name.as_str()), Some(name));
        assert_eq!(name.config_section(), format!("strategy.{}", name.as_str()));
    }
}

#[test]
fn each_concrete_composition_declares_its_plane_name() {
    assert_eq!(Settlement::NAME, StrategyName::Settlement);
    assert_eq!(MevblockerBackrun::NAME, StrategyName::MevblockerBackrun);
    assert_eq!(PeerBackrun::NAME, StrategyName::PeerBackrun);
}

#[test]
fn strategy_name_reads_the_facet_activation_flag() {
    let mut cfg = BotConfig::default();
    assert!(!StrategyName::Settlement.is_active(&cfg));
    assert!(!StrategyName::MevblockerBackrun.is_active(&cfg));
    cfg.strategy.settlement.active = true;
    cfg.strategy.peer_backrun.active = true;
    assert!(StrategyName::Settlement.is_active(&cfg));
    assert!(!StrategyName::MevblockerBackrun.is_active(&cfg));
    assert!(StrategyName::PeerBackrun.is_active(&cfg));
}

#[test]
fn every_strategy_selects_through_one_code_path() {
    // Settlement alone.
    let mut cfg = BotConfig::default();
    cfg.strategy.settlement.active = true;
    cfg.strategy.settlement.endpoints = Some(String::from("https://rpc.flashbots.net?hint=hash"));
    let selected = StrategyName::Settlement.select(&cfg, String::from("http://node.local"));
    assert_eq!(selected.name(), StrategyName::Settlement);
    let SelectedStrategy::Settlement(settlement) = selected else {
        panic!("settlement must select the settlement composition");
    };
    assert_eq!(
        settlement.config().endpoints,
        vec![String::from("https://rpc.flashbots.net?hint=hash")]
    );

    // Peer alone.
    let mut cfg = BotConfig::default();
    cfg.strategy.peer_backrun.active = true;
    cfg.strategy.peer_backrun.endpoints = Some(String::from("http://relay.one.local"));
    let selected = StrategyName::PeerBackrun.select(&cfg, String::from("http://node.local"));
    assert!(matches!(selected, SelectedStrategy::PeerBackrun(_)));
    assert_eq!(selected.name(), StrategyName::PeerBackrun);

    // Mevblocker alone.
    let mut cfg = BotConfig::default();
    cfg.strategy.mevblocker_backrun.active = true;
    cfg.strategy.mevblocker_backrun.endpoints = Some(String::from("wss://searchers.local"));
    cfg.strategy.mevblocker_backrun.mevblocker_url = Some(String::from("http://private.local"));
    let selected = StrategyName::MevblockerBackrun.select(&cfg, String::from("http://node.local"));
    assert!(matches!(selected, SelectedStrategy::MevblockerBackrun(_)));
    assert_eq!(selected.name(), StrategyName::MevblockerBackrun);

    // Settlement plus a backrun in the same config: each selects its own.
    cfg.strategy.settlement.active = true;
    cfg.strategy.settlement.endpoints = Some(String::from("https://rpc.flashbots.net?hint=hash"));
    let settlement = StrategyName::Settlement.select(&cfg, String::from("http://node.local"));
    let peer = StrategyName::PeerBackrun.select(&cfg, String::from("http://node.local"));
    assert_eq!(settlement.name(), StrategyName::Settlement);
    assert_eq!(peer.name(), StrategyName::PeerBackrun);
}

#[test]
fn plane_trait_carries_no_speculative_slots() {
    let source = include_str!("../src/strategy_plane.rs");
    let block = source
        .split("pub trait Strategy {")
        .nth(1)
        .expect("the plane declares a Strategy trait")
        .split('}')
        .next()
        .expect("the trait body is source-readable");
    let slots = block.matches("const ").count() + block.matches(" fn ").count();
    assert_eq!(
        slots, 1,
        "the plane contract names only what all three strategies share"
    );
}

/// The three strategies admit through the one registration surface: each
/// `StrategyName` registers under its own id, settlement and a backrun enable
/// independently in one process.
#[test]
fn all_three_strategies_register_and_enable_through_one_verb_surface() {
    use degenbot_bot::bot_core::route_registry::RouteRegistry;
    use degenbot_bot::connector_index::V2ConnectorIndex;
    use degenbot_bot::nonce_authority::{NonceAuthority, StrategyId};
    use degenbot_bot::strategy_host::{DriverPose, FacetStatus, StrategyHost};
    use degenbot_eventhub::Hub;
    use std::sync::Arc;

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
    let peer = StrategyId::new(StrategyName::PeerBackrun.as_str());
    assert_eq!(host.enable(&settlement), Ok(DriverPose::Enabled));
    assert_eq!(
        host.state_of(&peer),
        Some(DriverPose::Registered),
        "enabling settlement leaves a backrun dormant"
    );
    assert_eq!(host.enable(&peer), Ok(DriverPose::Enabled));
    assert_eq!(host.state_of(&settlement), Some(DriverPose::Enabled));
}
