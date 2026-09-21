//! The typed fail-close over strategy activation: an activated facet must
//! carry a non-empty, validated endpoint set, and settlement endpoints are
//! restricted to the pinned revert-protecting relay allowlist.

#![expect(
    clippy::expect_used,
    reason = "test fixtures fail loudly on an unconstructible prerequisite"
)]

use degenbot_config::{
    BotConfig, BotConfigLoader, MapEnv, DEFAULT_BACKRUN_STREAM_URL, SETTLEMENT_DEFAULT_ENDPOINTS,
};

use degenbot_config::readiness::{strategy_readiness, Arm};

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
    assert_eq!(readiness.backrun, Arm::Inactive);
}

#[test]
fn settlement_explicit_endpoints_resolve() {
    let cfg = activated(
        "settlement",
        Some("https://rpc.flashbots.net?hint=hash,https://rpc.mevblocker.io/fullprivacy"),
    );
    let readiness = strategy_readiness(&cfg).expect("ready");
    let Arm::Active(urls) = readiness.settlement else {
        panic!("settlement must resolve its endpoints");
    };
    assert_eq!(
        urls,
        vec![
            "https://rpc.flashbots.net?hint=hash",
            "https://rpc.mevblocker.io/fullprivacy",
        ]
    );
}

#[test]
fn the_pinned_allowlist_resolves_when_stamped_as_endpoints() {
    // `--endpoints-default` stamps the pinned allowlist into the persisted
    // `endpoints` key; readiness must accept exactly that state.
    let pinned = SETTLEMENT_DEFAULT_ENDPOINTS
        .iter()
        .copied()
        .collect::<Vec<_>>()
        .join(",");
    let cfg = activated("settlement", Some(&pinned));
    let readiness = strategy_readiness(&cfg).expect("ready");
    let Arm::Active(urls) = readiness.settlement else {
        panic!("the pinned allowlist must resolve");
    };
    assert_eq!(urls, SETTLEMENT_DEFAULT_ENDPOINTS.to_vec());
    assert_eq!(
        urls.first().map(String::as_str),
        Some("https://rpc.flashbots.net?hint=hash")
    );
}

#[test]
fn the_backrun_default_channel_resolves_when_stamped_as_endpoints() {
    // The reference default posture the CLI stamps for the backrun arm.
    let cfg = activated("backrun", Some(DEFAULT_BACKRUN_STREAM_URL));
    let readiness = strategy_readiness(&cfg).expect("ready");
    let Arm::Active(urls) = readiness.backrun else {
        panic!("the default backrun channel must resolve");
    };
    assert_eq!(urls, vec![DEFAULT_BACKRUN_STREAM_URL.to_string()]);
}

#[test]
fn backrun_explicit_endpoint_resolves() {
    let cfg = activated("backrun", Some("wss://searchers.example/x"));
    let readiness = strategy_readiness(&cfg).expect("ready");
    let Arm::Active(urls) = readiness.backrun else {
        panic!("backrun must resolve its endpoints");
    };
    assert_eq!(urls, vec!["wss://searchers.example/x"]);
}

#[test]
fn unset_or_blank_endpoints_on_an_active_facet_is_a_typed_refusal() {
    for facet in ["settlement", "backrun"] {
        for endpoints in [None, Some(""), Some("  ,  ")] {
            let cfg = activated(facet, endpoints);
            let error = strategy_readiness(&cfg).expect_err("blank endpoints must refuse");
            let message = error.to_string();
            assert!(
                message.contains(&format!("strategy \"{facet}\"")),
                "error names the facet: {message}"
            );
            assert!(
                message.contains("degenbot strategy activate settlement --endpoints")
                    || message.contains("degenbot strategy activate backrun --endpoints"),
                "error names the explicit remediation: {message}"
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
    // The MEV-Blocker trap paths: /fast and /nochecks hold no revert
    // protection, and an unknown host is not audited at all.
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
fn backrun_endpoint_shape_is_validated() {
    for url in ["http://searchers.example", "wss:", "wss://a,b wss://c"] {
        let cfg = activated("backrun", Some(url));
        assert!(
            strategy_readiness(&cfg).is_err(),
            "malformed backrun endpoint {url} must refuse"
        );
    }
}

#[test]
fn readiness_error_for_unset_survives_a_full_loader_round_trip() {
    // The remediation path an operator actually hits: env-driven activation
    // with no endpoint choice, loaded through the standard loader.
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
