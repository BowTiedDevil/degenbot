//! Pins for the `strategy.settlement` facet activation keys: the declared
//! surface resolves from TOML, from env, and collapses to its declared
//! defaults. A retired marker spelling is refused at the env boundary with
//! its activation remediation.

#![expect(
    clippy::expect_used,
    reason = "test fixtures fail loudly on an unconstructible prerequisite"
)]

use std::collections::BTreeMap;

use degenbot_config::{BotConfigLoader, MapEnv};

#[test]
fn settlement_facet_collapses_to_declared_defaults() {
    let loaded = BotConfigLoader::new()
        .without_env()
        .load()
        .expect("defaults load");
    let s = &loaded.config.strategy.settlement;
    assert!(!s.active);
    assert_eq!(s.endpoints, None);
}

#[test]
fn settlement_facet_resolves_from_env() {
    let env = MapEnv::new(BTreeMap::from([
        (
            "DEGENBOT_STRATEGY_SETTLEMENT_ACTIVE".to_string(),
            "1".to_string(),
        ),
        (
            "DEGENBOT_STRATEGY_SETTLEMENT_ENDPOINTS".to_string(),
            "https://rpc.flashbots.net?hint=hash,https://rpc.mevblocker.io/noreverts".to_string(),
        ),
    ]));
    let loaded = BotConfigLoader::new()
        .with_env(Box::new(env))
        .load()
        .expect("env load");
    let s = &loaded.config.strategy.settlement;
    assert!(s.active);
    assert_eq!(
        s.endpoints.as_deref(),
        Some("https://rpc.flashbots.net?hint=hash,https://rpc.mevblocker.io/noreverts")
    );
}

#[test]
fn the_retired_default_endpoint_marker_is_refused_at_the_env_boundary() {
    let env = MapEnv::new(BTreeMap::from([(
        "DEGENBOT_STRATEGY_SETTLEMENT_DEFAULT_ENDPOINTS".to_string(),
        "true".to_string(),
    )]));
    let err = BotConfigLoader::new()
        .with_env(Box::new(env))
        .load()
        .expect_err("the retired marker fails the load");
    let message = format!("{err}");
    assert!(
        message.contains("--endpoints-default"),
        "the refusal names the activation remediation: {message}"
    );
    assert!(
        message.contains("docs/config-migration.md"),
        "the refusal points at the migration doc: {message}"
    );
}

#[test]
fn settlement_facet_resolves_from_toml() {
    let path = std::env::temp_dir().join(format!("settlement-facet-{}.toml", std::process::id()));
    std::fs::write(
        &path,
        "[strategy.settlement]\nactive = true\nendpoints = \"https://rpc.mevblocker.io/noreverts\"\n",
    )
    .expect("write toml");
    let loaded = BotConfigLoader::new()
        .without_env()
        .with_config_path(&path)
        .load()
        .expect("toml load");
    let _ = std::fs::remove_file(&path);
    let s = &loaded.config.strategy.settlement;
    assert!(s.active);
    assert_eq!(
        s.endpoints.as_deref(),
        Some("https://rpc.mevblocker.io/noreverts")
    );
}

#[test]
fn settlement_active_rejects_junk_at_load() {
    let env = MapEnv::new(BTreeMap::from([(
        "DEGENBOT_STRATEGY_SETTLEMENT_ACTIVE".to_string(),
        "sure".to_string(),
    )]));
    assert!(
        BotConfigLoader::new()
            .with_env(Box::new(env))
            .load()
            .is_err(),
        "a non-boolean activation must fail the load, not silently deactivate"
    );
}
