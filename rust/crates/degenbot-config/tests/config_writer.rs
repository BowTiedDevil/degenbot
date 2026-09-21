#![expect(
    clippy::expect_used,
    reason = "test fixtures fail loudly on an unconstructible prerequisite"
)]

use std::collections::BTreeMap;
use std::path::Path;

use degenbot_config::writer::{remove_key, write_key_with_env, WriteOutcome};
use degenbot_config::{BotConfigLoader, KeyDecl, MapEnv, SCHEMA};

fn key(path: &str) -> &'static KeyDecl {
    let decl = SCHEMA.iter().find(|k| k.toml_path == path);
    assert!(decl.is_some(), "key {path} must be declared");
    decl.expect("declared in SCHEMA")
}

fn load(file: &Path, env: Option<MapEnv>) -> degenbot_config::LoadedConfig {
    let mut loader = BotConfigLoader::new().without_env().with_config_path(file);
    if let Some(env) = env {
        loader = loader.with_env(Box::new(env));
    }
    loader.load().expect("written file loads")
}

#[test]
fn write_key_persists_and_loads_with_file_provenance() {
    let dir = std::env::temp_dir().join(format!("writer-{}", std::process::id()));
    let file = dir.join("nested/config.toml");
    let outcome = write_key_with_env(&file, key("strategy.backrun.active"), "true", &empty_env())
        .expect("write");
    assert_eq!(outcome, WriteOutcome::Written);
    let loaded = load(&file, None);
    assert_eq!(
        loaded.source_of("DEGENBOT_STRATEGY_BACKRUN_ACTIVE"),
        Some(degenbot_config::loader::Source::File)
    );
    assert!(loaded.config.strategy.backrun.active);
}

#[test]
fn write_key_refuses_an_invalid_value_and_leaves_the_file_untouched() {
    let dir = std::env::temp_dir().join(format!("writer-refuse-{}", std::process::id()));
    let file = dir.join("config.toml");
    write_key_with_env(
        &file,
        key("strategy.backrun.bribe_bips"),
        "2000",
        &empty_env(),
    )
    .expect("seed write");
    let before = std::fs::read(&file).expect("read before");
    let err = write_key_with_env(
        &file,
        key("strategy.backrun.bribe_bips"),
        "not-a-number",
        &empty_env(),
    )
    .expect_err("junk must refuse");
    assert!(
        err.to_string().contains("bribe_bips"),
        "names the key: {err}"
    );
    let after = std::fs::read(&file).expect("read after");
    assert_eq!(before, after, "a refused value must not touch the file");
}

#[test]
fn write_key_preserves_neighboring_content() {
    let dir = std::env::temp_dir().join(format!("writer-neighbor-{}", std::process::id()));
    let file = dir.join("config.toml");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(
        &file,
        "# operator note\n[logging]\nlog_stderr = true\n[telemetry]\notel = false\n",
    )
    .expect("seed");
    write_key_with_env(
        &file,
        key("strategy.settlement.active"),
        "true",
        &empty_env(),
    )
    .expect("write");
    let text = std::fs::read_to_string(&file).expect("read back");
    assert!(text.contains("# operator note"), "comment survives: {text}");
    assert!(
        text.contains("log_stderr = true"),
        "neighbor key survives: {text}"
    );
    let loaded = load(&file, None);
    assert!(loaded.config.logging.log_stderr);
    assert!(loaded.config.strategy.settlement.active);
}

#[test]
fn remove_key_restores_the_schema_default() {
    let dir = std::env::temp_dir().join(format!("writer-remove-{}", std::process::id()));
    let file = dir.join("config.toml");
    write_key_with_env(
        &file,
        key("strategy.backrun.priority_fee_gwei"),
        "7",
        &empty_env(),
    )
    .expect("write");
    let seeded = load(&file, None);
    assert_eq!(seeded.config.strategy.backrun.priority_fee_gwei, 7);
    remove_key(&file, key("strategy.backrun.priority_fee_gwei")).expect("remove");
    let removed = load(&file, None);
    assert_eq!(removed.config.strategy.backrun.priority_fee_gwei, 2);
    assert_eq!(
        removed.source_of("DEGENBOT_STRATEGY_BACKRUN_PRIORITY_FEE_GWEI"),
        Some(degenbot_config::loader::Source::Default)
    );
}

#[test]
fn an_env_shadowing_the_key_is_reported() {
    let dir = std::env::temp_dir().join(format!("writer-shadow-{}", std::process::id()));
    let file = dir.join("config.toml");
    let env = MapEnv::new(BTreeMap::from([(
        "DEGENBOT_STRATEGY_BACKRUN_ACTIVE".to_string(),
        "false".to_string(),
    )]));
    let outcome =
        write_key_with_env(&file, key("strategy.backrun.active"), "true", &env).expect("write");
    assert_eq!(
        outcome,
        WriteOutcome::Shadowed {
            env: "DEGENBOT_STRATEGY_BACKRUN_ACTIVE"
        }
    );
    // The file still carries the write; the env layer wins at load time.
    let loaded = load(&file, Some(env));
    assert!(!loaded.config.strategy.backrun.active);
}

#[test]
fn a_comma_separated_endpoint_list_round_trips() {
    let dir = std::env::temp_dir().join(format!("writer-endpoints-{}", std::process::id()));
    let file = dir.join("config.toml");
    let urls = "https://rpc.flashbots.net?hint=hash,https://rpc.mevblocker.io/noreverts";
    write_key_with_env(
        &file,
        key("strategy.settlement.endpoints"),
        urls,
        &empty_env(),
    )
    .expect("write");
    let loaded = load(&file, None);
    assert_eq!(
        loaded.config.strategy.settlement.endpoints.as_deref(),
        Some(urls)
    );
}

fn empty_env() -> MapEnv {
    MapEnv::new(BTreeMap::new())
}
