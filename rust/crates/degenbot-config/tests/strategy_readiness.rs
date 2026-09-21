//! The typed fail-close over strategy activation: an activated facet must
//! carry a non-empty, validated endpoint set, and settlement endpoints are
//! restricted to the pinned revert-protecting relay allowlist.

#![expect(
    clippy::expect_used,
    reason = "test fixtures fail loudly on an unconstructible prerequisite"
)]

use degenbot_config::readiness::{strategy_readiness, Arm};
use degenbot_config::{
    BotConfig, BotConfigLoader, MapEnv, DEFAULT_BACKRUN_STREAM_URL, DEFAULT_PEER_BACKRUN_RELAYS,
    SETTLEMENT_DEFAULT_ENDPOINTS,
};

/// A config with an activated facet and an optional endpoints choice.
fn activated(facet: &str, endpoints: Option<&str>) -> BotConfig {
    let mut config = BotConfig::default();
    let section = format!("strategy.{facet}");
    config
        .assign(&section, "active", "true")
        .expect("assign active");
    if let Some(urls) = endpoints {
        config
            .assign(&section, "endpoints", urls)
            .expect("assign endpoints");
    }
    config
}

#[test]
fn inactive_facets_have_no_endpoint_requirement() {
    let readiness = strategy_readiness(&BotConfig::default()).expect("inactive config is ready");
    assert_eq!(readiness.settlement, Arm::Inactive);
    assert_eq!(readiness.mevblocker_backrun, Arm::Inactive);
    assert_eq!(readiness.peer_backrun, Arm::Inactive);
}

#[test]
fn settlement_explicit_endpoints_resolve() {
    let cfg = activated(
        "settlement",
        Some("https://rpc.flashbots.net?hint=hash,https://rpc.mevblocker.io/fullprivacy"),
    );
    let readiness = strategy_readiness(&cfg).expect("ready");
    assert_eq!(
        readiness.settlement,
        Arm::Active(vec![
            "https://rpc.flashbots.net?hint=hash".to_string(),
            "https://rpc.mevblocker.io/fullprivacy".to_string(),
        ])
    );
}

#[test]
fn the_pinned_allowlist_resolves_when_stamped_as_endpoints() {
    let pinned = SETTLEMENT_DEFAULT_ENDPOINTS.to_vec().join(",");
    let cfg = activated("settlement", Some(&pinned));
    let readiness = strategy_readiness(&cfg).expect("ready");
    assert_eq!(
        readiness.settlement,
        Arm::Active(
            SETTLEMENT_DEFAULT_ENDPOINTS
                .iter()
                .map(|url| (*url).to_string())
                .collect()
        )
    );
}

#[test]
fn the_mevblocker_default_channel_resolves_when_stamped_as_endpoints() {
    let cfg = activated("mevblocker_backrun", Some(DEFAULT_BACKRUN_STREAM_URL));
    let readiness = strategy_readiness(&cfg).expect("ready");
    assert_eq!(
        readiness.mevblocker_backrun,
        Arm::Active(vec![DEFAULT_BACKRUN_STREAM_URL.to_string()])
    );
}

#[test]
fn the_peer_default_relays_resolve_when_stamped_as_endpoints() {
    let pinned = DEFAULT_PEER_BACKRUN_RELAYS.join(",");
    let cfg = activated("peer_backrun", Some(&pinned));
    let readiness = strategy_readiness(&cfg).expect("ready");
    assert_eq!(
        readiness.peer_backrun,
        Arm::Active(
            DEFAULT_PEER_BACKRUN_RELAYS
                .iter()
                .map(|url| (*url).to_string())
                .collect()
        )
    );
}

#[test]
fn mevblocker_explicit_endpoint_resolves() {
    let cfg = activated("mevblocker_backrun", Some("wss://searchers.example/x"));
    let readiness = strategy_readiness(&cfg).expect("ready");
    assert_eq!(
        readiness.mevblocker_backrun,
        Arm::Active(vec!["wss://searchers.example/x".to_string()])
    );
}

#[test]
fn peer_explicit_endpoints_resolve() {
    let cfg = activated("peer_backrun", Some("https://relay.one,https://relay.two"));
    let readiness = strategy_readiness(&cfg).expect("ready");
    assert_eq!(
        readiness.peer_backrun,
        Arm::Active(vec![
            "https://relay.one".to_string(),
            "https://relay.two".to_string(),
        ])
    );
}

#[test]
fn unset_or_blank_endpoints_on_an_active_facet_is_a_typed_refusal() {
    for facet in ["settlement", "mevblocker_backrun", "peer_backrun"] {
        for endpoints in [None, Some(""), Some("  ,  ")] {
            let cfg = activated(facet, endpoints);
            let error = strategy_readiness(&cfg).expect_err("blank endpoints must refuse");
            let message = error.to_string();
            assert!(
                message.contains(&format!("strategy \"{facet}\"")),
                "error names the facet: {message}"
            );
            assert!(
                message.contains("--endpoints-default"),
                "error names the default remediation: {message}"
            );
        }
    }
}

#[test]
fn off_allowlist_settlement_endpoints_refuse() {
    for url in [
        "https://rpc.mevblocker.io/fast",
        "https://rpc.beaverbuild.org",
        "https://rpc.myrelay.example",
    ] {
        let cfg = activated("settlement", Some(url));
        let error = strategy_readiness(&cfg).expect_err("off-allowlist endpoint must refuse");
        let message = error.to_string();
        assert!(message.contains(url), "names the refused URL: {message}");
        assert!(
            message.contains("RELAYS_AND_GUARDRAILS"),
            "names the audit doc: {message}"
        );
    }
}

#[test]
fn mevblocker_endpoint_shape_is_validated() {
    for url in ["http://searchers.example", "wss:", "wss://a,b wss://c"] {
        let cfg = activated("mevblocker_backrun", Some(url));
        assert!(
            strategy_readiness(&cfg).is_err(),
            "malformed mevblocker endpoint {url} must refuse"
        );
    }
}

#[test]
fn peer_endpoint_shape_is_validated() {
    for url in [
        "wss://relay.example",
        "ftp://relay.example",
        "relay.example",
    ] {
        let cfg = activated("peer_backrun", Some(url));
        assert!(
            strategy_readiness(&cfg).is_err(),
            "non-http peer relay {url} must refuse"
        );
    }
}

#[test]
fn mevblocker_bid_mode_requires_key_and_private_url() {
    let mut cfg = activated("mevblocker_backrun", Some(DEFAULT_BACKRUN_STREAM_URL));
    cfg.assign("strategy.mevblocker_backrun", "bid_mode", "true")
        .expect("bid mode");
    let error = strategy_readiness(&cfg).expect_err("bid mode without key/url must refuse");
    let message = error.to_string();
    assert!(message.contains("key_file"), "{message}");

    cfg.assign(
        "strategy.mevblocker_backrun",
        "key_file",
        "/tmp/operator.key",
    )
    .expect("key file");
    let error = strategy_readiness(&cfg).expect_err("bid mode without the private url must refuse");
    assert!(error.to_string().contains("mevblocker_url"), "{error}");

    cfg.assign(
        "strategy.mevblocker_backrun",
        "mevblocker_url",
        "http://private.local:8545",
    )
    .expect("private url");
    let readiness = strategy_readiness(&cfg).expect("bid mode with key and url is ready");
    assert!(matches!(readiness.mevblocker_backrun, Arm::Active(_)));
}

#[test]
fn observe_only_mevblocker_needs_no_key_or_private_url() {
    let cfg = activated("mevblocker_backrun", Some(DEFAULT_BACKRUN_STREAM_URL));
    let readiness = strategy_readiness(&cfg).expect("observe-only activation is ready");
    assert!(matches!(readiness.mevblocker_backrun, Arm::Active(_)));
}

#[test]
fn readiness_error_for_unset_survives_a_full_loader_round_trip() {
    let env = MapEnv::new(
        [(
            "DEGENBOT_".to_string() + "STRATEGY_SETTLEMENT_ACTIVE",
            "true".to_string(),
        )]
        .into_iter()
        .collect(),
    );
    let loaded = BotConfigLoader::new()
        .with_env(Box::new(env))
        .load()
        .expect("load");
    let error = strategy_readiness(&loaded.config).expect_err("unset endpoints must refuse");
    assert!(error.to_string().contains("--endpoints-default"));
}
