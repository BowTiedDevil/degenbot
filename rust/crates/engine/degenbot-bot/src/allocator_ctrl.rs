//! Runtime mimalloc purge-delay control driven by observed block cadence
//!.
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
//! - [`CadenceState`] is a phase machine over header-arrival wall-clock
//!   intervals (trailing window of [`WINDOW`] samples; intervals outside
//!   [`MIN_INTERVAL_SECS`]..[`MAX_INTERVAL_SECS`] — gaps, reorg double-fires,
//!   burst double-headers — are discarded but still re-anchor the clock).
//!   The first header anchors, the [`MIN_BLOCKS`]th valid sample ends warm-up
//!   by applying the first target (`mult x mean`, clamped to
//!   [`MIN_DELAY_MS`]..[`MAX_DELAY_MS`]), and every header answers a
//!   [`CadenceOutcome`] naming the apply or the suppressed target.
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

use degenbot_core::{op_debug, op_info, op_warn};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

/// mimalloc v2 `mi_option_purge_delay` enum index (vendored
/// `libmimalloc-sys` 0.1.49 builds mimalloc 2.3.02 by default; the Rust
/// bindings intentionally omit experimental option constants, so the index
/// is pinned here against the vendored `mimalloc.h` enum and the version
/// guard in `supported_version`).
#[cfg_attr(not(feature = "allocator-ctrl"), expect(dead_code))]
const MI_OPTION_PURGE_DELAY: i32 = 15;
/// `mi_option_purge_decommits` — same index (5) in both vendored v2 and v3
/// headers. Set to 0: purges use `MADV_FREE` (lazy reclaim) instead of
/// `MADV_DONTNEED`, so freed pages stay mapped and their zero-refault reuse
/// is measurable until the kernel actually needs them under memory pressure.
/// Matrix arm `madv-free`: faults/block 5,598 vs 49,612 at
/// default, best `on_drain` p95 of all arms, RSS delta fully reclaimable.
#[cfg_attr(not(feature = "allocator-ctrl"), expect(dead_code))]
const MI_OPTION_PURGE_DECOMMITS: i32 = 5;
/// Also `mi_option_purge_decommits` (index 5) — same in v2/v3.
/// The vendored C source ships BOTH mimalloc v2 (2.3.02,
/// `MI_MALLOC_VERSION 20302`) and v3 sources; the dev `.so` observed live at
/// `mi_version() == 30302` (v3). Index `15` was verified byte-identical in both
/// vendored headers (`mi_option_purge_delay`), so allow the 2.x and 3.x
/// majors and refuse anything else (v1 or a future major reordering).
// The supported-version constants + the guard fn are reachable in BOTH
// feature configs (the feature build calls them from the apply paths; the
// no-feature build's TESTS exercise them directly) — so the pair is
// cfg-gated on `any(feature, test)` instead of a dead_code allow/expect
// whose fulfillment would differ per build config (allow_attributes is
// workspace-denied; an expect is unfulfilled under cfg(test)).
#[cfg(any(feature = "allocator-ctrl", test))]
const SUPPORTED_VERSION_MIN: i32 = 20_000;
#[cfg(any(feature = "allocator-ctrl", test))]
const SUPPORTED_VERSION_MAX: i32 = 40_000;

pub const MIN_DELAY_MS: i64 = 2_000;
pub const MAX_DELAY_MS: i64 = 600_000;
const WINDOW: usize = 30;
const MIN_BLOCKS: usize = 20;
// Hysteresis band: 10 percent of the applied value before re-applying,
// written as integer math in `observe` (`delta * 10 > applied`) to avoid a
// float cast under the cast-precision lint.
const MIN_INTERVAL_SECS: f64 = 2.0;
const MAX_INTERVAL_SECS: f64 = 60.0;

static AUTO_ENABLED: AtomicBool = AtomicBool::new(true);
static INIT_DONE: OnceLock<()> = OnceLock::new();

#[must_use]
pub fn clamp_delay_ms(ms: i64) -> i64 {
    ms.clamp(MIN_DELAY_MS, MAX_DELAY_MS)
}

/// Trailing-window simple mean x multiplier, clamped.
///
/// Readiness is the tracker's phase, not this function's: the warm-up
/// transition is the only caller, and only with `min_blocks` or more intervals
/// in the window.
#[must_use]
pub fn compute_purge_delay_ms(intervals: &[f64], mult: f64) -> i64 {
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
    clamp_delay_ms(ms)
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

/// the purge config is a typed schema section
/// (`allocator.mimalloc_*` / `DEGENBOT_MIMALLOC_*`); the loader owns the
/// environment read and the fail-closed parse. Fixed override > auto flag >
/// mult; clamping mirrors the schema-declared ranges.
#[must_use]
pub fn config_from_cfg(cfg: &::degenbot_config::schema::AllocatorConfig) -> PurgeConfig {
    PurgeConfig {
        fixed_ms: cfg.mimalloc_purge_delay_ms.map(clamp_delay_ms),
        auto: cfg.mimalloc_auto_purge,
        mult: cfg.mimalloc_purge_delay_mult.clamp(1.0, 20.0),
        // Default OFF (MADV_FREE): the T3 matrix arm measured -89 percent
        // refault churn at equal/better solve p95; `true` restores mimalloc's
        // aggressive decommit behavior.
        decommits: cfg.mimalloc_purge_decommits,
    }
}

/// Which measurement stage the cadence tracker is in.
///
/// The phase owns the clock anchor and the applied delay, so the two can never
/// disagree, and "nothing to re-tune from yet" is a phase rather than a `None`
/// return shared with the hysteresis verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CadencePhase {
    /// No header observed yet: the next arrival anchors the clock and measures
    /// nothing.
    AwaitingAnchor,
    /// Anchored, short of `min_blocks` valid intervals: no target exists.
    Warming { anchor_ms: u64 },
    /// A target has been applied: `applied_ms` is the live purge delay, held
    /// until a target leaves the hysteresis band.
    Tuning { anchor_ms: u64, applied_ms: i64 },
}

/// What one header arrival did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CadenceOutcome {
    /// The first header: the clock is anchored, no interval exists yet.
    Anchored,
    /// Too few valid intervals to compute a target: nothing is applied.
    Warming,
    /// The target cleared the hysteresis band and is now the applied delay.
    Tuned { delay_ms: i64 },
    /// The target stayed inside the band: `applied_ms` is still the live delay.
    Suppressed { target_ms: i64, applied_ms: i64 },
}

/// Cadence tracker: pure, testable, one instance per pump.
struct CadenceState {
    window: VecDeque<f64>,
    phase: CadencePhase,
    mult: f64,
    min_blocks: usize,
}

impl CadenceState {
    fn new(mult: f64, min_blocks: usize) -> Self {
        Self {
            window: VecDeque::with_capacity(WINDOW),
            phase: CadencePhase::AwaitingAnchor,
            mult,
            min_blocks,
        }
    }

    /// Record a header arrival at `now_ms` and answer what it did.
    ///
    /// An out-of-band interval (gap, reorg double-fire, sub-second
    /// double-header) never enters the window, but the arrival still re-anchors
    /// the clock so the NEXT interval measures from this header.
    fn observe(&mut self, now_ms: u64) -> CadenceOutcome {
        let (anchor_ms, applied_ms) = match self.phase {
            CadencePhase::AwaitingAnchor => {
                self.phase = CadencePhase::Warming { anchor_ms: now_ms };
                return CadenceOutcome::Anchored;
            }
            CadencePhase::Warming { anchor_ms } => (anchor_ms, None),
            CadencePhase::Tuning {
                anchor_ms,
                applied_ms,
            } => (anchor_ms, Some(applied_ms)),
        };
        self.record_interval(anchor_ms, now_ms);

        let Some(applied_ms) = applied_ms else {
            if self.window.len() < self.min_blocks {
                self.phase = CadencePhase::Warming { anchor_ms: now_ms };
                return CadenceOutcome::Warming;
            }
            // Warm-up is over. Nothing is applied yet, so there is no band to
            // suppress against: the first target tunes.
            let delay_ms = compute_purge_delay_ms(self.window.make_contiguous(), self.mult);
            self.phase = CadencePhase::Tuning {
                anchor_ms: now_ms,
                applied_ms: delay_ms,
            };
            return CadenceOutcome::Tuned { delay_ms };
        };

        let target_ms = compute_purge_delay_ms(self.window.make_contiguous(), self.mult);
        if within_band(applied_ms, target_ms) {
            self.phase = CadencePhase::Tuning {
                anchor_ms: now_ms,
                applied_ms,
            };
            return CadenceOutcome::Suppressed {
                target_ms,
                applied_ms,
            };
        }
        self.phase = CadencePhase::Tuning {
            anchor_ms: now_ms,
            applied_ms: target_ms,
        };
        CadenceOutcome::Tuned {
            delay_ms: target_ms,
        }
    }

    /// Push the interval when it is a plausible block cadence.
    fn record_interval(&mut self, anchor_ms: u64, now_ms: u64) {
        let dt_secs =
            f64::from(u32::try_from(now_ms.saturating_sub(anchor_ms)).unwrap_or(u32::MAX)) / 1000.0;
        if (MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS).contains(&dt_secs) {
            self.window.push_back(dt_secs);
            while self.window.len() > WINDOW {
                self.window.pop_front();
            }
        }
    }
}

/// The 10 percent hysteresis band in integer math (no float casts): a target
/// this close to `applied_ms` leaves the applied delay alone, so the mimalloc
/// option is not churned every block.
fn within_band(applied_ms: i64, target_ms: i64) -> bool {
    (target_ms - applied_ms).abs() * 10 <= applied_ms
}

#[cfg(any(feature = "allocator-ctrl", test))]
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
    op_info!(
        domain = pump,
        decommits,
        "mimalloc purge decommits applied (false = MADV_FREE)"
    );
}

#[cfg(not(feature = "allocator-ctrl"))]
fn apply_decommits(decommits: bool) {
    let _ = decommits; // inert without the feature; same one-time contract as apply_delay_ms
}

#[cfg(feature = "allocator-ctrl")]
fn apply_delay_ms(delay_ms: i64) {
    let ok = VERSION_OK.get_or_init(|| {
        // SAFETY: pure function over the vendored C statics; no allocation.
        let v = unsafe { libmimalloc_sys::mi_version() };
        let ok = supported_version(v);
        if !ok {
            op_warn!(
                domain = pump,
                version = v,
                "unsupported mimalloc major - purge-delay control disabled"
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
    op_info!(
        domain = pump,
        delay_ms,
        "mimalloc purge delay applied from block cadence"
    );
}

#[cfg(not(feature = "allocator-ctrl"))]
fn apply_delay_ms(delay_ms: i64) {
    static WARNED: OnceLock<()> = OnceLock::new();
    if WARNED.set(()).is_ok() {
        op_warn!(
            domain = pump,
            delay_ms,
            "computed purge delay but the allocator-ctrl \
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
    // typed schema section; the env read belongs to the loader.
    let cfg = config_from_cfg(&degenbot_substrate::stance::config().allocator);
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
        op_info!(
            domain = pump,
            mult = cfg.mult,
            min_blocks = MIN_BLOCKS,
            window = WINDOW,
            "block-cadence purge-delay discovery armed"
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
        let cfg = config_from_cfg(&degenbot_substrate::stance::config().allocator);
        CadenceState::new(cfg.mult, MIN_BLOCKS)
    });
    match state.observe(now_ms) {
        CadenceOutcome::Anchored | CadenceOutcome::Warming => {}
        CadenceOutcome::Tuned { delay_ms } => apply_delay_ms(delay_ms),
        // Debug, not info: once tuned this fires on every header.
        CadenceOutcome::Suppressed {
            target_ms,
            applied_ms,
        } => op_debug!(
            domain = pump,
            target_ms,
            applied_ms,
            "purge-delay target inside the hysteresis band"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT_MULT: f64 = 2.0;

    fn state() -> CadenceState {
        CadenceState::new(DEFAULT_MULT, MIN_BLOCKS)
    }

    /// Observe a clean 12s cadence through warm-up; returns the last header's
    /// timestamp. The final observation tunes to `24_000`ms.
    fn fill_warm_up(s: &mut CadenceState) -> u64 {
        let min_blocks = MIN_BLOCKS as u64;
        assert_eq!(s.observe(0), CadenceOutcome::Anchored);
        for i in 1..=min_blocks {
            let _ = s.observe(i * 12_000);
        }
        min_blocks * 12_000
    }

    #[test]
    fn first_header_anchors_and_applies_nothing() {
        let mut s = state();
        assert_eq!(s.observe(1_000), CadenceOutcome::Anchored);
        assert_eq!(s.phase, CadencePhase::Warming { anchor_ms: 1_000 });
        assert!(
            s.window.is_empty(),
            "no interval exists before a second header"
        );
    }

    #[test]
    fn warm_up_outcome_is_distinct_from_suppression() {
        let mut s = state();
        assert_eq!(s.observe(0), CadenceOutcome::Anchored);
        for i in 1..MIN_BLOCKS as u64 {
            assert_eq!(s.observe(i * 12_000), CadenceOutcome::Warming);
        }
        assert_eq!(s.window.len(), MIN_BLOCKS - 1);
        assert!(matches!(s.phase, CadencePhase::Warming { .. }));
    }

    #[test]
    fn first_apply_tunes_without_a_band_check() {
        let mut s = state();
        let last = fill_warm_up(&mut s);
        assert_eq!(
            s.phase,
            CadencePhase::Tuning {
                anchor_ms: last,
                applied_ms: 24_000,
            }
        );
        assert_eq!(s.window.len(), MIN_BLOCKS);
    }

    #[test]
    fn compute_scales_mean_by_multiplier() {
        let ivs = [13.16; MIN_BLOCKS];
        assert_eq!(compute_purge_delay_ms(&ivs, DEFAULT_MULT), 26_320);
    }

    #[test]
    fn compute_clamps_to_bounds() {
        let fast = [1.0; MIN_BLOCKS];
        assert_eq!(compute_purge_delay_ms(&fast, DEFAULT_MULT), MIN_DELAY_MS);
        let slow = [900.0; MIN_BLOCKS];
        assert_eq!(compute_purge_delay_ms(&slow, DEFAULT_MULT), MAX_DELAY_MS);
    }

    #[test]
    fn invalid_interval_reanchors_without_applying() {
        let mut s = state();
        assert_eq!(s.observe(0), CadenceOutcome::Anchored);
        for i in 1..MIN_BLOCKS as u64 {
            let _ = s.observe(i * 12_000);
        }
        let last = (MIN_BLOCKS as u64 - 1) * 12_000;
        // A 400s gap lies outside the cadence band: no sample, no apply.
        assert_eq!(s.observe(last + 400_000), CadenceOutcome::Warming);
        assert_eq!(
            s.phase,
            CadencePhase::Warming {
                anchor_ms: last + 400_000
            }
        );
        assert_eq!(s.window.len(), MIN_BLOCKS - 1, "a gap is not a sample");
        // The next 12s interval measures from the re-anchored header; had the
        // clock stayed on `last`, this arrival would read as a 412s gap.
        assert_eq!(
            s.observe(last + 400_000 + 12_000),
            CadenceOutcome::Tuned { delay_ms: 24_000 }
        );
    }

    #[test]
    fn out_of_band_interval_while_tuning_holds_the_applied_delay() {
        let mut s = state();
        let mut now = fill_warm_up(&mut s);
        now += 300; // sub-second double-fire
        assert_eq!(
            s.observe(now),
            CadenceOutcome::Suppressed {
                target_ms: 24_000,
                applied_ms: 24_000,
            }
        );
        assert_eq!(s.window.len(), MIN_BLOCKS, "a double-fire is not a sample");
        assert!(matches!(s.phase, CadencePhase::Tuning { anchor_ms, .. } if anchor_ms == now));
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
    fn suppressed_outcome_carries_target_and_applied() {
        let mut s = state();
        let mut now = fill_warm_up(&mut s);
        // One 13s sample against 20 x 12s: target 24_095ms, inside the band.
        now += 13_000;
        assert_eq!(
            s.observe(now),
            CadenceOutcome::Suppressed {
                target_ms: 24_095,
                applied_ms: 24_000,
            }
        );
        assert!(
            matches!(s.phase, CadencePhase::Tuning { applied_ms, .. } if applied_ms == 24_000),
            "suppression keeps the applied delay"
        );
    }

    #[test]
    fn hysteresis_suppresses_drift_inside_the_band() {
        let mut s = state();
        let mut now = fill_warm_up(&mut s);
        for _ in 0..WINDOW {
            now += 13_000;
            assert!(
                matches!(s.observe(now), CadenceOutcome::Suppressed { .. }),
                "13s under a 24_000ms band must never re-apply"
            );
        }
        assert!(matches!(s.phase, CadencePhase::Tuning { applied_ms, .. } if applied_ms == 24_000));
    }

    #[test]
    fn hysteresis_retunes_drift_outside_the_band() {
        let mut s = state();
        let mut now = fill_warm_up(&mut s);
        // Drain the window to all-40s samples: the target migrates stepwise and
        // freezes once a step falls inside the band, so the applied value must
        // have retuned past the warm-up 24_000ms and end within the band of the
        // pure-40s target (40s x 2.0).
        let mut applied = 24_000;
        for _ in 0..WINDOW {
            now += 40_000;
            if let CadenceOutcome::Tuned { delay_ms } = s.observe(now) {
                applied = delay_ms;
            }
        }
        assert!(applied > 24_000, "a 40s cadence must retune past 24_000");
        let final_target = 80_000;
        assert!(
            (final_target - applied).abs() * 10 <= applied,
            "applied {applied} must sit within 10% of target {final_target}"
        );
        assert!(
            matches!(s.phase, CadencePhase::Tuning { applied_ms, .. } if applied_ms == applied)
        );
    }

    #[test]
    fn config_typed_section_maps_onto_purge_config() {
        // env parsing moved to the degenbot-config loader (its own
        // tests own the string contract). This asserts the typed mapping.
        let cfg = config_from_cfg(&::degenbot_config::schema::AllocatorConfig {
            mimalloc_purge_delay_ms: Some(45_000),
            mimalloc_auto_purge: false,
            mimalloc_purge_delay_mult: 3.0,
            mimalloc_purge_decommits: true,
        });
        assert_eq!(cfg.fixed_ms, Some(45_000));
        assert!(!cfg.auto);
        assert!((cfg.mult - 3.0).abs() < f64::EPSILON);
        assert!(cfg.decommits, "=1/true must restore decommit purges");

        let cfg = config_from_cfg(&::degenbot_config::schema::AllocatorConfig::default());
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
