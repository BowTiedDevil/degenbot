//! Bounded retry-with-backoff for transient verification RPC failures —
//! the Rust twin of `src/degenbot/arbitrage/verification_retry.py`
//! (parity-ledger row 9, ergo `XFEJUG`).
//!
//! Contract (mirrors `retry_verification_call`):
//!
//! - [`VerifyErrorKind::Rpc`] is retried up to `max_attempts` with
//!   exponential backoff + jitter, capped at `max_delay`.
//! - [`VerifyErrorKind::Mismatch`] propagates immediately (genuine on-chain
//!   divergence — never retried).
//! - Any other failure propagates immediately.
//! - After exhausting attempts the last RPC failure is re-raised so the bot
//!   crashes loudly rather than silently continuing on unverified data.

use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::claims::{VerificationError, VerifyErrorKind};

/// Default policy knobs, mirrored byte-for-byte from
/// `verification_retry.py` / `runner/config.py`
/// (`VERIFICATION_RETRY_*` env knobs feed the same fields).
pub const DEFAULT_MAX_ATTEMPTS: u32 = 4;
pub const DEFAULT_BASE_DELAY: f64 = 0.5;
pub const DEFAULT_MAX_DELAY: f64 = 4.0;
pub const DEFAULT_JITTER: f64 = 0.5;

/// Bounded retry-with-backoff policy for transient verification failures.
#[derive(Clone, Debug, PartialEq)]
pub struct VerificationRetryPolicy {
    /// Total attempts (>= 1); the first attempt is not a retry.
    pub max_attempts: u32,
    /// Initial backoff delay in seconds.
    pub base_delay: f64,
    /// Backoff cap in seconds.
    pub max_delay: f64,
    /// Uniform jitter added to each backoff, in seconds.
    pub jitter: f64,
}

impl Default for VerificationRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            base_delay: DEFAULT_BASE_DELAY,
            max_delay: DEFAULT_MAX_DELAY,
            jitter: DEFAULT_JITTER,
        }
    }
}

impl VerificationRetryPolicy {
    /// Validate the field bounds (mirrors `__post_init__`); returns the
    /// Python `ValueError` message text on violation.
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

    /// The nth backoff delay (1-indexed attempt):
    /// `min(max_delay, base_delay * 2^(n-1)) + uniform(0, jitter)`.
    #[must_use]
    pub fn backoff_secs(&self, attempt: u32) -> f64 {
        let exp =
            self.base_delay * 2f64.powi(i32::try_from(attempt.saturating_sub(1)).unwrap_or(0));
        let capped = exp.min(self.max_delay);
        capped + jitter_fraction(attempt) * self.jitter
    }
}

/// A deterministic-per-call pseudo-random fraction in `[0, 1)` (no `rand`
/// dependency; the jitter only needs to de-correlate retries).
fn jitter_fraction(attempt: u32) -> f64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0_u64, |d| u64::from(d.subsec_nanos()));
    let mut z = nanos
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(u64::from(attempt));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    #[expect(
        clippy::cast_precision_loss,
        reason = "53-bit mantissa fraction; the value is a jitter scalar"
    )]
    let fraction = (z >> 11) as f64 / (1_u64 << 53) as f64;
    fraction
}

/// Call `attempt_fn(attempt_number)` with bounded retry on RPC failures.
///
/// `attempt_number` is 1-indexed. Only [`VerifyErrorKind::Rpc`] is retried;
/// all other failures return immediately.
///
/// # Errors
///
/// Returns the last failure (an exhausted RPC failure or the immediate fatal).
pub async fn retry_verification_call<F, Fut>(
    policy: &VerificationRetryPolicy,
    mut attempt_fn: F,
) -> Result<(), VerificationError>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<(), VerificationError>>,
{
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match attempt_fn(attempt).await {
            Ok(()) => return Ok(()),
            Err(err) if err.kind == VerifyErrorKind::Rpc && attempt < policy.max_attempts => {
                let delay = policy.backoff_secs(attempt);
                if delay > 0.0 {
                    tokio::time::sleep(Duration::from_secs_f64(delay)).await;
                }
            }
            Err(err) => return Err(err),
        }
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests assert on known-valid inputs")]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    fn zero_policy(max_attempts: u32) -> VerificationRetryPolicy {
        VerificationRetryPolicy {
            max_attempts,
            base_delay: 0.0,
            max_delay: 0.0,
            jitter: 0.0,
        }
    }

    #[tokio::test]
    async fn retries_rpc_until_success() {
        let policy = zero_policy(3);
        let attempts = Arc::new(AtomicU32::new(0));
        let result = retry_verification_call(&policy, |n| {
            let attempts = Arc::clone(&attempts);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                if n < 3 {
                    Err(VerificationError::new(VerifyErrorKind::Rpc, "transient"))
                } else {
                    Ok(())
                }
            }
        })
        .await;
        assert!(result.is_ok());
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn mismatch_is_fatal_and_never_retried() {
        let policy = zero_policy(3);
        let attempts = Arc::new(AtomicU32::new(0));
        let result = retry_verification_call(&policy, |_n| {
            let attempts = Arc::clone(&attempts);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(VerificationError::new(
                    VerifyErrorKind::Mismatch,
                    "diverged",
                ))
            }
        })
        .await;
        assert_eq!(result.unwrap_err().kind, VerifyErrorKind::Mismatch);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn rpc_exhaustion_reraises_the_last_error() {
        let policy = zero_policy(2);
        let attempts = Arc::new(AtomicU32::new(0));
        let result = retry_verification_call(&policy, |_n| {
            let attempts = Arc::clone(&attempts);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(VerificationError::new(VerifyErrorKind::Rpc, "down"))
            }
        })
        .await;
        assert_eq!(result.unwrap_err().kind, VerifyErrorKind::Rpc);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn validate_bounds_mirror_the_python_errors() {
        assert!(VerificationRetryPolicy::default().validate().is_ok());
        assert!(zero_policy(0).validate().is_err());
        let bad_jitter = VerificationRetryPolicy {
            jitter: 2.0,
            ..VerificationRetryPolicy::default()
        };
        assert!(bad_jitter.validate().is_err());
        let bad_order = VerificationRetryPolicy {
            base_delay: 5.0,
            max_delay: 1.0,
            ..VerificationRetryPolicy::default()
        };
        assert!(bad_order.validate().is_err());
    }

    #[test]
    fn backoff_is_exponential_and_capped() {
        let policy = VerificationRetryPolicy {
            max_attempts: 6,
            base_delay: 0.5,
            max_delay: 4.0,
            jitter: 0.0,
        };
        assert!((policy.backoff_secs(1) - 0.5).abs() < f64::EPSILON);
        assert!((policy.backoff_secs(2) - 1.0).abs() < f64::EPSILON);
        assert!((policy.backoff_secs(3) - 2.0).abs() < f64::EPSILON);
        assert!((policy.backoff_secs(4) - 4.0).abs() < f64::EPSILON);
        assert!((policy.backoff_secs(9) - 4.0).abs() < f64::EPSILON);
    }
}
