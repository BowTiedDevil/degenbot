//! RED-first pins for the two per-ecosystem backrun facets: every key
//! resolves from TOML, from env, and collapses to its declared default.

#![expect(
    clippy::expect_used,
    reason = "test fixtures fail loudly on an unconstructible prerequisite"
)]

use std::collections::BTreeMap;

use degenbot_config::{BotConfig, BotConfigLoader, MapEnv};

const MEVBLOCKER_ENV: &[(&str, &str)] = &[
    ("DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_ACTIVE", "1"),
    ("DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_BID_MODE", "1"),
    (
        "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_BUDGET_WEI",
        "12345678901234567890",
    ),
    ("DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_MAX_BUNDLE_WEI", "999"),
    ("DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_BRIBE_BIPS", "9500"),
    (
        "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_PRIORITY_FEE_GWEI",
        "7",
    ),
    (
        "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_BUNDLE_GAS_EST",
        "333000",
    ),
    ("DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_DRY_RUN", "1"),
    (
        "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_KEY_FILE",
        "/tmp/k.key",
    ),
    (
        "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_EXECUTOR",
        "0x00000000000000000000000000000000000000aa",
    ),
    (
        "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_OPERATOR",
        "0x00000000000000000000000000000000000000bb",
    ),
    (
        "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_SIM_URL",
        "http://sim.local:8545",
    ),
    (
        "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_ENDPOINTS",
        "wss://stream.local",
    ),
    (
        "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_MEVBLOCKER_URL",
        "http://private.local:8545",
    ),
    ("DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_RANK_EVIDENCE", "1"),
    ("DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_CONNECTORS", "5"),
    ("DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_CYCLE_MAX_HOPS", "6"),
    (
        "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_FIXTURE_HEAD",
        "26001272",
    ),
    (
        "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_STOP_FILE",
        "/tmp/stop",
    ),
];

const PEER_ENV: &[(&str, &str)] = &[
    ("DEGENBOT_STRATEGY_TXPOOL_BACKRUN_ACTIVE", "1"),
    ("DEGENBOT_STRATEGY_TXPOOL_BACKRUN_BID_MODE", "1"),
    (
        "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_ENDPOINTS",
        "https://relay.one,https://relay.two",
    ),
    (
        "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_STOP_FILE",
        "/tmp/peer-stop",
    ),
];

fn env_map(pairs: &[(&str, &str)]) -> MapEnv {
    MapEnv::new(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect::<BTreeMap<String, String>>(),
    )
}

#[test]
fn backrun_facets_collapse_to_declared_defaults() {
    let loaded = BotConfigLoader::new()
        .without_env()
        .load()
        .expect("defaults load");
    let b = &loaded.config.strategy.mevblocker_backrun;
    assert!(!b.bid_mode);
    assert_eq!(b.budget_wei, 0);
    assert_eq!(b.max_bundle_wei, 1_000_000_000_000_000);
    assert_eq!(b.bribe_bips, 9_800);
    assert_eq!(b.priority_fee_gwei, 2);
    assert_eq!(b.bundle_gas_est, 300_000);
    assert!(!b.dry_run);
    assert_eq!(b.key_file, None);
    assert_eq!(b.connectors, 8);
    assert_eq!(b.cycle_max_hops, 4);
    assert_eq!(b.fixture_head, None);
    assert_eq!(b.mevblocker_url, None);

    let p = &loaded.config.strategy.txpool_backrun;
    assert!(!p.bid_mode);
    assert_eq!(p.budget_wei, 0);
    assert_eq!(p.max_bundle_wei, 1_000_000_000_000_000);
    assert_eq!(p.connectors, 8);
    assert_eq!(p.cycle_max_hops, 4);
    assert_eq!(p.endpoints, None);
    assert_eq!(
        p.stop_file,
        std::path::PathBuf::from("/tmp/degenbot-sidecar-STOP")
    );
    assert!(!BotConfig::default().strategy.txpool_backrun.active);
}

#[test]
fn mevblocker_facet_resolves_from_env() {
    let loaded = BotConfigLoader::new()
        .with_env(Box::new(env_map(MEVBLOCKER_ENV)))
        .load()
        .expect("env load");
    let b = &loaded.config.strategy.mevblocker_backrun;
    assert!(b.bid_mode);
    assert_eq!(b.budget_wei, 12_345_678_901_234_567_890u128);
    assert_eq!(b.bribe_bips, 9_500);
    assert_eq!(b.priority_fee_gwei, 7);
    assert_eq!(b.fixture_head, Some(26_001_272));
    assert_eq!(b.stop_file, std::path::PathBuf::from("/tmp/stop"));
    assert_eq!(b.cycle_max_hops, 6);
    assert_eq!(
        b.mevblocker_url.as_deref(),
        Some("http://private.local:8545")
    );
    assert!(b.active);
    assert_eq!(b.endpoints.as_deref(), Some("wss://stream.local"));
}

#[test]
fn peer_facet_resolves_from_env() {
    let loaded = BotConfigLoader::new()
        .with_env(Box::new(env_map(PEER_ENV)))
        .load()
        .expect("env load");
    let p = &loaded.config.strategy.txpool_backrun;
    assert!(p.active);
    assert!(p.bid_mode);
    assert_eq!(
        p.endpoints.as_deref(),
        Some("https://relay.one,https://relay.two")
    );
    assert_eq!(p.stop_file, std::path::PathBuf::from("/tmp/peer-stop"));
    // The MEVBlocker facet is untouched by peer env.
    assert!(!loaded.config.strategy.mevblocker_backrun.active);
}

#[test]
fn mevblocker_facet_resolves_from_toml() {
    let path = std::env::temp_dir().join(format!("backrun-facet-{}.toml", std::process::id()));
    std::fs::write(
        &path,
        "[strategy.mevblocker_backrun]\n         bid_mode = true\n         budget_wei = \"12345678901234567890\"\n         bribe_bips = 9500\n         connectors = 5\n         cycle_max_hops = 6\n         fixture_head = 26001272\n",
    )
    .expect("write toml");
    let loaded = BotConfigLoader::new()
        .without_env()
        .with_config_path(&path)
        .load()
        .expect("toml load");
    let _ = std::fs::remove_file(&path);
    let b = &loaded.config.strategy.mevblocker_backrun;
    assert!(b.bid_mode);
    assert_eq!(b.budget_wei, 12_345_678_901_234_567_890u128);
    assert_eq!(b.bribe_bips, 9_500);
    assert_eq!(b.connectors, 5);
    assert_eq!(b.cycle_max_hops, 6);
    assert_eq!(b.fixture_head, Some(26_001_272));
    assert_eq!(b.max_bundle_wei, 1_000_000_000_000_000);
}

#[test]
fn cycle_max_hops_below_two_refuses_at_load_with_the_remedy() {
    for (facet_env, toml_path) in [
        (
            "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_CYCLE_MAX_HOPS",
            "strategy.mevblocker_backrun.cycle_max_hops",
        ),
        (
            "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_CYCLE_MAX_HOPS",
            "strategy.txpool_backrun.cycle_max_hops",
        ),
    ] {
        for bad in ["0", "1"] {
            let error = BotConfigLoader::new()
                .with_env(Box::new(env_map(&[(facet_env, bad)])))
                .load()
                .expect_err("below the pin-plus-connector minimum must fail the load");
            let message = error.to_string();
            assert!(
                message.contains(toml_path),
                "the refusal names the key ({toml_path}): {message}"
            );
            assert!(
                message.contains("minimum") && message.contains("connector"),
                "the refusal names the remedy: {message}"
            );
        }
        assert!(
            BotConfigLoader::new()
                .with_env(Box::new(env_map(&[(facet_env, "2")])))
                .load()
                .is_ok(),
            "the minimum 2 (pin + one connector) loads"
        );
    }
}

#[test]
fn fixture_head_rejects_junk_at_load() {
    let env = MapEnv::new(BTreeMap::from([(
        "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_FIXTURE_HEAD".to_string(),
        "latest".to_string(),
    )]));
    assert!(
        BotConfigLoader::new()
            .with_env(Box::new(env))
            .load()
            .is_err(),
        "a non-numeric pinned head must fail the load, not silently fall back"
    );
}

#[test]
fn logging_artifact_keys_resolve() {
    let mut raw = BTreeMap::new();
    raw.insert("DEGENBOT_LOG_STDERR".to_string(), "1".to_string());
    raw.insert(
        "DEGENBOT_TRACE_JSONL".to_string(),
        "/tmp/t.jsonl".to_string(),
    );
    raw.insert(
        "DEGENBOT_DRY_RUN_JSONL".to_string(),
        "/tmp/f.jsonl".to_string(),
    );
    let loaded = BotConfigLoader::new()
        .with_env(Box::new(MapEnv::new(raw)))
        .load()
        .expect("env load");
    let l = &loaded.config.logging;
    assert!(l.log_stderr);
    assert_eq!(
        l.trace_jsonl,
        Some(std::path::PathBuf::from("/tmp/t.jsonl"))
    );
    assert_eq!(
        l.dry_run_jsonl,
        Some(std::path::PathBuf::from("/tmp/f.jsonl"))
    );
    let defaults = BotConfig::default().logging;
    assert!(!defaults.log_stderr);
    assert_eq!(defaults.trace_jsonl, None);
    assert_eq!(defaults.dry_run_jsonl, None);
}
