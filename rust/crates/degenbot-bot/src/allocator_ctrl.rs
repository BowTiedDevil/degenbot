//! Runtime mimalloc purge-delay control driven by observed block cadence
//! (epic AZZDBI task XXJR3A).
//!
//! # Why
//!
//! mimalloc's default purge delay (~10ms) discards freed pages almost
//! immediately. The T2 baseline soak measured the consequence on a 12s-cadence
//! chain: every block's transient working set (~194 MiB average, worst ~670
//! MiB) is purged between blocks and re-faulted at the next burst —
//! 49,612 ± 39,893 minor faults per block, paid at burst start, the worst
//! moment for solve-phase p95. A purge delay on the order of a few block
//! intervals keeps the working set resident across blocks while still
//! self-correcting (no glibc-style arena ratchet: retention stays bounded by
//! the actual working set, not scaled by arena count).
//!
//! # Design
//!
//! - A pure [`CadenceState`] records header-arrival wall-clock intervals
//!   (trailing window of [`WINDOW`] samples; intervals outside
//!   [`MIN_INTERVAL_SECS`]..[`MAX_INTERVAL_SECS`] — gaps, reorg double-fires,
//!   burst double-headers — are discarded but still re-anchor the clock).
//!   Once [`MIN_BLOCKS`] valid samples accumulate, the delay target is
//!   `mult x mean`, clamped to [`MIN_DELAY_MS`]..[`MAX_DELAY_MS`].
//! - Re-application is rate-limited by a 10% hysteresis band on the applied
//!   value so the mimalloc option is not churned every block.
//! - Config precedence: fixed `DEGENBOT_MIMALLOC_PURGE_DELAY_MS` overrides
//!   everything; otherwise auto-discovery runs (default ON,
//!   `DEGENBOT_MIMALLOC_AUTO_PURGE=0` disables);
//!   `DEGENBOT_MIMALLOC_PURGE_DELAY_MULT` scales the mean (default 2.0).
//! - Without the `allocator-ctrl` cargo feature the whole module is inert:
//!   the tracker still compiles for tests, but no mimalloc symbols are
//!   touched and a one-time warn surfaces the no-op (same pattern as
//!   `profiling.rs`).
//!
//! # Safety notes
//!
//! `mi_option_set` is documented not-thread-safe; the single writer is the
//! pump task's header arm (plus one startup init), and option writes are
//! idempotent monotone values, so contention is structurally excluded.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

/// mimalloc v2 `mi_option_purge_delay` enum index (vendored
/// `libmimalloc-sys` 0.1.49 builds mimalloc 2.3.02 by default; the Rust
/// bindings intentionally omit experimental option constants, so the index
/// is pinned here against the vendored `mimalloc.h` enum and the version
/// guard in `supported_version`).
#[cfg_attr(not(feature = "allocator-ctrl"), allow(dead_code))]
const MI_OPTION_PURGE_DELAY: i32 = 15;
/// `mi_option_purge_decommits` — same index (5) in both vendored v2 and v3
/// headers. Set to 0: purges use `MADV_FREE` (lazy reclaim) instead of
/// `MADV_DONTNEED`, so freed pages stay mapped and their zero-refault reuse
/// is measurable until the kernel actually needs them under memory pressure.
/// Matrix arm `madv-free` (epic AZZDBI T3): faults/block 5,598 vs 49,612 at
/// default, best `on_drain` p95 of all arms, RSS delta fully reclaimable.
const MI_OPTION_PURGE_DECOMMITS: i32 = 5;
/// Also `mi_option_purge_decommits` (index 5) — same in v2/v3.
/// The vendored C source ships BOTH mimalloc v2 (2.3.02,
/// `MI_MALLOC_VERSION 20302`) and v3 sources; the dev `.so` observed live at
/// `mi_version() == 30302` (v3). Index `15` was verified byte-identical in both
/// vendored headers (`mi_option_purge_delay`), so allow the 2.x and 3.x
/// majors and refuse anything else (v1 or a future major reordering).
#[cfg_attr(not(feature = "allocator-ctrl"), allow(dead_code))]
const SUPPORTED_VERSION_MIN: i32 = 20_000;
#[cfg_attr(not(feature = "allocator-ctrl"), allow(dead_code))]
const SUPPORTED_VERSION_MAX: i32 = 40_000;

pub const MIN_DELAY_MS: i64 = 2_000;
pub const MAX_DELAY_MS: i64 = 600_000;
const WINDOW: usize = 30;
const MIN_BLOCKS: usize = 20;
const DEFAULT_MULT: f64 = 2.0;
// Hysteresis band: 10 percent of the applied value before re-applying,
// written as integer math in `observe` (`delta * 10 > applied`) to avoid a
// float cast under the cast-precision lint.
const MIN_INTERVAL_SECS: f64 = 2.0;
const MAX_INTERVAL_SECS: f64 = 60.0;

const ENV_FIXED: &str = "DEGENBOT_MIMALLOC_PURGE_DELAY_MS";
const ENV_AUTO: &str = "DEGENBOT_MIMALLOC_AUTO_PURGE";
const ENV_MULT: &str = "DEGENBOT_MIMALLOC_PURGE_DELAY_MULT";
const ENV_PURGE_DECOMMITS: &str = "DEGENBOT_MIMALLOC_PURGE_DECOMMITS";

static AUTO_ENABLED: AtomicBool = AtomicBool::new(true);
static INIT_DONE: OnceLock<()> = OnceLock::new();

#[must_use]
pub fn clamp_delay_ms(ms: i64) -> i64 {
    ms.clamp(MIN_DELAY_MS, MAX_DELAY_MS)
}

/// Trailing-window simple mean x multiplier, clamped. `None` until
/// `min_blocks` interval samples exist — the engine must have seen enough
/// blocks before it is allowed to retune mimalloc.
#[must_use]
pub fn compute_purge_delay_ms(intervals: &[f64], mult: f64, min_blocks: usize) -> Option<i64> {
    if intervals.len() < min_blocks {
        return None;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "window length is bounded far below 2^52"
    )]
    let mean = intervals.iter().sum::<f64>() / intervals.len() as f64;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "delay_ms is clamped to <= 600_000 before the round, i64-safe"
    )]
    let ms = (mean * 1000.0 * mult).round() as i64;
    Some(clamp_delay_ms(ms))
}

/// Resolved startup configuration from the environment.
#[derive(Debug, Clone, PartialEq)]
pub struct PurgeConfig {
    /// Fixed delay (ms) that overrides discovery entirely.
    pub fixed_ms: Option<i64>,
    /// Whether cadence discovery may re-apply the option.
    pub auto: bool,
    /// Multiplier: delay = mult x mean block interval.
    pub mult: f64,
    /// Purge with `MADV_DONTNEED` (immediate reclaim, high refault churn)
    /// instead of `MADV_FREE` (lazy, kernel-pressure fenced). Default false.
    pub decommits: bool,
}

/// Env parsing: fixed override > auto flag > mult. Malformed values fall
/// back to defaults (fail-open, mirroring the hotpath/otel env gates).
#[must_use]
pub fn config_from_env() -> PurgeConfig {
    let fixed_ms = std::env::var(ENV_FIXED)
        .ok()
        .and_then(|raw| raw.replace('_', "").parse::<i64>().ok())
        .map(clamp_delay_ms);
    let auto = std::env::var(ENV_AUTO) != Ok(String::from("0"));
    let mult = std::env::var(ENV_MULT)
        .ok()
        .and_then(|raw| raw.parse::<f64>().ok())
        .map_or(DEFAULT_MULT, |m| m.clamp(1.0, 20.0));
    // Default OFF (MADV_FREE): the T3 matrix arm measured -89 percent
    // refault churn at equal/better solve p95; `=1/true` restores mimalloc's
    // aggressive decommit behavior.
    let decommits = matches!(
        std::env::var(ENV_PURGE_DECOMMITS).as_deref(),
        Ok("1" | "true")
    );
    PurgeConfig {
        fixed_ms,
        auto,
        mult,
        decommits,
    }
}

/// Cadence tracker: pure, testable, one instance per pump.
struct CadenceState {
    window: VecDeque<f64>,
    last_header_ms: Option<u64>,
    applied_ms: Option<i64>,
    mult: f64,
    min_blocks: usize,
}

impl CadenceState {
    fn new(mult: f64, min_blocks: usize) -> Self {
        Self {
            window: VecDeque::with_capacity(WINDOW),
            last_header_ms: None,
            applied_ms: None,
            mult,
            min_blocks,
        }
    }

    /// Record a header arrival at `now_ms`; return the newly-applied delay
    /// only when the hysteresis band allows a re-apply.
    fn observe(&mut self, now_ms: u64) -> Option<i64> {
        if let Some(last) = self.last_header_ms {
            let dt_secs =
                f64::from(u32::try_from(now_ms.saturating_sub(last)).unwrap_or(u32::MAX)) / 1000.0;
            if (MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS).contains(&dt_secs) {
                self.window.push_back(dt_secs);
                while self.window.len() > WINDOW {
                    self.window.pop_front();
                }
            }
            // Invalid intervals (gaps, reorg double-fires, sub-second
            // double-headers) never enter the window, but the clock always
            // re-anchors so the NEXT interval measures from this header.
        }
        self.last_header_ms = Some(now_ms);
        let target =
            compute_purge_delay_ms(self.window.make_contiguous(), self.mult, self.min_blocks)?;
        // Hysteresis: only re-apply when the target moved >10% off the
        // applied value (or nothing was applied yet).
        let changed = match self.applied_ms {
            None => true,
            // Integer form of the 10 percent band (no float casts).
            Some(prev) => (target - prev).abs() * 10 > prev,
        };
        if !changed {
            return None;
        }
        self.applied_ms = Some(target);
        Some(target)
    }
}

#[cfg_attr(not(feature = "allocator-ctrl"), allow(dead_code))]
fn supported_version(version: i32) -> bool {
    (SUPPORTED_VERSION_MIN..SUPPORTED_VERSION_MAX).contains(&version)
}

#[cfg(feature = "allocator-ctrl")]
static VERSION_OK: OnceLock<bool> = OnceLock::new();

#[cfg(feature = "allocator-ctrl")]
fn apply_decommits(decommits: bool) {
    if !*VERSION_OK.get_or_init(|| {
        // SAFETY: pure function over the vendored C statics; no allocation.
        let v = unsafe { libmimalloc_sys::mi_version() };
        supported_version(v)
    }) {
        return;
    }
    // SAFETY: same single-writer exclusion as apply_delay_ms.
    unsafe {
        libmimalloc_sys::mi_option_set_enabled(MI_OPTION_PURGE_DECOMMITS, decommits);
    }
    tracing::info!(
        decommits,
        "[allocator-ctrl] mimalloc purge decommits applied (false = MADV_FREE)"
    );
}

#[cfg(feature = "allocator-ctrl")]
fn apply_delay_ms(delay_ms: i64) {
    let ok = VERSION_OK.get_or_init(|| {
        // SAFETY: pure function over the vendored C statics; no allocation.
        let v = unsafe { libmimalloc_sys::mi_version() };
        let ok = supported_version(v);
        if !ok {
            tracing::warn!(
                version = v,
                "[allocator-ctrl] unsupported mimalloc major - purge-delay control disabled"
            );
        }
        ok
    });
    if !*ok {
        return;
    }
    // SAFETY: single-writer (pump header arm + one startup init); option
    // writes are idempotent integer stores. mi_option_set is documented as
    // not-thread-safe; contention is structurally excluded by the caller.
    unsafe {
        libmimalloc_sys::mi_option_set(MI_OPTION_PURGE_DELAY, delay_ms as core::ffi::c_long);
    }
    tracing::info!(
        delay_ms,
        "[allocator-ctrl] mimalloc purge delay applied from block cadence"
    );
}

#[cfg(not(feature = "allocator-ctrl"))]
fn apply_delay_ms(delay_ms: i64) {
    static WARNED: OnceLock<()> = OnceLock::new();
    if WARNED.set(()).is_ok() {
        tracing::warn!(
            delay_ms,
            "[allocator-ctrl] computed purge delay but the allocator-ctrl \
             cargo feature is not enabled - mimalloc keeps its default delay"
        );
    }
}

static CADENCE: Mutex<Option<CadenceState>> = Mutex::new(None);

#[must_use]
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Startup: apply any fixed override immediately; arm auto-discovery.
/// Called once from the pump start (next to the hotpath guard).
pub fn init_from_env_at_pump_start() {
    let cfg = config_from_env();
    if INIT_DONE.set(()).is_err() {
        return; // another pump in this process already configured the seam
    }
    apply_decommits(cfg.decommits);
    if let Some(ms) = cfg.fixed_ms {
        AUTO_ENABLED.store(false, Ordering::Relaxed);
        apply_delay_ms(clamp_delay_ms(ms));
        return;
    }
    AUTO_ENABLED.store(cfg.auto, Ordering::Relaxed);
    if cfg.auto {
        tracing::info!(
            mult = cfg.mult,
            min_blocks = MIN_BLOCKS,
            window = WINDOW,
            "[allocator-ctrl] block-cadence purge-delay discovery armed"
        );
    }
}

/// Pump hook: call on every live block header (the header arm of the pump
/// loop, next to the telemetry beat). Cheap: a mutex-guarded struct update,
/// and the mimalloc option write is hysteresis-rate-limited.
pub fn on_header_observed() {
    observe_at(now_ms());
}

fn observe_at(now_ms: u64) {
    if !AUTO_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let Ok(mut guard) = CADENCE.lock() else {
        return; // poisoned/contended: skip this beat, try the next header
    };
    let state = guard.get_or_insert_with(|| {
        let cfg = config_from_env();
        CadenceState::new(cfg.mult, MIN_BLOCKS)
    });
    if let Some(delay_ms) = state.observe(now_ms) {
        apply_delay_ms(delay_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> CadenceState {
        CadenceState::new(DEFAULT_MULT, MIN_BLOCKS)
    }

    #[test]
    fn compute_returns_none_below_min_blocks() {
        let ivs = [12.0; MIN_BLOCKS - 1];
        assert_eq!(compute_purge_delay_ms(&ivs, DEFAULT_MULT, MIN_BLOCKS), None);
    }

    #[test]
    fn compute_scales_mean_by_multiplier() {
        let ivs = [13.16; MIN_BLOCKS];
        assert_eq!(
            compute_purge_delay_ms(&ivs, DEFAULT_MULT, MIN_BLOCKS),
            Some(26_320)
        );
    }

    #[test]
    fn compute_clamps_to_bounds() {
        let fast = [1.0; MIN_BLOCKS];
        assert_eq!(
            compute_purge_delay_ms(&fast, DEFAULT_MULT, MIN_BLOCKS),
            Some(MIN_DELAY_MS)
        );
        let slow = [900.0; MIN_BLOCKS];
        assert_eq!(
            compute_purge_delay_ms(&slow, DEFAULT_MULT, MIN_BLOCKS),
            Some(MAX_DELAY_MS)
        );
    }

    #[test]
    fn observe_ignores_invalid_intervals_but_reanchors_clock() {
        let mut s = state();
        assert_eq!(s.observe(1_000), None); // first header: anchors
        assert_eq!(s.observe(401_000), None); // 400s gap: rejected
        assert_eq!(s.observe(401_100), None); // 0.1s double-fire: rejected
        assert_eq!(s.observe(401_150), None); // 0.05s: rejected
        assert_eq!(s.last_header_ms, Some(401_150), "clock re-anchored");
        for i in 1..=MIN_BLOCKS as u64 {
            let _ = s.observe(401_150 + i * 12_000);
        }
        assert_eq!(s.window.len(), MIN_BLOCKS, "valid intervals recorded");
    }

    #[test]
    fn observe_emits_delay_exactly_once_at_threshold() {
        let mut s = state();
        let mut applied: Vec<i64> = Vec::new();
        s.observe(1_000);
        for i in 1..=WINDOW as u64 {
            if let Some(d) = s.observe(1_000 + i * 12_000) {
                applied.push(d);
            }
        }
        assert_eq!(applied, vec![24_000]);
    }

    #[test]
    fn window_is_bounded_to_trailing_samples() {
        let mut s = state();
        s.observe(0);
        for i in 1..=(WINDOW * 2) as u64 {
            s.observe(i * 12_000);
        }
        assert_eq!(s.window.len(), WINDOW);
    }

    #[test]
    fn hysteresis_suppresses_small_drift() {
        let mut s = state();
        s.observe(0);
        for i in 1..=MIN_BLOCKS as u64 {
            let _ = s.observe(i * 12_000); // applies 24_000
        }
        assert_eq!(s.applied_ms, Some(24_000));
        // 13s mean -> target 26_000; 2000 <= 10% of 24_000 -> suppressed
        let mut got = None;
        for n in (MIN_BLOCKS + 1)..=(MIN_BLOCKS + WINDOW) {
            got = s.observe(u64::try_from(n).unwrap_or(u64::MAX) * 13_000);
        }
        assert_eq!(got, None);
    }

    #[test]
    fn hysteresis_permits_real_changes() {
        let mut s = state();
        s.observe(0);
        for i in 1..=MIN_BLOCKS as u64 {
            let _ = s.observe(i * 12_000); // applies 24_000
        }
        // Drain the window to all-40s intervals: the target mean migrates
        // stepwise and hysteresis legitimately applies intermediate values,
        // freezing when the target's drift falls inside the 10% band. Final
        // pure-40s target = 40s x 2 = 80_000ms; the applied value must have
        // re-applied at least once past 24_000 and sit within the band.
        let mut last_change: Option<i64> = None;
        for i in (MIN_BLOCKS + 1)..=(MIN_BLOCKS + WINDOW) {
            if let Some(d) = s.observe(i as u64 * 40_000) {
                last_change = Some(d);
            }
        }
        assert!(
            last_change.is_some(),
            "40s cadence must re-apply past 24_000"
        );
        let applied = s.applied_ms.unwrap_or_default();
        let final_target = 80_000i64; // 40s x 2.0
                                      // Integer form of the same 10 percent band.
        assert!(
            (final_target - applied).abs() * 10 <= applied,
            "applied {applied} must freeze within 10% of target {final_target}"
        );
    }

    #[test]
    fn config_env_parse_and_defaults_are_process_ordered() {
        // std env is process-global: parallel tests racing set_var/remove_var
        // would read each other's leftovers, so both scenarios live in ONE
        // serialized test (also locking out any future env-touching sibling).
        std::env::set_var(ENV_FIXED, "45_000");
        std::env::set_var(ENV_AUTO, "0");
        std::env::set_var(ENV_MULT, "3");
        std::env::set_var(ENV_PURGE_DECOMMITS, "1");
        let cfg = config_from_env();
        assert_eq!(cfg.fixed_ms, Some(45_000));
        assert!(!cfg.auto);
        assert!((cfg.mult - 3.0).abs() < f64::EPSILON);
        assert!(cfg.decommits, "=1 must restore decommit purges");

        std::env::remove_var(ENV_FIXED);
        std::env::remove_var(ENV_AUTO);
        std::env::remove_var(ENV_MULT);
        std::env::remove_var(ENV_PURGE_DECOMMITS);
        let cfg = config_from_env();
        assert_eq!(cfg.fixed_ms, None);
        assert!(cfg.auto);
        assert!((cfg.mult - DEFAULT_MULT).abs() < f64::EPSILON);
        assert!(!cfg.decommits, "MADV_FREE is the shipped default");
    }

    #[test]
    fn version_guard_bands() {
        assert!(supported_version(20_302));
        assert!(supported_version(30_302), "vendored dev .so is mimalloc v3");
        assert!(!supported_version(18_106));
        assert!(!supported_version(40_000));
    }
}
