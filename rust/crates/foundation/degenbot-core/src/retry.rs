//! Canonical retry-with-backoff policy for the workspace.
//!
//! One Rust-owned policy shape — `max_attempts`, `base_delay`, `max_delay`,
//! `jitter` — consumed by every exponential-backoff wait loop in the tree:
//!
//! - `degenbot-rpc`'s per-call RPC retry loop and the WS/IPC connect +
//!   subscription-watchdog reconnects,
//! - `degenbot-db`'s WAL-switch retry on a contended fresh open,
//! - the engine's verification retry (`degenbot-bot`'s
//!   `bot_core::verification_retry` and the `verify_lifecycle` that defaults
//!   to [`RetryPolicy::verification_default`]), and
//! - the settlement-arbitrage example's verification retry.
//!
//! The Python driver mirrors this shape across the FFI seam
//! (`degenbot-python`'s `config::RetryPolicy`), so the policy shape has one home.
//!
//! The retry *classification* (which errors are retryable) is deliberately
//! NOT unified: each call site keeps its own predicate (`ProviderError::
//! is_retryable`, `VerifyErrorKind::Rpc`, `VerificationRpcError`). Only the
//! policy *machinery* — attempts, exponential growth, cap, jitter bound — is
//! shared here.
//!
//! The wait for retry `n` (1-indexed) is
//! `min(max_delay, base_delay * 2^(n-1))` plus a site-owned uniform jitter
//! draw in `[0, jitter]`. This type carries the jitter *bound*, not an RNG, so
//! each site's existing random source (rand, or the example's splitmix) is
//! unchanged.

use std::time::Duration;

/// Bounded exponential-backoff policy.
///
/// Fields are public to keep the type a plain value object at construction
/// sites (a driver shell's own policy value carries this shape across its
/// seam): a literal declares a policy this crate's owners chose, and a
/// consumer reads its fields without a further check.
///
/// The four knobs are bounded. A value that arrives from outside the process —
/// an operator file, the environment layer, a driver shell's constructor —
/// enters through [`RetryPolicy::try_untrusted`], which refuses an
/// out-of-range knob instead of returning a policy the retry dance cannot
/// honour. That door is the boundary, so validity is a property of the value
/// rather than a step each consumer has to remember.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RetryPolicy {
    /// Total attempts (>= 1); the first attempt is not a retry.
    pub max_attempts: u32,
    /// Initial backoff delay, in seconds.
    pub base_delay: f64,
    /// Backoff cap, in seconds.
    pub max_delay: f64,
    /// Upper bound of the uniform jitter added to each backoff, in seconds.
    pub jitter: f64,
}

impl RetryPolicy {
    /// Build a policy from millisecond durations (the `degenbot-rpc` call
    /// sites' native units).
    #[must_use]
    pub fn from_millis(
        max_attempts: u32,
        base_delay_ms: u64,
        max_delay_ms: u64,
        jitter_ms: u64,
    ) -> Self {
        Self {
            max_attempts,
            base_delay: Duration::from_millis(base_delay_ms).as_secs_f64(),
            max_delay: Duration::from_millis(max_delay_ms).as_secs_f64(),
            jitter: Duration::from_millis(jitter_ms).as_secs_f64(),
        }
    }

    /// Build a policy from four knobs supplied from outside the process,
    /// refusing a combination the retry dance cannot honour.
    ///
    /// The refusal text names the knob and the value that failed; it is
    /// surfaced verbatim to the operator (the FFI `RetryPolicy` constructor's
    /// `ValueError`, the settlement example's config build), so its wording is
    /// part of the boundary's contract.
    ///
    /// # Errors
    ///
    /// Returns a message when a knob is out of range.
    pub fn try_untrusted(
        max_attempts: u32,
        base_delay: f64,
        max_delay: f64,
        jitter: f64,
    ) -> Result<Self, String> {
        let policy = Self {
            max_attempts,
            base_delay,
            max_delay,
            jitter,
        };
        policy.check_bounds()?;
        Ok(policy)
    }

    /// The verification-retry defaults — the single declaration site the
    /// Python driver reads through `degenbot._ffi` and the example's env
    /// parse falls back to.
    ///
    /// Sane for a local/edge node recovering from a transient transport blip:
    /// up to 4 attempts, ~0.5s -> ~1s -> ~2s base delays (before jitter),
    /// capped at 4s.
    #[must_use]
    pub const fn verification_default() -> Self {
        Self {
            max_attempts: 4,
            base_delay: 0.5,
            max_delay: 4.0,
            jitter: 0.5,
        }
    }

    /// The field bounds [`RetryPolicy::try_untrusted`] enforces, as the
    /// refusal it returns.
    ///
    /// # Errors
    ///
    /// Returns a message when a knob is out of range.
    fn check_bounds(&self) -> Result<(), String> {
        if self.max_attempts < 1 {
            return Err(format!(
                "max_attempts must be >= 1, got {}",
                self.max_attempts
            ));
        }
        if self.base_delay < 0.0 {
            return Err(format!("base_delay must be >= 0, got {}", self.base_delay));
        }
        if self.max_delay < 0.0 {
            return Err(format!("max_delay must be >= 0, got {}", self.max_delay));
        }
        if !(0.0..=1.0).contains(&self.jitter) {
            return Err(format!("jitter must be in [0, 1], got {}", self.jitter));
        }
        if self.base_delay > self.max_delay && self.max_delay > 0.0 {
            return Err(format!(
                "base_delay ({}) must be <= max_delay ({})",
                self.base_delay, self.max_delay
            ));
        }
        Ok(())
    }

    /// The capped exponential backoff for 1-indexed retry `attempt`, in
    /// seconds (no jitter): `min(max_delay, base_delay * 2^(attempt-1))`.
    #[must_use]
    pub fn capped_backoff_secs(&self, attempt: u32) -> f64 {
        let exponent = i32::try_from(attempt.saturating_sub(1)).unwrap_or(i32::MAX);
        (self.base_delay * 2f64.powi(exponent)).min(self.max_delay)
    }

    /// [`RetryPolicy::capped_backoff_secs`] as a `Duration` (the rpc/example
    /// wait loops' sleep unit).
    #[must_use]
    pub fn capped_backoff(&self, attempt: u32) -> Duration {
        Duration::from_secs_f64(self.capped_backoff_secs(attempt).max(0.0))
    }

    /// The total backoff this policy schedules across `retries` retries
    /// (attempts `1..=retries`), before jitter.
    ///
    /// This is the number a caller compares against its own patience. The
    /// `degenbot-rpc` curve — `max_attempts = 11` over a 100ms base doubling to
    /// a 30s cap — totals 81.1s of backoff, so a single call can outlive a
    /// caller whose deadline is shorter than that. That arithmetic is why the
    /// rpc loop carries an explicit retry budget alongside the schedule.
    #[must_use]
    pub fn cumulative_backoff(&self, retries: u32) -> Duration {
        (1..=retries).map(|retry| self.capped_backoff(retry)).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_default_is_valid() {
        let default = RetryPolicy::verification_default();
        assert_eq!(
            RetryPolicy::try_untrusted(
                default.max_attempts,
                default.base_delay,
                default.max_delay,
                default.jitter
            ),
            Ok(default)
        );
    }

    /// The boundary refuses an out-of-range knob with the message the FFI
    /// constructor's `ValueError` and the settlement example's config build
    /// surface verbatim, so the wording is pinned byte for byte here.
    #[test]
    fn try_untrusted_rejects_zero_attempts_and_wide_jitter() {
        assert_eq!(
            RetryPolicy::try_untrusted(0, 0.5, 4.0, 0.5),
            Err("max_attempts must be >= 1, got 0".to_string())
        );
        assert_eq!(
            RetryPolicy::try_untrusted(4, 0.5, 4.0, 1.5),
            Err("jitter must be in [0, 1], got 1.5".to_string())
        );
        assert_eq!(
            RetryPolicy::try_untrusted(4, -0.1, 4.0, 0.5),
            Err("base_delay must be >= 0, got -0.1".to_string())
        );
        assert_eq!(
            RetryPolicy::try_untrusted(4, 0.5, -1.0, 0.5),
            Err("max_delay must be >= 0, got -1".to_string())
        );
        assert_eq!(
            RetryPolicy::try_untrusted(4, 5.0, 1.0, 0.5),
            Err("base_delay (5) must be <= max_delay (1)".to_string())
        );
    }

    #[test]
    fn backoff_is_exponential_and_capped() {
        let policy = RetryPolicy {
            max_attempts: 6,
            base_delay: 0.5,
            max_delay: 4.0,
            jitter: 0.0,
        };
        assert!((policy.capped_backoff_secs(1) - 0.5).abs() < f64::EPSILON);
        assert!((policy.capped_backoff_secs(2) - 1.0).abs() < f64::EPSILON);
        assert!((policy.capped_backoff_secs(3) - 2.0).abs() < f64::EPSILON);
        assert!((policy.capped_backoff_secs(4) - 4.0).abs() < f64::EPSILON);
        assert!((policy.capped_backoff_secs(9) - 4.0).abs() < f64::EPSILON);
    }

    /// The rpc loop's default ceiling: eleven attempts over the crate's
    /// 100ms-base/30s-cap curve sleep 81.1s in backoff alone. Pinned as
    /// arithmetic so a curve change that quietly lengthens the worst case has
    /// to face this number.
    #[test]
    fn eleven_attempt_curve_costs_81_seconds_of_backoff() {
        let policy = RetryPolicy::from_millis(11, 100, 30_000, 100);
        assert_eq!(
            policy.cumulative_backoff(10),
            Duration::from_millis(81_100),
            "10 retries: 0.1+0.2+0.4+0.8+1.6+3.2+6.4+12.8+25.6+30.0s"
        );
        assert_eq!(policy.cumulative_backoff(0), Duration::ZERO);
    }

    #[test]
    fn from_millis_preserves_the_ms_curve() {
        let policy = RetryPolicy::from_millis(10, 100, 30_000, 100);
        assert_eq!(policy.capped_backoff(1), Duration::from_millis(100));
        assert_eq!(policy.capped_backoff(2), Duration::from_millis(200));
        assert_eq!(policy.capped_backoff(9), Duration::from_millis(25_600));
        assert_eq!(policy.capped_backoff(10), Duration::from_secs(30));
        assert_eq!(policy.capped_backoff(u32::MAX), Duration::from_secs(30));
    }
}
