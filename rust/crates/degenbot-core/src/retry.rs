//! Canonical retry-with-backoff policy for the workspace .
//!
//! One Rust-owned policy shape — `max_attempts`, `base_delay`, `max_delay`,
//! `jitter` — consumed by every exponential-backoff wait loop in the tree:
//!
//! - `degenbot-rpc`'s per-call RPC retry loop and the WS/IPC connect +
//!   subscription-watchdog reconnects, and
//! - the settlement-arbitrage example's verification retry.
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
/// sites (the Python `VerificationRetryPolicy` dataclass mirrors this shape);
/// call [`RetryPolicy::validate`] before consuming a policy built from
/// untrusted input.
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

    /// Validate the field bounds (fail fast). The message text matches the
    /// Python `VerificationRetryPolicy.__post_init__` `ValueError` text so the
    /// driver shell and the Rust example surface the same wording.
    ///
    /// # Errors
    ///
    /// Returns a message when a knob is out of range.
    pub fn validate(&self) -> Result<(), String> {
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
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests assert on known-valid inputs")]
mod tests {
    use super::*;

    #[test]
    fn verification_default_is_valid() {
        assert!(RetryPolicy::verification_default().validate().is_ok());
    }

    #[test]
    fn validate_rejects_out_of_range_knobs() {
        let zero = RetryPolicy {
            max_attempts: 0,
            ..RetryPolicy::verification_default()
        };
        assert!(zero.validate().unwrap_err().contains("max_attempts"));

        let bad_jitter = RetryPolicy {
            jitter: 1.5,
            ..RetryPolicy::verification_default()
        };
        assert!(bad_jitter.validate().unwrap_err().contains("jitter"));

        let negative = RetryPolicy {
            base_delay: -0.1,
            ..RetryPolicy::verification_default()
        };
        assert!(negative.validate().unwrap_err().contains("base_delay"));

        let inverted = RetryPolicy {
            base_delay: 5.0,
            max_delay: 1.0,
            ..RetryPolicy::verification_default()
        };
        assert!(inverted.validate().unwrap_err().contains("base_delay"));
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
