//! hotpath Prometheus exporter probe : the smallest
//! end-to-end proof that a guard-enabled build serves profiler metrics on
//! :6772.
//!
//! Procedure (the exporter must be compiled in):
//!   `DEGENBOT_HOTPATH=1` cargo run -p degenbot-bot \
//!       --features hotpath-prometheus --example `hotpath_prometheus_probe`
//!
//! The probe installs the typed config from the process environment (the
//! pure-Rust consumer path — `DEGENBOT_HOTPATH=1` maps to 'trace.hotpath'),
//! constructs the SAME guard the pump holds (`profiling::hotpath_guard`), then
//! scrapes http://127.0.0.1:{HOTPATH_PROMETHEUS_PORT:-6772}/metrics from the
//! exporter hotpath starts with the guard, asserting the always-present
//! process families and a measure_block!-sampled function family are
//! exported. Exits non-zero on any wiring breakage.

#![expect(
    clippy::print_stdout,
    reason = "diagnostic example: prints scrape evidence on stdout"
)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

/// Families that MUST appear in every scrape: the always-present process
/// section plus one function sample recorded below.
const REQUIRED_FAMILIES: [&str; 3] = [
    "hotpath_build_info",
    "hotpath_uptime_seconds",
    "hotpath_function_duration_seconds",
];

fn main() -> Result<(), String> {
    if !matches!(std::env::var("DEGENBOT_HOTPATH").as_deref(), Ok("1")) {
        return Err("run with DEGENBOT_HOTPATH=1 (the guard/runtime gate, see \
             rust/crates/engine/degenbot-bot/src/profiling.rs)"
            .into());
    }

    // Same load the live bot gets: schema defaults + DEGENBOT_* env (the
    // loader default keeps the process-env layer attached), then install —
    // ``profiling::hotpath_guard`` reads trace.hotpath off the installed config.
    let loaded = ::degenbot_config::BotConfigLoader::new()
        .load()
        .map_err(|e| format!("config load failed: {e}"))?;
    let cfg = Arc::new(loaded.config);
    if !::degenbot_bot::bot_core::stance::install(Arc::clone(&cfg)) {
        return Err("a config was already installed in this process".into());
    }

    // The exact production seam the pump uses (not a bare
    // `HotpathGuardBuilder`), so the probe exercises the real call site.
    let _guard = degenbot_bot::profiling::hotpath_guard("hotpath_prometheus_probe").ok_or(
        "hotpath_guard returned None — was the degenbot-bot 'hotpath' Cargo \
         feature enabled for this build?",
    )?;

    // One measured sample so hotpath_function_duration_seconds has data, and
    // a grace period for the exporter thread started by the guard.
    hotpath::measure_block!("probe.sleep", {
        std::thread::sleep(Duration::from_millis(1500));
    });

    let port: u16 = std::env::var("HOTPATH_PROMETHEUS_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(6772);
    let body = scrape(port)?;

    for family in REQUIRED_FAMILIES {
        if !body.contains(family) {
            return Err(format!(
                "exporter scrape on 127.0.0.1:{port} lacks required family '{family}' \
                 ({} bytes scraped)",
                body.len()
            ));
        }
    }
    println!(
        "hotpath prometheus export OK: port={port}, {} bytes, families \
         {:?} all present",
        body.len(),
        REQUIRED_FAMILIES
    );

    // The guard's scope exit (main returning) writes the end-of-profile
    // report (HOTPATH_OUTPUT_* knobs) and stops the exporter thread it
    // started. Binding as `_guard` keeps it alive to this point; the
    // feature-off stub types identically (the external `hotpath` crate's
    // no-op guard has no Drop impl, so an explicit `drop` would trip
    // clippy::drop_non_drop in default-feature builds).
    Ok(())
}

/// Plain-HTTP GET /metrics against the loopback exporter. hotpath serves the
/// text exposition format to non-protobuf scrapers (this client), so the body
/// is UTF-8 metric lines.
fn scrape(port: u16) -> Result<String, String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).map_err(|e| {
        format!(
            "exporter not reachable on 127.0.0.1:{port}: {e} — the exporter \
             starts with the guard; was hotpath-prometheus compiled in?"
        )
    })?;
    stream
        .write_all(b"GET /metrics HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
        .map_err(|e| format!("request write failed: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("read timeout set failed: {e}"))?;
    let mut body = String::new();
    stream
        .read_to_string(&mut body)
        .map_err(|e| format!("scrape read failed: {e}"))?;
    Ok(body)
}
