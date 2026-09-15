//! Registration-bound parity : the
//! `DEGENBOT_MAX_PATHS` cap and the time-throttled registration-progress
//! summary, mirroring `src/degenbot/runner/build_paths.py`.
//!
//! Python semantics mirrored here:
//! - `MAX_REGISTERED_PATHS = int(os.environ.get("DEGENBOT_MAX_PATHS", "100000"))`,
//!   applied via `engine.set_path_cap(MAX_REGISTERED_PATHS or None)` BEFORE
//!   discovery runs — so `0` (or unset) means uncapped.
//! - `RegistrationPipeline._PROGRESS_INTERVAL_S = float(os.environ.get(
//!   "DEGENBOT_REG_PROGRESS_SECS", "30"))` gates a summary that fires even when
//!   `path_count` never crosses a 1000-boundary, so a discovery-heavy
//!   skip-fest stays visible mid-crawl.
//!
//! The cadence is a pure function of an injected `Instant` clock so the
//! throttle is unit-testable without sleeping or RPC.

use std::time::{Duration, Instant};

use crate::pipeline::PipelineReport;

/// The `MAX_REGISTERED_PATHS` default (`build_paths.py`).
pub const DEFAULT_MAX_PATHS: usize = 100_000;

/// The `_PROGRESS_INTERVAL_S` default (`build_paths.py`).
pub const DEFAULT_PROGRESS_SECS: f64 = 30.0;

/// Parse `DEGENBOT_MAX_PATHS` with Python semantics: unset/empty → the
/// 100 000 default; `0` → uncapped (`None`); a positive integer → the cap.
///
/// # Errors
///
/// Refuses a negative or non-integer value loudly (Python's `int()` raises).
pub fn parse_max_paths(raw: Option<&str>) -> Result<Option<usize>, String> {
    let raw = match raw {
        None | Some("") => return Ok(Some(DEFAULT_MAX_PATHS)),
        Some(value) => value,
    };
    let parsed = raw
        .parse::<i128>()
        .map_err(|e| format!("DEGENBOT_MAX_PATHS must be an integer, got {raw:?}: {e}"))?;
    if parsed < 0 {
        return Err(format!("DEGENBOT_MAX_PATHS must be >= 0, got {parsed}"));
    }
    let cap = usize::try_from(parsed)
        .map_err(|_| format!("DEGENBOT_MAX_PATHS exceeds the platform usize: {parsed}"))?;
    Ok(if cap == 0 { None } else { Some(cap) })
}

/// Parse `DEGENBOT_REG_PROGRESS_SECS` (default 30.0). A value of `0` keeps
/// Python's always-due cadence; a negative or non-finite value is refused.
///
/// # Errors
///
/// Refuses a non-numeric, negative, or non-finite value.
pub fn parse_progress_secs(raw: Option<&str>) -> Result<Duration, String> {
    let secs = match raw {
        None | Some("") => DEFAULT_PROGRESS_SECS,
        Some(value) => value.parse::<f64>().map_err(|e| {
            format!("DEGENBOT_REG_PROGRESS_SECS must be a number, got {value:?}: {e}")
        })?,
    };
    if !secs.is_finite() || secs < 0.0 {
        return Err(format!(
            "DEGENBOT_REG_PROGRESS_SECS must be a finite value >= 0, got {secs}"
        ));
    }
    Ok(Duration::from_secs_f64(secs))
}

/// The time-throttled cadence for the registration-progress summary — the
/// Rust twin of `self._last_progress_ts` / `_PROGRESS_INTERVAL_S`.
///
/// The clock is a parameter to [`ProgressCadence::due`] so the throttle is
/// testable with two synthetic `Instant`s instead of a sleep.
#[derive(Clone, Debug)]
pub struct ProgressCadence {
    interval: Duration,
    last_emit: Option<Instant>,
}

impl ProgressCadence {
    /// A cadence that fires no more than once per `interval`.
    #[must_use]
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_emit: None,
        }
    }

    /// Whether a progress line is due at `now`. A due call records `now` and
    /// returns `true`; a throttled call returns `false` and leaves the clock
    /// untouched. The first call is always due (Python's `_last_progress_ts`
    /// starts at 0.0 while `monotonic()` is already past one interval).
    pub fn due(&mut self, now: Instant) -> bool {
        match self.last_emit {
            None => {
                self.last_emit = Some(now);
                true
            }
            Some(last) => {
                if now.saturating_duration_since(last) >= self.interval {
                    self.last_emit = Some(now);
                    true
                } else {
                    false
                }
            }
        }
    }
}

/// The reason-tagged skip breakdown, most common first (Python's
/// `Counter.most_common(8)`), as `reason=count` pairs.
#[must_use]
pub fn skip_breakdown(report: &PipelineReport) -> String {
    let mut pairs: Vec<(&str, usize)> = report
        .skip_reasons
        .iter()
        .map(|(reason, count)| (reason.as_str(), *count))
        .collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    pairs
        .iter()
        .take(8)
        .map(|(reason, count)| format!("{reason}={count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The `[build_paths] Progress` summary line, mirroring the Python field
/// order (`path_count` / `skip_count` / `token_filter` / `engine_reject` /
/// `register_fail` / `cap_skip` / `dup`) plus the Rust-side v4-hop and capped
/// witnesses.
#[must_use]
pub fn progress_line(report: &PipelineReport) -> String {
    format!(
        "[build_paths] Progress: {} paths registered, {} skipped, {} token-filtered, \
         {} engine-rejected, {} register-fail, {} cap-skipped, {} duplicates, \
         {} v4-hops, capped={} {{skip_reasons: {}}}",
        report.path_count,
        report.skip_count,
        report.token_filter_count,
        report.engine_reject_count,
        report.register_fail_count,
        report.cap_skip_count,
        report.dup_count,
        report.v4_pool_count,
        report.capped,
        skip_breakdown(report),
    )
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "tests assert on known-valid parsed values"
)]
mod tests {
    use super::*;
    use crate::ledger::RegistrationOutcome;

    #[test]
    fn max_paths_unset_and_empty_use_the_default() {
        assert_eq!(parse_max_paths(None).unwrap(), Some(DEFAULT_MAX_PATHS));
        assert_eq!(parse_max_paths(Some("")).unwrap(), Some(DEFAULT_MAX_PATHS));
    }

    #[test]
    fn max_paths_zero_is_uncapped() {
        assert_eq!(parse_max_paths(Some("0")).unwrap(), None);
    }

    #[test]
    fn max_paths_positive_is_the_cap() {
        assert_eq!(parse_max_paths(Some("42")).unwrap(), Some(42));
    }

    #[test]
    fn max_paths_rejects_negative_and_junk() {
        assert!(parse_max_paths(Some("-1")).is_err());
        assert!(parse_max_paths(Some("nope")).is_err());
    }

    #[test]
    fn progress_secs_defaults_and_zero_mirror_python() {
        assert_eq!(
            parse_progress_secs(None).unwrap(),
            Duration::from_secs_f64(DEFAULT_PROGRESS_SECS)
        );
        assert_eq!(
            parse_progress_secs(Some("")).unwrap(),
            Duration::from_secs_f64(DEFAULT_PROGRESS_SECS)
        );
        assert_eq!(parse_progress_secs(Some("0")).unwrap(), Duration::ZERO);
        assert!(parse_progress_secs(Some("-3")).is_err());
        assert!(parse_progress_secs(Some("nan")).is_err());
        assert!(parse_progress_secs(Some("junk")).is_err());
    }

    #[test]
    fn cadence_is_time_throttled_with_an_injected_clock() {
        let mut cadence = ProgressCadence::new(Duration::from_secs(30));
        let t0 = Instant::now();
        assert!(cadence.due(t0), "the first call is always due");
        assert!(!cadence.due(t0 + Duration::from_secs(29)));
        assert!(!cadence.due(t0 + Duration::from_millis(29_999)));
        assert!(
            cadence.due(t0 + Duration::from_secs(30)),
            "one interval past the last emit is due"
        );
        assert!(!cadence.due(t0 + Duration::from_secs(59)));
    }

    #[test]
    fn progress_line_reports_the_cap_skip_and_reason_breakdown() {
        let mut report = PipelineReport {
            path_count: 7,
            skip_count: 1,
            cap_skip_count: 1,
            capped: true,
            ..PipelineReport::default()
        };
        report
            .skip_reasons
            .insert(RegistrationOutcome::PathCap.as_str().to_string(), 1);
        let line = progress_line(&report);
        assert!(line.contains("7 paths registered"), "{line}");
        assert!(line.contains("capped=true"), "{line}");
        assert!(
            line.contains(&format!("{}=", RegistrationOutcome::PathCap.as_str())),
            "{line}"
        );
    }
}
