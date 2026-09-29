//! The substrate→application telemetry port.
//!
//! The pipeline instruments registry is application-owned (the host crate's
//! `instruments` module, behind its `otel` feature). The substrate cannot
//! depend on the host, so these hot paths report through a function-pointer
//! bundle the host installs once at boot ([`register`]). Unregistered builds
//! (pure-Rust consumers without the bot shell) observe every hook as absent
//! — identical to the pre-extraction `pipeline() == None` no-op path.

use std::sync::OnceLock;

/// Host-installed pipeline telemetry hooks. Call sites pattern-match
/// `pipeline()` exactly as they did against the application registry.
#[derive(Clone, Copy)]
pub struct PipelineInstruments {
    /// Quarantine-set depth gauge (ADR-040).
    pub set_quarantined_pools: fn(usize),
    /// State-lock wait/hold observations (incident 2026-08-21).
    pub observe_state_lock_wait: fn(&'static str, &'static str, f64),
    pub observe_state_lock_hold: fn(&'static str, &'static str, f64),
    /// Log intake → apply funnel counters/observations.
    pub count_log_received: fn(),
    pub observe_log_decode: fn(f64),
    pub count_log_decoded: fn(),
    pub count_log_undecoded: fn(),
    pub count_log_apply_missed: fn(),
    pub observe_state_apply: fn(f64),
    pub count_log_applied: fn(),
}

static PORT: OnceLock<Option<PipelineInstruments>> = OnceLock::new();

/// Install (or explicitly clear) the telemetry bundle. Later calls are
/// no-ops (`OnceLock`).
pub fn register(instruments: Option<PipelineInstruments>) {
    let _ = PORT.set(instruments);
}

/// The installed bundle, or `None` when the host never registered one.
#[must_use]
pub fn pipeline() -> Option<&'static PipelineInstruments> {
    PORT.get().and_then(|p| p.as_ref())
}

impl PipelineInstruments {
    /// Quarantine-set depth gauge (ADR-040).
    pub fn set_quarantined_pools(&self, depth: usize) {
        (self.set_quarantined_pools)(depth);
    }
    /// State-lock wait observation (incident 2026-08-21).
    pub fn observe_state_lock_wait(&self, site: &'static str, mode: &'static str, secs: f64) {
        (self.observe_state_lock_wait)(site, mode, secs);
    }
    /// State-lock hold observation.
    pub fn observe_state_lock_hold(&self, site: &'static str, mode: &'static str, secs: f64) {
        (self.observe_state_lock_hold)(site, mode, secs);
    }
    /// Log intake counter.
    pub fn count_log_received(&self) {
        (self.count_log_received)();
    }
    /// Decode-time observation.
    pub fn observe_log_decode(&self, secs: f64) {
        (self.observe_log_decode)(secs);
    }
    /// Decoded-log counter.
    pub fn count_log_decoded(&self) {
        (self.count_log_decoded)();
    }
    /// Undecoded-log counter.
    pub fn count_log_undecoded(&self) {
        (self.count_log_undecoded)();
    }
    /// Apply-miss counter.
    pub fn count_log_apply_missed(&self) {
        (self.count_log_apply_missed)();
    }
    /// State-apply time observation.
    pub fn observe_state_apply(&self, secs: f64) {
        (self.observe_state_apply)(secs);
    }
    /// Applied-log counter.
    pub fn count_log_applied(&self) {
        (self.count_log_applied)();
    }
}
