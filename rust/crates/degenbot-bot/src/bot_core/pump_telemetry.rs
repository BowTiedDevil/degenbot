//! The block-pump investigation telemetry seam (`PumpTelemetry`).
//!
//! One module owns the block pump's **diagnostic/instrumentation** concern — the
//! `[DIAG]` header-stall counters, the periodic freeze-probe stats, and the
//! live-stream liveness signals that a long-running freeze/de-sync investigation
//! (`ergo 3YA7ZJ`) left woven into the hot loop. The decision maker
//! (`BlockPump::run_with_stream`) calls a tiny API (`on_header` / `on_log` /
//! `maybe_stats`); all counters, intervals, and stats emission live here.
//!
//! This is a **pure telemetry seam** — no provider, no `Bot` handle, no bot
//! state. `maybe_stats` takes the pool-state head as data so the module stays
//! I/O-free and trivially testable.
//!
//! Because every diagnostic is behind one interface, the *deletion test*
//! applies cleanly in the intended direction: when the investigation closes,
//! removing the whole seam (or the gated removals tracked by `ergo VSDZ4X`)
//! means deleting this one module — not surgically carving interleaved `[DIAG]`
//! sites out of the decision loop. Until then, behavior is byte-identical to
//! the inline instrumentation it replaces.
//!
//! Psst — naming: this seals the *block-pump* telemetry. The solver-state
//! verifier's own env-gated probes (`solve_anchor_probe`, `staged_clock_probe`,
//! `divergence_scan`) stay grouped in `solver_state_verifier.rs`; they are
//! already self-contained gated functions and are not part of the pump's hot
//! loop.

use std::time::Duration;

use tokio::time::Instant;

/// How often the periodic `[DIAG] stats` log is emitted (see `maybe_stats`).
const DIAG_STATS_INTERVAL: Duration = Duration::from_secs(10);
/// A header gap beyond this window is flagged as a `[DIAG] HEADER STALL`.
const HEADER_STALL_WINDOW: Duration = Duration::from_secs(20);

/// Owns the block pump's diagnostic counters, intervals, and stats emission.
#[derive(Debug)]
pub struct PumpTelemetry {
    header_count: u64,
    log_count: u64,
    last_header_at: Instant,
    last_stats_at: Instant,
    stats_interval: Duration,
    stall_window: Duration,
    stat_emissions: u64,
}

impl PumpTelemetry {
    /// A telemetry tracker with the production windows (10s stats, 20s stall).
    #[must_use]
    pub fn new() -> Self {
        Self::with_windows(DIAG_STATS_INTERVAL, HEADER_STALL_WINDOW)
    }

    /// Construct with explicit windows so the cadence is deterministically
    /// testable (e.g. a zero stats interval always emits on the first call).
    fn with_windows(stats_interval: Duration, stall_window: Duration) -> Self {
        let now = Instant::now();
        Self {
            header_count: 0,
            log_count: 0,
            last_header_at: now,
            last_stats_at: now,
            stats_interval,
            stall_window,
            stat_emissions: 0,
        }
    }

    /// Record an accepted block header and emit the `[DIAG] HEADER` / stall
    /// signals (the newHeads-liveness questions: are headers arriving at all?).
    pub fn on_header(&mut self, number: u64) {
        self.header_count += 1;
        let gap = if self.header_count == 1 {
            0.0
        } else {
            self.last_header_at.elapsed().as_secs_f64()
        };
        tracing::info!(
            number,
            diag_header_count = self.header_count,
            gap_secs = %format!("{:.1}", gap),
            "BlockPump: [DIAG] HEADER"
        );
        if self.header_count > 1 && self.last_header_at.elapsed() > self.stall_window {
            tracing::warn!(
                number,
                silent_secs = %format!("{:.1}", gap),
                "BlockPump: [DIAG] *** HEADER STALL: headers were silent"
            );
        }
        self.last_header_at = Instant::now();
    }

    /// Record an applied relevant log (the "the pump IS polling logs even while
    /// headers are gone" liveness signal).
    pub fn on_log(&mut self) {
        self.log_count += 1;
    }

    /// Emit the periodic `[DIAG] stats`/freeze-probe log at most once per
    /// `stats_interval`. `current_block` is the pump's engine clock, and
    /// `pool_state_head` the max pool `update_block` — the two fields whose
    /// divergence is the post-backfill drain-freeze signature (`ergo 3YA7ZJ`).
    pub fn maybe_stats(&mut self, current_block: u64, pool_state_head: u64) {
        if self.last_stats_at.elapsed() < self.stats_interval {
            return;
        }
        self.stat_emissions += 1;
        let last_header_secs = self.last_header_at.elapsed().as_secs();
        tracing::info!(
            diag_header_count = self.header_count,
            diag_log_count = self.log_count,
            last_header_secs = last_header_secs,
            current_block,
            pool_state_head,
            "BlockPump: [DIAG] stats (fsm.clock vs pool-state freeze probe)"
        );
        self.last_stats_at = Instant::now();
    }

    /// Number of headers seen since construction (exposed for tests).
    #[must_use]
    pub fn header_count(&self) -> u64 {
        self.header_count
    }

    /// Number of applied logs seen since construction (exposed for tests).
    #[must_use]
    pub fn log_count(&self) -> u64 {
        self.log_count
    }

    /// Number of periodic stats logs emitted (exposed for tests).
    #[must_use]
    pub fn stat_emissions(&self) -> u64 {
        self.stat_emissions
    }
}

impl Default for PumpTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_header_increments_and_first_gap_is_zero() {
        let mut t = PumpTelemetry::new();
        assert_eq!(t.header_count(), 0);
        t.on_header(101);
        assert_eq!(t.header_count(), 1);
        t.on_header(102);
        assert_eq!(t.header_count(), 2);
        // The gap for the first header is 0 regardless of construction.
        // (We only assert the counter here; the gap is a log field.)
    }

    #[test]
    fn on_log_increments_independently_of_headers() {
        let mut t = PumpTelemetry::new();
        t.on_log();
        t.on_log();
        t.on_log();
        assert_eq!(t.log_count(), 3);
        assert_eq!(t.header_count(), 0);
    }

    #[test]
    fn zero_stats_interval_emits_on_first_maybe_stats() {
        // A zero interval means "always due" — every call emits.
        let mut t = PumpTelemetry::with_windows(Duration::ZERO, Duration::from_secs(20));
        assert_eq!(t.stat_emissions(), 0);
        t.maybe_stats(100, 100);
        assert_eq!(t.stat_emissions(), 1);
        t.maybe_stats(101, 100);
        assert_eq!(t.stat_emissions(), 2);
    }

    #[test]
    fn default_stats_interval_does_not_emit_immediately() {
        // The production 10s window means the very first call never emits.
        let mut t = PumpTelemetry::new();
        t.maybe_stats(100, 100);
        assert_eq!(t.stat_emissions(), 0);
    }
}
