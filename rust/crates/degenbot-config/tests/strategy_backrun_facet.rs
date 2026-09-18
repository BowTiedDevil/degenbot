//! RED-first pins for the strategy.backrun facet keys (X6P5GN): every key
//! resolves from TOML, from env, and collapses to its declared default.

#![expect(
    clippy::expect_used,
    reason = "test fixtures fail loudly on an unconstructible prerequisite"
)]

use std::collections::BTreeMap;

use degenbot_config::{BotConfig, BotConfigLoader, MapEnv};

/// The complete backrun facet surface (env name -> (toml leaf, raw env value)).
fn facet_env() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("DEGENBOT_STRATEGY_BACKRUN_BID_MODE", "1"),
        (
            "DEGENBOT_STRATEGY_BACKRUN_BUDGET_WEI",
            "12345678901234567890",
        ),
        ("DEGENBOT_STRATEGY_BACKRUN_MAX_BUNDLE_WEI", "999"),
        ("DEGENBOT_STRATEGY_BACKRUN_BRIBE_BIPS", "9500"),
        ("DEGENBOT_STRATEGY_BACKRUN_PRIORITY_FEE_GWEI", "7"),
        ("DEGENBOT_STRATEGY_BACKRUN_BUNDLE_GAS_EST", "333000"),
        ("DEGENBOT_STRATEGY_BACKRUN_DRY_RUN", "1"),
        ("DEGENBOT_STRATEGY_BACKRUN_KEY_FILE", "/tmp/k.key"),
        (
            "DEGENBOT_STRATEGY_BACKRUN_EXECUTOR",
            "0x00000000000000000000000000000000000000aa",
        ),
        (
            "DEGENBOT_STRATEGY_BACKRUN_OPERATOR",
            "0x00000000000000000000000000000000000000bb",
        ),
        ("DEGENBOT_STRATEGY_BACKRUN_SIM_URL", "http://sim.local:8545"),
        ("DEGENBOT_STRATEGY_BACKRUN_STREAM_URL", "wss://stream.local"),
        ("DEGENBOT_STRATEGY_BACKRUN_RANK_EVIDENCE", "1"),
        ("DEGENBOT_STRATEGY_BACKRUN_CONNECTORS", "5"),
        ("DEGENBOT_STRATEGY_BACKRUN_FIXTURE_HEAD", "26001272"),
        ("DEGENBOT_STRATEGY_BACKRUN_STOP_FILE", "/tmp/stop"),
    ])
}

#[test]
fn backrun_facet_collapses_to_declared_defaults() {
    let loaded = BotConfigLoader::new()
        .without_env()
        .load()
        .expect("defaults load");
    let b = &loaded.config.strategy.backrun;
    assert!(!b.bid_mode);
    assert_eq!(b.budget_wei, 0);
    assert_eq!(b.max_bundle_wei, 1_000_000_000_000_000);
    assert_eq!(b.bribe_bips, 9_800);
    assert_eq!(b.priority_fee_gwei, 2);
    assert_eq!(b.bundle_gas_est, 300_000);
    assert!(!b.dry_run);
    assert_eq!(b.key_file, None);
    assert_eq!(b.connectors, 8);
    assert_eq!(b.fixture_head, None);
}

#[test]
fn backrun_facet_resolves_from_env() {
    let env = MapEnv::new(
        facet_env()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    );
    let loaded = BotConfigLoader::new()
        .with_env(Box::new(env))
        .load()
        .expect("env load");
    let b = &loaded.config.strategy.backrun;
    assert!(b.bid_mode);
    assert_eq!(b.budget_wei, 12_345_678_901_234_567_890u128);
    assert_eq!(b.bribe_bips, 9_500);
    assert_eq!(b.priority_fee_gwei, 7);
    assert_eq!(b.fixture_head, Some(26_001_272));
    assert_eq!(b.stop_file, std::path::PathBuf::from("/tmp/stop"));
}

#[test]
fn backrun_facet_resolves_from_toml() {
    let path = std::env::temp_dir().join(format!("x6p5gn-{}.toml", std::process::id()));
    std::fs::write(
        &path,
        "[strategy.backrun]\n         bid_mode = true\n         budget_wei = \"12345678901234567890\"\n         bribe_bips = 9500\n         connectors = 5\n         fixture_head = 26001272\n",
    )
    .expect("write toml");
    let loaded = BotConfigLoader::new()
        .without_env()
        .with_config_path(&path)
        .load()
        .expect("toml load");
    let _ = std::fs::remove_file(&path);
    let b = &loaded.config.strategy.backrun;
    assert!(b.bid_mode);
    assert_eq!(b.budget_wei, 12_345_678_901_234_567_890u128);
    assert_eq!(b.bribe_bips, 9_500);
    assert_eq!(b.connectors, 5);
    assert_eq!(b.fixture_head, Some(26_001_272));
    // Untouched keys keep defaults.
    assert_eq!(b.max_bundle_wei, 1_000_000_000_000_000);
}

#[test]
fn fixture_head_rejects_junk_at_load() {
    let env = MapEnv::new(BTreeMap::from([(
        "DEGENBOT_STRATEGY_BACKRUN_FIXTURE_HEAD".to_string(),
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
