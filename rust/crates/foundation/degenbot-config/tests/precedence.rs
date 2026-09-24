//! Acceptance criterion: precedence is unit-tested per layer on
//! representative keys of each type (bool, duration-ms, usize).

use std::path::PathBuf;

use degenbot_config::{BotConfigLoader, MapEnv, Source};

// Representative keys, one per type:
// - bool   : `allocator.mimalloc_auto_purge` / `DEGENBOT_MIMALLOC_AUTO_PURGE`
// - ms     : `state_lock.warn_ms`            / `DEGENBOT_LOCK_WARN_MS`
// - usize  : `solve.envelope_max_tangent_lines` / `DEGENBOT_ENVELOPE_MAX_TANGENT_LINES`
// - enum   : (no standing representative — the enum-key slots retired with
//          `solve.executor` / `DEGENBOT_SOLVE_EXECUTOR` at the P6YXA6 hard
//          cutover and with `fleet.stance` / `DEGENBOT_FLEET` at the LW-T9
//          hard cutover; the enum parse law is covered by the quiesce-mode
//          chain below, BM35LK)

// Test env provider.
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
        "degenbot-config-precedence-{}-{name}.toml",
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

/// The injection knob participates in the standard env parity: the env
/// name is the only env-side surface (there is no alternate env spelling).
#[test]
fn inject_executor_code_env_parity() {
    let env_loader =
        BotConfigLoader::new().with_env(map_env(&[("DEGENBOT_INJECT_EXECUTOR_CODE", "1")]));
    let configured = must_ok(&env_loader);
    assert!(
        configured.config.simulation.inject_executor_code,
        "env DEGENBOT_INJECT_EXECUTOR_CODE=1"
    );
    assert_eq!(
        configured.source_of("DEGENBOT_INJECT_EXECUTOR_CODE"),
        Some(Source::Env)
    );

    let default_loader = BotConfigLoader::new().with_env(map_env(&[]));
    let fallback = must_ok(&default_loader);
    assert!(
        !fallback.config.simulation.inject_executor_code,
        "unset env -> default false"
    );
}

/// The per-session run-artifact root: typed schema default under HOME,
/// overridable through the standard `DEGENBOT_RUNS_DIR` env layer.
#[test]
fn logging_runs_dir_default_and_env_override() {
    let dflt = must_ok(&BotConfigLoader::new().without_env());
    assert_eq!(
        dflt.config.logging.runs_dir,
        PathBuf::from("~/.local/state/degenbot/logs"),
        "default is the state-home-relative run-artifacts root"
    );
    assert_eq!(dflt.source_of("DEGENBOT_RUNS_DIR"), Some(Source::Default));

    let env = must_ok(
        &BotConfigLoader::new().with_env(map_env(&[("DEGENBOT_RUNS_DIR", "/srv/degenbot/runs")])),
    );
    assert_eq!(
        env.config.logging.runs_dir,
        PathBuf::from("/srv/degenbot/runs")
    );
    assert_eq!(env.source_of("DEGENBOT_RUNS_DIR"), Some(Source::Env));
}

/// Load that MUST succeed; panics with the config error otherwise.
fn must_ok(loader: &BotConfigLoader) -> degenbot_config::LoadedConfig {
    match loader.load() {
        Ok(catalog) => catalog,
        Err(e) => unreachable!("load constructed to succeed: {e}"),
    }
}

/// Load that MUST fail; panics when it succeeds.
fn must_err(loader: &BotConfigLoader) -> degenbot_config::ConfigError {
    let Err(err) = loader.load() else {
        unreachable!("load constructed to fail");
    };
    err
}

#[test]
fn defaults_feed_every_representative_type() {
    let cfg = must_ok(&BotConfigLoader::new().without_env());
    assert!(cfg.config.allocator.mimalloc_auto_purge, "bool default");
    assert_eq!(cfg.config.state_lock.warn_ms, 500, "duration-ms default");
    assert_eq!(
        cfg.config.solve.envelope_max_tangent_lines, 32,
        "usize default"
    );
    // BM35LK quiesce-estimator keys: f64 + enum + ms defaults.
    assert_eq!(
        cfg.config.pump.quiesce_mode,
        degenbot_config::QuiesceMode::Adaptive,
        "quiesce mode default (adaptive = post-2026-09-09 A/B cutover)"
    );
    assert_eq!(
        (
            cfg.config.pump.quiesce_floor_ms,
            cfg.config.pump.quiesce_ceil_ms
        ),
        (2, 20),
        "quiesce floor/ceil defaults"
    );
    assert!(
        (cfg.config.pump.quiesce_margin_ms - 3.0).abs() < f64::EPSILON,
        "quiesce margin default"
    );
    assert!(
        (cfg.config.pump.quiesce_ewma_alpha - 0.1).abs() < f64::EPSILON,
        "quiesce alpha default"
    );
    assert_eq!(
        cfg.config.pump.quiesce_late_budget, 120,
        "quiesce late budget default"
    );
    assert_eq!(
        cfg.source_of("DEGENBOT_MIMALLOC_AUTO_PURGE"),
        Some(Source::Default)
    );
}

#[test]
fn file_layer_overrides_defaults() {
    let path = temp_toml(
        "file",
        "[state_lock]\nwarn_ms = 1500\n\n[solve]\nenvelope_max_tangent_lines = 64\n\n[allocator]\nmimalloc_auto_purge = false\n",
    );
    let loaded = must_ok(&BotConfigLoader::new().without_env().with_config_path(&path));
    assert_eq!(loaded.config.state_lock.warn_ms, 1500);
    assert_eq!(loaded.config.solve.envelope_max_tangent_lines, 64);
    assert!(
        !loaded.config.allocator.mimalloc_auto_purge,
        "bool from file"
    );
    assert_eq!(
        loaded.source_of("DEGENBOT_LOCK_WARN_MS"),
        Some(Source::File)
    );
    cleanup(&path);
}

#[test]
fn env_layer_overrides_file_layer() {
    let path = temp_toml("env", "[state_lock]\nwarn_ms = 1500\n");
    let loaded = must_ok(
        &BotConfigLoader::new()
            .with_env(map_env(&[("DEGENBOT_LOCK_WARN_MS", "2500")]))
            .with_config_path(&path),
    );
    assert_eq!(loaded.config.state_lock.warn_ms, 2500, "env beats file");
    assert_eq!(loaded.source_of("DEGENBOT_LOCK_WARN_MS"), Some(Source::Env));
    cleanup(&path);
}

#[test]
fn cli_layer_overrides_env_and_file() {
    let path = temp_toml(
        "cli",
        "[state_lock]\nwarn_ms = 1500\n\n[solve]\nenvelope_max_tangent_lines = 64\n\n[allocator]\nmimalloc_auto_purge = false\n",
    );
    let loaded = must_ok(
        &BotConfigLoader::new()
            .with_env(map_env(&[
                ("DEGENBOT_LOCK_WARN_MS", "2500"),
                ("DEGENBOT_ENVELOPE_MAX_TANGENT_LINES", "96"),
                ("DEGENBOT_MIMALLOC_AUTO_PURGE", "0"),
            ]))
            .with_config_path(&path)
            // A CLI override key accepts the env name OR the TOML dotted path.
            .with_cli("DEGENBOT_LOCK_WARN_MS", "3500")
            .with_cli("solve.envelope_max_tangent_lines", "128")
            .with_cli("allocator.mimalloc_auto_purge", "true"),
    );
    assert_eq!(
        loaded.config.state_lock.warn_ms, 3500,
        "cli beats env (duration-ms)"
    );
    assert_eq!(
        loaded.config.solve.envelope_max_tangent_lines, 128,
        "cli (by TOML path) beats env (usize)"
    );
    assert!(
        loaded.config.allocator.mimalloc_auto_purge,
        "cli beats env (bool)"
    );
    assert_eq!(loaded.source_of("DEGENBOT_LOCK_WARN_MS"), Some(Source::Cli));
    cleanup(&path);
}

#[test]
fn quiesce_keys_precedence_chain_env_beats_file_beats_default() {
    // the pump.quiesce_* keys follow the same precedence law.
    let path = temp_toml(
        "quiesce",
        "[pump]\nquiesce_mode = \"adaptive\"\nquiesce_floor_ms = 3\nquiesce_margin_ms = 2.5\n",
    );
    let default = must_ok(&BotConfigLoader::new().without_env());
    assert_eq!(
        default.config.pump.quiesce_mode,
        degenbot_config::QuiesceMode::Adaptive,
        "default mode is adaptive"
    );
    let from_file = must_ok(&BotConfigLoader::new().without_env().with_config_path(&path));
    assert_eq!(
        from_file.config.pump.quiesce_mode,
        degenbot_config::QuiesceMode::Adaptive,
        "file layer drives quiesce_mode"
    );
    assert_eq!(from_file.config.pump.quiesce_floor_ms, 3);
    assert!((from_file.config.pump.quiesce_margin_ms - 2.5).abs() < f64::EPSILON);
    let from_env = must_ok(
        &BotConfigLoader::new()
            .with_env(map_env(&[("DEGENBOT_PUMP_QUIESCE_MODE", "fixed")]))
            .with_config_path(&path),
    );
    assert_eq!(
        from_env.config.pump.quiesce_mode,
        degenbot_config::QuiesceMode::Fixed,
        "env layer beats the file layer"
    );
    assert_eq!(
        from_env.config.pump.quiesce_floor_ms, 3,
        "env-unset keys keep their file value"
    );
    cleanup(&path);
}

#[test]
fn every_layer_for_every_type_in_sequence() {
    // (BM35LK: the quiesce keys ride the same loader machinery — asserted
    // for env+file precedence below in `quiesce_keys_precedence_chain`.)
    // One full precedence chain per representative type.
    let path = temp_toml(
        "chain",
        "[solve]\nenvelope_max_tangent_lines = 64\n\n[state_lock]\nwarn_ms = 1500\n\n[allocator]\nmimalloc_auto_purge = false\n",
    );
    let loaded = must_ok(
        &BotConfigLoader::new()
            .with_env(map_env(&[
                ("DEGENBOT_ENVELOPE_MAX_TANGENT_LINES", "96"),
                ("DEGENBOT_LOCK_WARN_MS", "2500"),
                ("DEGENBOT_MIMALLOC_AUTO_PURGE", "0"),
            ]))
            .with_config_path(&path)
            .with_cli_overrides([
                (
                    "DEGENBOT_ENVELOPE_MAX_TANGENT_LINES".to_string(),
                    "7".to_string(),
                ),
                ("DEGENBOT_LOCK_WARN_MS".to_string(), "42".to_string()),
                (
                    "DEGENBOT_MIMALLOC_AUTO_PURGE".to_string(),
                    "true".to_string(),
                ),
            ]),
    );
    assert_eq!(
        loaded.config.solve.envelope_max_tangent_lines, 7,
        "cli top (usize)"
    );
    assert_eq!(
        loaded.config.state_lock.warn_ms, 42,
        "cli top (duration-ms)"
    );
    assert!(
        loaded.config.allocator.mimalloc_auto_purge,
        "cli top (bool)"
    );
    cleanup(&path);
}

#[test]
fn loader_fails_closed_on_bad_values_and_unknown_keys() {
    // LW-T9: the RETIRED stance key is not a schema key anymore — a CLI
    // override naming it is rejected as unknown (fail-closed, no silent
    // fallback).
    let err = must_err(
        &BotConfigLoader::new()
            .without_env()
            .with_cli("DEGENBOT_FLEET", "ninja"),
    );
    assert!(
        format!("{err}").contains("DEGENBOT_FLEET"),
        "the retired key is rejected: {err}"
    );

    // the RETIRED executor key is not a schema key anymore — a CLI
    // override naming it is rejected as unknown (fail-closed).
    let err = must_err(
        &BotConfigLoader::new()
            .without_env()
            .with_cli("DEGENBOT_SOLVE_EXECUTOR", "tokio"),
    );
    assert!(
        format!("{err}").contains("DEGENBOT_SOLVE_EXECUTOR"),
        "the retired key is rejected: {err}"
    );

    // WFF6MM hard cutover: the RETIRED detached-solve stance key is not a
    // schema key anymore — a CLI override naming it is rejected as unknown
    // (fail-closed), and a surviving TOML key fails with the generic
    // unknown-key error (the one-release pointed refusal was removed at
    // d5c450c84; ADR-012 deletion, not a flag).
    let err = must_err(
        &BotConfigLoader::new()
            .without_env()
            .with_cli("DEGENBOT_DETACHED_SOLVES", "0"),
    );
    assert!(
        format!("{err}").contains("DEGENBOT_DETACHED_SOLVES"),
        "the retired detached-solve key is rejected: {err}"
    );
    let retired_toml = temp_toml("detached_solves", "[solve]\ndetached_solves = true\n");
    let err = must_err(
        &BotConfigLoader::new()
            .without_env()
            .with_config_path(&retired_toml),
    );
    assert!(
        format!("{err}").contains("unknown key detached_solves"),
        "a surviving solve.detached_solves TOML key must fail closed: {err}"
    );
    cleanup(&retired_toml);

    // Unknown TOML key -> error.
    let path = temp_toml("unknown", "[nope]\nflag = true\n");
    let err = must_err(&BotConfigLoader::new().without_env().with_config_path(&path));
    assert!(format!("{err}").contains("unknown section"));
    cleanup(&path);

    // Unknown CLI key -> error.
    let err = must_err(
        &BotConfigLoader::new()
            .without_env()
            .with_cli("NOT_A_SCHEMA_KEY_1", "1"),
    );
    assert!(
        format!("{err}").contains("does not name a schema key"),
        "unknown cli keys are rejected"
    );
}

#[test]
fn missing_config_file_is_reported() {
    let err = must_err(
        &BotConfigLoader::new()
            .without_env()
            .with_config_path("/nonexistent/degenbot-config-should-not-exist.toml"),
    );
    assert!(format!("{err}").contains("unreadable"));
}

// ---- ambient-runtime sizing key (`runtime.io_workers`) ----

#[test]
fn runtime_io_workers_unset_by_default_and_derived_marker() {
    let loaded = must_ok(&BotConfigLoader::new().without_env());
    assert_eq!(
        loaded.config.runtime.io_workers, None,
        "ambient workers default to the CPU-budget derivation, not a fixed count"
    );
}

#[test]
fn runtime_io_workers_env_key_loads_with_provenance() {
    let loaded =
        must_ok(&BotConfigLoader::new().with_env(map_env(&[("DEGENBOT_IO_WORKERS", "4")])));
    assert_eq!(loaded.config.runtime.io_workers, Some(4));
    assert_eq!(
        loaded.source_of("DEGENBOT_IO_WORKERS"),
        Some(Source::Env),
        "the declared DEGENBOT_* env name must map onto the typed field"
    );
}

#[test]
fn runtime_io_workers_file_and_env_precedence() {
    let path = temp_toml("runtime", "[runtime]\nio_workers = 6\n");
    let loaded = must_ok(
        &BotConfigLoader::new()
            .with_env(map_env(&[("DEGENBOT_IO_WORKERS", "4")]))
            .with_config_path(&path),
    );
    assert_eq!(loaded.config.runtime.io_workers, Some(4), "env beats file");
    assert_eq!(loaded.source_of("DEGENBOT_IO_WORKERS"), Some(Source::Env));
    let from_file = must_ok(&BotConfigLoader::new().without_env().with_config_path(&path));
    assert_eq!(from_file.config.runtime.io_workers, Some(6), "file wins");
    assert_eq!(
        from_file.source_of("DEGENBOT_IO_WORKERS"),
        Some(Source::File)
    );
    cleanup(&path);
}

#[test]
fn runtime_io_workers_invalid_value_fails_closed() {
    let err =
        must_err(&BotConfigLoader::new().with_env(map_env(&[("DEGENBOT_IO_WORKERS", "lots")])));
    assert!(
        format!("{err}").contains("runtime.io_workers"),
        "error names the typed key: {err}"
    );
}

/// Post-migration contract: the retired env names are plain unknown env —
/// the migration-shim refusals have been removed, so a surviving variable
/// neither fails the load nor resurrects legacy behavior (it is ignored).
#[test]
fn retired_env_shims_are_ignored() {
    let ok = BotConfigLoader::new()
        .with_env(map_env(&[
            ("TOKIO_WORKER_THREADS", "2"),
            ("DEGENBOT_SOLVE_EXECUTOR", "tokio"),
            ("DEGENBOT_FLEET", "fleet"),
            ("DEGENBOT_SOLVE_SIM_INFLIGHT", "8"),
            ("DEGENBOT_LPT_PARTITION", "4"),
            ("DEGENBOT_DETACHED_SOLVES", "0"),
        ]))
        .load();
    assert!(ok.is_ok(), "retired env names are ignored: {ok:?}");
}

#[test]
fn unset_option_keys_stay_unset_and_default_provenance_holds() {
    let loaded = must_ok(&BotConfigLoader::new().without_env());
    assert_eq!(loaded.config.allocator.mimalloc_purge_delay_ms, None);
    assert_eq!(
        loaded.provenance.len(),
        degenbot_config::SCHEMA.len(),
        "one provenance entry per schema key"
    );
    for key in degenbot_config::SCHEMA {
        assert_eq!(loaded.source_of(key.env), Some(Source::Default));
    }
}
