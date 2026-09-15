//! KAHU5W config-only smoke boot: the lightest start-up path of the bot
//! engine, driven by a config TOML file ONLY (the loader never sees a
//! DEGENBOT_* variable — see the exported clean-env guarantee below).
//!
//! Procedure:
//!   cargo run -p degenbot-bot --example `config_smoke_boot` -- <config.toml>
//!
//! A full live boot needs RPC; this main()-level construction proves the
//! same wiring the pump uses: loader (file layer) -> holder install ->
//! stance-packing -> `EngineStages` construction with its instance
//! `SolveRuntimeConfig`. Exits non-zero on any wiring breakage.

#![expect(
    clippy::print_stdout,
    reason = "diagnostic example: reports the packed stances on stdout"
)]

use std::sync::Arc;

fn main() -> Result<(), String> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: config_smoke_boot <config.toml> (file layer only; NO DEGENBOT_* env)")?;

    // Guard: the boot must be config-file-driven. Any DEGENBOT_* variable
    // visible to this process is a smoke-boot violation.
    let degenbot_env: Vec<_> = std::env::vars()
        .filter(|(k, _)| k.starts_with("DEGENBOT_"))
        .collect();
    if !degenbot_env.is_empty() {
        return Err(format!(
            "smoke boot requires NO DEGENBOT_* env; found: {degenbot_env:?}"
        ));
    }

    // The ONE env-reading site is the loader; this run attaches the
    // environment-free layer (loader env disabled) so even a stray
    // DEGENBOT_* in the checkpoint environment cannot influence the load.
    let loaded = ::degenbot_config::BotConfigLoader::new()
        .with_config_path(&path)
        .without_env()
        .load()
        .map_err(|e| format!("config load failed: {e}"))?;
    let cfg = Arc::new(loaded.config);

    // The boot path installs the typed config BEFORE any engine/pump
    // construction (stance::config() below serves the packed value).
    if !::degenbot_bot::bot_core::stance::install(Arc::clone(&cfg)) {
        return Err("a config was already installed in this process".into());
    }

    // Main()-level engine construction: the same construction the live pump
    // performs once per engine — stances packed from the typed config.
    let core = Arc::new(degenbot_bot::bot_core::state_lock::StateLock::new(
        degenbot_bot::bot_core::BotState::new(),
    ));
    // The ONE external construction seam: `EngineStages` builds the engine
    // internally (the engine type never crosses the crate boundary).
    let stages = degenbot_bot::arb_engine::EngineStages::with_core_cfg(
        core,
        &cfg,
        Arc::new(degenbot_bot::bot_core::EpochDelta::new(0u64)),
    );

    // Observe the packed stances end-to-end (config file -> engine field):
    // the file sets pump.streaming_delivery=false and solve.min_profit_wei;
    // failure to surface them proves a wiring break.
    println!(
        "config-only smoke boot OK: file={path}, streaming_delivery={}, \
         min_profit_wei={}, solve_cpus_unset={}, metrics_addr={}",
        stages.streaming_delivery_probe(),
        cfg.solve.min_profit_wei,
        cfg.solve.solve_cpus.is_none(),
        cfg.telemetry.metrics_addr,
    );
    Ok(())
}
