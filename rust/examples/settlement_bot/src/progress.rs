//! Registration-bound parity : the
//! `DEGENBOT_MAX_PATHS` cap and the time-throttled registration-progress
//! summary, mirroring `src/degenbot/runner/build_paths.py`.
//!
//! The values arrive through the schema, not a private parse: the loader
//! resolves `pathfinding.max_registered_paths` (declared default 100 000; `0`
//! means uncapped) and `pathfinding.reg_progress_secs` (declared default 30.0;
//! `0` emits on every update). The declared default IS the unset behaviour, so
//! an operator who exports nothing is capped at 100 000 — the same as Python's
//! `int(os.environ.get("DEGENBOT_MAX_PATHS", "100000")) or None`.
//!
//! `pathfinding.reg_progress_secs` still gates a summary that fires even when
//! `path_count` never crosses a 1000-boundary, so a discovery-heavy
//! skip-fest stays visible mid-crawl.
//!
//! The cadence is a pure function of an injected `Instant` clock so the
//! throttle is unit-testable without sleeping or RPC.

use std::time::{Duration, Instant};

use crate::pipeline::PipelineReport;

/// The engine's registered-path cap from the schema's
/// `pathfinding.max_registered_paths`. `0` is the operator's uncapped
/// sentinel (`set_path_cap(None)`); any other value is a real ceiling.
///
/// The schema types the key as a plain `usize`, so `0` and "unset" would
/// otherwise collapse; the declared default (100 000) is what an unset key
/// resolves to, so unset means capped. Pinned by
/// `unset_max_registered_paths_is_the_declared_default_cap_not_uncapped`.
#[must_use]
pub fn path_cap(max_registered_paths: usize) -> Option<usize> {
    (max_registered_paths != 0).then_some(max_registered_paths)
}

/// The registration-progress reporting cadence from the schema's
/// `pathfinding.reg_progress_secs`. `0` keeps Python's always-due cadence; a
/// negative or non-finite value is refused here because the schema type is an
/// unconstrained `f64`.
///
/// # Errors
///
/// Refuses a negative or non-finite value.
pub fn progress_interval(secs: f64) -> Result<Duration, String> {
    if !secs.is_finite() || secs < 0.0 {
        return Err(format!(
            "pathfinding.reg_progress_secs must be a finite value >= 0, got {secs}"
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
    clippy::float_cmp,
    reason = "tests assert on known-valid parsed values and exact declared literals"
)]
mod tests {
    use super::*;
    use degenbot::bot::bot_core::registration_ledger::RegistrationOutcome;

    #[test]
    fn the_env_layer_resolves_the_six_schema_owned_keys() {
        // The example is a `cargo add degenbot` consumer, so these six names
        // are the schema's contract: the two `DEGENBOT_`-prefixed pathfinding
        // keys plus the four unprefixed verification-retry names. The loader
        // owns the spelling; the example never re-parses them from `std::env`.
        let env = degenbot::config::MapEnv::new(std::collections::BTreeMap::from([
            ("DEGENBOT_MAX_PATHS".to_string(), "0".to_string()),
            ("DEGENBOT_REG_PROGRESS_SECS".to_string(), "5".to_string()),
            (
                "VERIFICATION_RETRY_MAX_ATTEMPTS".to_string(),
                "7".to_string(),
            ),
            (
                "VERIFICATION_RETRY_BASE_DELAY".to_string(),
                "0.25".to_string(),
            ),
            (
                "VERIFICATION_RETRY_MAX_DELAY".to_string(),
                "3.0".to_string(),
            ),
            ("VERIFICATION_RETRY_JITTER".to_string(), "0.75".to_string()),
        ]));
        let loaded = degenbot::config::BotConfigLoader::new()
            .with_env(Box::new(env))
            .load()
            .unwrap();
        assert_eq!(
            path_cap(loaded.config.pathfinding.max_registered_paths),
            None,
            "DEGENBOT_MAX_PATHS=0 is the uncapped sentinel"
        );
        assert_eq!(
            progress_interval(loaded.config.pathfinding.reg_progress_secs).unwrap(),
            Duration::from_secs(5)
        );
        assert_eq!(loaded.config.verify.verify_retry_max_attempts, 7);
        assert_eq!(loaded.config.verify.verify_retry_base_delay, 0.25);
        assert_eq!(loaded.config.verify.verify_retry_max_delay, 3.0);
        assert_eq!(loaded.config.verify.verify_retry_jitter, 0.75);
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

    #[test]
    fn path_cap_reads_zero_as_uncapped_and_any_other_value_as_the_cap() {
        assert_eq!(path_cap(0), None, "an explicit 0 is the uncapped sentinel");
        assert_eq!(path_cap(1), Some(1));
        assert_eq!(path_cap(100_000), Some(100_000));
    }

    #[test]
    fn unset_max_registered_paths_is_the_declared_default_cap_not_uncapped() {
        // The loader fills the schema's declared default when the key is
        // absent, so an operator who sets nothing gets 100_000. Only an
        // explicit 0 is uncapped. Python:
        // `set_path_cap(int(os.environ.get("DEGENBOT_MAX_PATHS", "100000")) or None)`.
        let defaulted = degenbot::config::BotConfig::default();
        let declared = defaulted.pathfinding.max_registered_paths;
        assert_eq!(declared, 100_000, "the schema's declared default moved");
        assert_eq!(path_cap(declared), Some(100_000));
        assert_ne!(path_cap(declared), None, "unset must not read as uncapped");
    }

    #[test]
    fn the_example_resolves_the_pathfinding_keys_through_the_loaded_cascade() {
        // The example is a `cargo add degenbot` consumer: the values must
        // arrive through the schema cascade, not a private env parse.
        let loaded = degenbot::config::BotConfigLoader::new()
            .without_env()
            .with_cli("pathfinding.max_registered_paths", "0")
            .with_cli("pathfinding.reg_progress_secs", "5")
            .load()
            .unwrap();
        assert_eq!(
            path_cap(loaded.config.pathfinding.max_registered_paths),
            None
        );
        assert_eq!(
            progress_interval(loaded.config.pathfinding.reg_progress_secs).unwrap(),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn progress_interval_defaults_and_zero_mirror_python() {
        let defaulted = degenbot::config::BotConfig::default();
        assert_eq!(defaulted.pathfinding.reg_progress_secs, 30.0);
        assert_eq!(
            progress_interval(defaulted.pathfinding.reg_progress_secs).unwrap(),
            Duration::from_secs(30)
        );
        assert_eq!(progress_interval(0.0).unwrap(), Duration::ZERO);
    }

    #[test]
    fn progress_interval_refuses_negative_and_non_finite() {
        assert!(progress_interval(-3.0).is_err());
        assert!(progress_interval(f64::NAN).is_err());
        assert!(progress_interval(f64::INFINITY).is_err());
    }
}
