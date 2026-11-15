//! Config-only boot: the lightest start-up path of the bot engine, driven by a
//! config TOML file ONLY (the loader never sees a DEGENBOT_* variable — see
//! the clean-environment proof below).
//!
//! The boot proves the same wiring the live pump performs once per engine:
//! loader (file layer) -> typed-config install -> stance packing ->
//! `EngineStages` construction with its instance `SolveRuntimeConfig`.
//!
//! `degenbot_substrate::stance::install` is a PROCESS GLOBAL, so the boot
//! never runs in the test runner's process. This is the crate's ONE test in
//! a dedicated integration binary, and it re-invokes its own executable in
//! child processes:
//!
//! 1. GUARD CHILD: planted with one `DEGENBOT_*` variable, it must refuse
//!    the boot — non-zero exit, stderr naming the refusal and the variable.
//!    The refusal is a real process-environment check (`std::env::vars()`),
//!    exercised the way an operator's polluted shell would trip it.
//! 2. CLEAN CHILD: environment constructed EMPTY apart from the child-mode
//!    flag, so no `DEGENBOT_*` variable can be visible to it. It performs
//!    the sole file-layer load (`.without_env()`), the sole stance
//!    installation, and the sole engine construction, then prints the four
//!    observations for the parent to assert. Sibling tests can never see
//!    the installed stance: it exists only in the child's process image.
//!
//! Failure mode: any wiring breakage is a non-zero exit (guard child) or a
//! failing assertion (parent), never a silent pass. The child-mode flag is
//! set only by this test itself, so the recursion is exactly one level deep.

#![expect(
    clippy::exit,
    clippy::expect_used,
    clippy::print_stdout,
    reason = "test-only: spawned children report their verdicts on stdout and exit their own process; the parent asserts against both"
)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

/// Child-mode flag. Deliberately NOT `DEGENBOT_*`: the clean child must be
/// provably free of that prefix while still selecting the child code path.
const CHILD_FLAG: &str = "CONFIG_SMOKE_BOOT_CHILD";

/// The one variable planted into the guard child's environment: an existing
/// real schema key, so the guard must refuse a legitimate-but-misplaced var.
const PLANTED_VAR: &str = "DEGENBOT_HOTPATH";

/// The refusal text the guard must report (preserved from the boot smoke).
const GUARD_TEXT: &str = "smoke boot requires NO DEGENBOT_* env";

/// Fixture values the parent asserts against the clean child's output; both
/// differ from the schema defaults so a defaulted (unwired) value fails.
const EXPECTED_MIN_PROFIT_WEI: &str = "1234500000000";
const EXPECTED_METRICS_ADDR: &str = "127.0.0.1:9177";

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config_smoke_boot.toml")
}

#[test]
fn config_only_boot_refuses_stray_env_and_packs_the_file_config() {
    if std::env::var_os(CHILD_FLAG).is_some() {
        run_child_scenario();
        return;
    }
    let exe = std::env::current_exe().expect("test executable path is resolvable");

    // Guard child: the planted variable must be refused, by name.
    let planted = Command::new(&exe)
        // --nocapture: the child's libtest harness must not swallow the
        // refusal text the parent asserts against.
        .arg("--nocapture")
        .env_clear()
        .env(CHILD_FLAG, "1")
        .env(PLANTED_VAR, "1")
        .output()
        .expect("guard child spawns");
    let stderr = String::from_utf8_lossy(&planted.stderr);
    assert!(
        !planted.status.success(),
        "guard child with a planted DEGENBOT_* variable must exit non-zero"
    );
    assert!(
        stderr.contains(GUARD_TEXT) && stderr.contains(PLANTED_VAR),
        "guard child must report '{GUARD_TEXT}' and '{PLANTED_VAR}'; stderr: {stderr}"
    );

    // Clean child: exactly one environment variable, no DEGENBOT_* prefix.
    let clean = Command::new(&exe)
        // --nocapture: the child's libtest harness must not swallow the
        // observation line the parent asserts against.
        .arg("--nocapture")
        .env_clear()
        .env(CHILD_FLAG, "1")
        .output()
        .expect("clean child spawns");
    let stdout = String::from_utf8_lossy(&clean.stdout);
    assert!(
        clean.status.success(),
        "clean-env child must boot: status={:?}, stdout: {stdout}, stderr: {}",
        clean.status.code(),
        String::from_utf8_lossy(&clean.stderr)
    );
    let observations = [
        "streaming_delivery=false".to_owned(),
        format!("min_profit_wei={EXPECTED_MIN_PROFIT_WEI}"),
        "solve_cpus_unset=true".to_owned(),
        format!("metrics_addr={EXPECTED_METRICS_ADDR}"),
    ];
    for expected in &observations {
        assert!(
            stdout.contains(expected.as_str()),
            "clean-env child must observe '{expected}' end-to-end; stdout: {stdout}"
        );
    }
}

/// The child half: boot the fixture or fail the process loudly. The parent
/// asserts against the child's exit status and captured output.
fn run_child_scenario() {
    if let Err(msg) = boot_scenario(&fixture_path()) {
        let mut stderr = std::io::stderr();
        let _ = writeln!(stderr, "{msg}");
        std::process::exit(1);
    }
}

fn boot_scenario(path: &Path) -> Result<(), String> {
    // Guard: the boot must be config-file-driven. Any DEGENBOT_* variable
    // visible to this process is a boot violation.
    let degenbot_env: Vec<_> = std::env::vars()
        .filter(|(k, _)| k.starts_with("DEGENBOT_"))
        .collect();
    if !degenbot_env.is_empty() {
        return Err(format!("{GUARD_TEXT}; found: {degenbot_env:?}"));
    }

    // The ONE env-reading site is the loader; this boot attaches the
    // environment-free layer (loader env disabled) so even a stray
    // DEGENBOT_* in the spawning environment cannot influence the load.
    let loaded = ::degenbot_config::BotConfigLoader::new()
        .with_config_path(path)
        .without_env()
        .load()
        .map_err(|e| format!("config load failed: {e}"))?;
    let cfg = Arc::new(loaded.config);

    // The boot path installs the typed config BEFORE any engine/pump
    // construction (stance::config() below serves the packed value).
    if !::degenbot_substrate::stance::install(Arc::clone(&cfg)) {
        return Err("a config was already installed in this process".into());
    }

    // Engine construction: the same construction the live pump performs
    // once per engine — stances packed from the typed config.
    let core = Arc::new(degenbot_substrate::state_lock::StateLock::new(
        degenbot_substrate::BotState::new(),
    ));
    // The ONE external construction seam: `EngineStages` builds the engine
    // internally (the engine type never crosses the crate boundary).
    let stages = degenbot_bot::arb_engine::EngineStages::with_core_cfg(
        core,
        &cfg,
        Arc::new(degenbot_substrate::EpochDelta::new(0u64)),
    );

    // Observe the packed stances end-to-end (config file -> engine field):
    // the fixture sets pump.streaming_delivery=false and solve.min_profit_wei;
    // failure to surface them proves a wiring break.
    println!(
        "config-only smoke boot OK: file={}, streaming_delivery={}, \
         min_profit_wei={}, solve_cpus_unset={}, metrics_addr={}",
        path.display(),
        stages.streaming_delivery_probe(),
        cfg.solve.min_profit_wei,
        cfg.solve.solve_cpus.is_none(),
        cfg.telemetry.metrics_addr,
    );
    Ok(())
}
