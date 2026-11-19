//! Acceptance criterion: the pump settle-window ranges are enforced where the
//! configuration is loaded. A value that parses into its declared kind but
//! cannot serve the estimator (a zero window, a crossed window, a non-positive
//! or non-finite multiplier/smoothing constant) fails the load with an error
//! naming the key and its env name; a valid value reaches the typed tree
//! exactly as written, with no re-clamping downstream.

use std::path::PathBuf;

use degenbot_config::{BotConfigLoader, ConfigError, LoadedConfig, MapEnv, Source};

fn map_env(pairs: &[(&str, &str)]) -> Box<dyn degenbot_config::EnvVars> {
    Box::new(MapEnv::new(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
    ))
}

fn temp_toml(name: &str, body: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "degenbot-config-pump-validation-{}-{name}.toml",
        std::process::id()
    ));
    if let Err(e) = std::fs::write(&path, body) {
        unreachable!("temp toml write failed: {e}");
    }
    path
}

fn cleanup(path: &PathBuf) {
    // Temp-file cleanup is best-effort in a sandboxed test tree.
    if std::fs::remove_file(path).is_err() {}
}

fn must_ok(loader: &BotConfigLoader) -> LoadedConfig {
    match loader.load() {
        Ok(catalog) => catalog,
        Err(e) => unreachable!("load constructed to succeed: {e}"),
    }
}

fn must_err(loader: &BotConfigLoader) -> ConfigError {
    let Err(err) = loader.load() else {
        unreachable!("load constructed to fail");
    };
    err
}

/// Absent pump keys keep the declared defaults — the operator saying nothing is
/// not an invalid setting.
#[test]
fn absent_pump_keys_keep_the_declared_defaults() {
    let cfg = must_ok(&BotConfigLoader::new().without_env());
    assert_eq!(cfg.config.pump.pump_debounce_ms, 50);
    assert_eq!(cfg.config.pump.early_slice_ms, 25);
    assert_eq!(cfg.config.pump.quiesce_floor_ms, 2);
    assert_eq!(cfg.config.pump.quiesce_ceil_ms, 20);
    assert!((cfg.config.pump.quiesce_margin_ms - 3.0).abs() < f64::EPSILON);
    assert!((cfg.config.pump.quiesce_ewma_alpha - 0.1).abs() < f64::EPSILON);
    for env in [
        "DEGENBOT_PUMP_DEBOUNCE_MS",
        "DEGENBOT_PUMP_QUIESCE_FLOOR_MS",
        "DEGENBOT_PUMP_QUIESCE_CEIL_MS",
        "DEGENBOT_PUMP_QUIESCE_MARGIN_MS",
        "DEGENBOT_PUMP_QUIESCE_EWMA_ALPHA",
    ] {
        assert_eq!(cfg.source_of(env), Some(Source::Default), "{env}");
    }
}

/// A valid file layer reaches the typed tree byte-for-byte: the boundary
/// validates, it does not rewrite. `early_slice_ms = 0` is a real setting.
#[test]
fn valid_file_values_reach_the_typed_tree_exactly() {
    let path = temp_toml(
        "valid-file-values",
        "[pump]\npump_debounce_ms = 15\nearly_slice_ms = 0\n\
         quiesce_floor_ms = 3\nquiesce_ceil_ms = 40\n\
         quiesce_margin_ms = 1.5\nquiesce_ewma_alpha = 1.0\n",
    );
    let cfg = must_ok(&BotConfigLoader::new().without_env().with_config_path(&path));
    cleanup(&path);

    assert_eq!(cfg.config.pump.pump_debounce_ms, 15);
    assert_eq!(cfg.config.pump.early_slice_ms, 0);
    assert_eq!(cfg.config.pump.quiesce_floor_ms, 3);
    assert_eq!(cfg.config.pump.quiesce_ceil_ms, 40);
    assert!((cfg.config.pump.quiesce_margin_ms - 1.5).abs() < f64::EPSILON);
    assert!((cfg.config.pump.quiesce_ewma_alpha - 1.0).abs() < f64::EPSILON);
}

/// A zero debounce in the file layer is refused by key and env name — it is no
/// longer silently replaced with the default 50.
#[test]
fn zero_debounce_in_the_file_names_the_key() {
    let path = temp_toml("zero-debounce", "[pump]\npump_debounce_ms = 0\n");
    let err = must_err(&BotConfigLoader::new().without_env().with_config_path(&path));
    cleanup(&path);

    let message = err.to_string();
    assert!(
        message.contains("pump.pump_debounce_ms") && message.contains("DEGENBOT_PUMP_DEBOUNCE_MS"),
        "the refusal names the key and its env name: {message}"
    );
}

/// A zero floor arriving through the env layer is named there too.
#[test]
fn zero_floor_from_the_env_names_the_key() {
    let err = must_err(
        &BotConfigLoader::new().with_env(map_env(&[("DEGENBOT_PUMP_QUIESCE_FLOOR_MS", "0")])),
    );
    let message = err.to_string();
    assert!(
        message.contains("pump.quiesce_floor_ms")
            && message.contains("DEGENBOT_PUMP_QUIESCE_FLOOR_MS"),
        "the refusal names the key and its env name: {message}"
    );
}

/// A ceiling below the floor crosses the window rule, so the load refuses it
/// naming both bounds.
#[test]
fn crossed_window_from_the_env_names_both_bounds() {
    let err = must_err(&BotConfigLoader::new().with_env(map_env(&[
        ("DEGENBOT_PUMP_QUIESCE_FLOOR_MS", "20"),
        ("DEGENBOT_PUMP_QUIESCE_CEIL_MS", "10"),
    ])));
    let message = err.to_string();
    assert!(
        message.contains("pump.quiesce_ceil_ms")
            && message.contains("DEGENBOT_PUMP_QUIESCE_CEIL_MS")
            && message.contains("pump.quiesce_floor_ms")
            && message.contains("DEGENBOT_PUMP_QUIESCE_FLOOR_MS"),
        "the refusal names both bounds: {message}"
    );
}

/// The float spellings the parser accepts (`nan`, `inf`, a negative) are
/// refused at the boundary instead of reaching the estimator.
#[test]
fn non_finite_float_env_values_are_refused() {
    for (env, key) in [
        ("DEGENBOT_PUMP_QUIESCE_MARGIN_MS", "pump.quiesce_margin_ms"),
        (
            "DEGENBOT_PUMP_QUIESCE_EWMA_ALPHA",
            "pump.quiesce_ewma_alpha",
        ),
    ] {
        for raw in ["nan", "inf", "-1"] {
            let err = must_err(&BotConfigLoader::new().with_env(map_env(&[(env, raw)])));
            let message = err.to_string();
            assert!(
                message.contains(key) && message.contains(env),
                "{env}={raw} must be refused by name: {message}"
            );
        }
    }
}

/// File and env problems aggregate into ONE error, matching the loader's
/// fail-closed contract for every other semantic refusal.
#[test]
fn file_and_env_problems_aggregate_into_one_error() {
    let path = temp_toml("aggregate", "[pump]\npump_debounce_ms = 0\n");
    let err = must_err(
        &BotConfigLoader::new()
            .with_env(map_env(&[("DEGENBOT_PUMP_QUIESCE_EWMA_ALPHA", "2")]))
            .with_config_path(&path),
    );
    cleanup(&path);

    assert_eq!(err.problems.len(), 2, "{err}");
    let message = err.to_string();
    assert!(message.contains("pump.pump_debounce_ms"), "{message}");
    assert!(message.contains("pump.quiesce_ewma_alpha"), "{message}");
}
