//! Drain-path profiling guard — the hotpath tracer bullet.
//!
//! [hotpath-rs](https://hotpath.rs) instruments functions, channels, locks, and
//! threads. The `#[hotpath::measure]` attributes sprinkled across the
//! block-pump → `SolveCoordinator` → `EngineHandle` drain path are the lasting
//! instrumentation: they are **no-ops unless the `hotpath` Cargo feature is
//! enabled** (hotpath's `lib_off.rs` expands them to nothing), so default
//! builds pay zero compile-time or runtime cost.
//!
//! This module owns the **guard lifetime**: hotpath's background measurement
//! processor only runs while a single `HotpathGuard` is alive, and the report
//! is generated when that guard is dropped. `degenbot-bot` is a `cdylib`
//! driven by Python (no Rust `main`), so `#[hotpath::main]` is unavailable —
//! [`hotpath_guard`] builds the guard programmatically via
//! `HotpathGuardBuilder` and is held for the pump loop's lifetime by
//! [`BlockPump::run_with_stream`](crate::bot_core::block_pump::BlockPump::run_with_stream).
//!
//! # Operational gating
//!
//! Constructing a second `HotpathGuard` panics, and the cdylib is loaded into
//! a Python process that may spin up several bots / tests. So the guard is
//! **opt-in via the `DEGENBOT_HOTPATH=1` env var** — absent it,
//! [`hotpath_guard`] returns `None` and no guard is built. Combine with
//! hotpath's own env vars to shape the report, e.g.:
//!
//! ```text
//! DEGENBOT_HOTPATH=1 \
//! HOTPATH_OUTPUT_FORMAT=json \
//! HOTPATH_OUTPUT_PATH=hp.json \
//! HOTPATH_REPORT=functions-timing,threads \
//! HOTPATH_SHUTDOWN_MS=30000 \
//! uv run python ...
//! ```
//!
//! `HOTPATH_SHUTDOWN_MS` forces a timed report for a long-running bot (the
//! guard otherwise only drops when the pump exits). For a live view, run
//! `hotpath console` ( installed separately via `cargo install hotpath
//! --features=tui`) while the instrumented bot runs.
//!
//! # Why here, why the drain path
//!
//! The pump → coordinator → engine-handle path is the canonical
//! "Rust-is-the-engine" layer (ADR-005 / ADR-006): a tokio task driving WS
//! `newHeads` + `logs` through a `DrainSink` → `Arc<dyn Engine>` fan-out under
//! the `drain_lock → engine-Mutex → BotState RwLock` ordering. It is where
//! latency = lost MEV, and it combines every hotpath capability relevant
//! here: tokio runtime, the result-batch mpsc channel, three nested locks,
//! and CPU-bound solve work. See `docs/architecture/rust-owned-bot.md` for the
//! component map.

/// The profiling guard type. With the `hotpath` feature off this is hotpath's
/// no-op `HotpathGuard` (a zero-sized stub from `lib_off.rs`); with it on it
/// is the real guard whose drop writes the report.
pub type Guard = hotpath::HotpathGuard;

/// Build a hotpath guard for the pump run, iff the operator opted in.
///
/// Returns `None` (no guard) unless `DEGENBOT_HOTPATH=1` is set, so the
/// single-guard invariant can't be tripped by default runs, tests, or a
/// Python process hosting multiple bots. When the `hotpath` Cargo feature is
/// off the returned guard is a no-op stub — calling this is always safe.
#[must_use]
pub fn hotpath_guard(caller_name: &'static str) -> Option<Guard> {
    // Honor hotpath's own all-off path: when the feature isn't enabled the
    // builder still exists (no-op) but we skip constructing it anyway so
    // there's not even a stub object held across the loop. The env gate
    // applies in both cases.
    if std::env::var("DEGENBOT_HOTPATH").as_deref() != Ok("1") {
        return None;
    }
    #[cfg(feature = "hotpath")]
    {
        let builder = hotpath::HotpathGuardBuilder::new(caller_name);
        // If a timed shutdown is requested (HOTPATH_SHUTDOWN_MS), hand the
        // guard to hotpath's own background thread — it sleeps for the
        // duration, drops the guard (→ writes the report), then exits the
        // process. This is the clean way to capture a fixed window from a
        // long-running bot (the pump loop never returns on its own). Without
        // it, build() returns a guard the caller holds so the report lands on
        // normal pump exit.
        if std::env::var("HOTPATH_SHUTDOWN_MS").is_ok() {
            builder.build_with_shutdown(std::time::Duration::from_secs(0));
            tracing::info!(
                caller_name,
                "hotpath: profiling active — timed exit via HOTPATH_SHUTDOWN_MS"
            );
            return None;
        }
        let guard = builder.build();
        tracing::info!(
            caller_name,
            "hotpath: profiling guard active (report on pump exit)"
        );
        Some(guard)
    }
    #[cfg(not(feature = "hotpath"))]
    {
        // Feature off: the builder is a no-op stub, but constructing it would
        // still mislead the operator (they set DEGENBOT_HOTPATH=1 without
        // enabling the Cargo feature). Surface that instead of silently
        // profiling nothing.
        let _ = caller_name;
        tracing::warn!(
            "hotpath: DEGENBOT_HOTPATH=1 set but the hotpath Cargo feature is not \
             enabled on degenbot-bot — rebuild with --features hotpath"
        );
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test for the guard lifecycle. Ignored by default — it touches the
    /// process-global `DEGENBOT_HOTPATH` env var AND hotpath's singleton guard,
    /// so it must run alone. Run with:
    ///
    /// ```text
    /// DEGENBOT_HOTPATH=1 \
    /// cargo test -p degenbot-bot --features hotpath profiling:: -- --ignored --nocapture
    /// ```
    ///
    /// Without the env var (or without the feature) `hotpath_guard` returns
    /// `None`; with both it returns a guard whose drop writes a report.
    #[test]
    #[ignore = "touches global env + hotpath guard singleton; run alone"]
    fn guard_constructs_only_when_opted_in() {
        // Env var unset → no guard.
        std::env::remove_var("DEGENBOT_HOTPATH");
        std::env::remove_var("HOTPATH_SHUTDOWN_MS");
        assert!(hotpath_guard("smoke").is_none());

        // Opt in, no timed shutdown → guard returned (held by caller; report
        // on drop at scope end).
        std::env::set_var("DEGENBOT_HOTPATH", "1");
        std::env::remove_var("HOTPATH_SHUTDOWN_MS");
        let guard = hotpath_guard("smoke");
        #[cfg(feature = "hotpath")]
        {
            assert!(guard.is_some(), "opted-in + feature on → guard built");
        }
        #[cfg(not(feature = "hotpath"))]
        {
            assert!(guard.is_none(), "feature off → no guard even if opted in");
        }
        // Guard (if any) drops at scope end — when the `hotpath` feature is
        // on this writes the report; the no-op stub when off has nothing to drop.
    }

    /// `HOTPATH_SHUTDOWN_MS` switches the builder to the autonomous
    /// background-thread shutdown path — hotpath owns the guard, so
    /// `hotpath_guard` returns `None` (the caller holds nothing). Ignored:
    /// would exit the process after the timeout.
    #[test]
    #[ignore = "would trigger process exit via HOTPATH_SHUTDOWN_MS; run alone"]
    fn guard_uses_timed_shutdown_when_env_set() {
        std::env::set_var("DEGENBOT_HOTPATH", "1");
        std::env::set_var("HOTPATH_SHUTDOWN_MS", "999999"); // far future; test just checks the branch
        let guard = hotpath_guard("smoke_shutdown");
        #[cfg(feature = "hotpath")]
        {
            assert!(
                guard.is_none(),
                "HOTPATH_SHUTDOWN_MS set → hotpath owns the guard, caller holds nothing"
            );
        }
        #[cfg(not(feature = "hotpath"))]
        {
            assert!(guard.is_none());
        }
        std::env::remove_var("HOTPATH_SHUTDOWN_MS");
    }
}
