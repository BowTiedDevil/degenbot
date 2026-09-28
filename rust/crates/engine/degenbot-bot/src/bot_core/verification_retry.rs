//! Bounded retry-with-backoff for transient registration-verify failures.
//!
//! The retry *classification* is the core verify taxonomy carried by
//! [`VerifyError`]: a per-call RPC transport failure ([`VerifyError::Rpc`]) or
//! a verify-provider construction failure ([`VerifyError::Provider`]) is
//! transient; a snapshot mismatch ([`VerifyError::Snapshot`]) is genuine
//! on-chain divergence and every other failure is fatal. The backoff *policy*
//! is the caller's [`RetryPolicy`] — this module owns only the dance.
//!
//! After exhausting attempts the last transient failure is returned so the
//! caller crashes loudly rather than continuing on unverified data.

use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use degenbot_core::retry::RetryPolicy;

use super::snapshot_verify::VerifyError;

/// Whether a core verify failure is transient and may be re-attempted.
///
/// [`VerifyError::Rpc`] (per-call transport) and [`VerifyError::Provider`]
/// (provider construction) are retriable; [`VerifyError::Snapshot`] is a
/// genuine mismatch and the remaining variants are fatal.
#[must_use]
pub fn is_retryable(err: &VerifyError) -> bool {
    matches!(err, VerifyError::Rpc(_) | VerifyError::Provider(_))
}

/// A pseudo-random fraction in `[0, 1)` derived from the clock and the attempt
/// number. No RNG dependency is warranted: the jitter only de-correlates
/// retries.
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

/// Call `attempt_fn(attempt_number)` with bounded retry on transient failures.
///
/// `attempt_number` is 1-indexed. Only [`is_retryable`] failures are retried;
/// all other failures return immediately.
///
/// # Errors
///
/// Returns the last transient failure after exhausting `policy`, or the first
/// fatal failure.
pub async fn retry_verification_call<F, Fut>(
    policy: &RetryPolicy,
    mut attempt_fn: F,
) -> Result<(), VerifyError>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<(), VerifyError>>,
{
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match attempt_fn(attempt).await {
            Ok(()) => return Ok(()),
            Err(err) if is_retryable(&err) && attempt < policy.max_attempts => {
                let delay = policy.capped_backoff(attempt)
                    + Duration::from_secs_f64(jitter_fraction(attempt) * policy.jitter);
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
            }
            Err(err) => return Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    fn zero_policy(max_attempts: u32) -> RetryPolicy {
        RetryPolicy {
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
                    Err(VerifyError::Rpc("transient".to_string()))
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
    async fn retries_provider_construction_until_success() {
        let policy = zero_policy(2);
        let attempts = Arc::new(AtomicU32::new(0));
        let result = retry_verification_call(&policy, |n| {
            let attempts = Arc::clone(&attempts);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(VerifyError::Provider("provider init failed".to_string()))
                } else {
                    Ok(())
                }
            }
        })
        .await;
        assert!(result.is_ok());
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn snapshot_mismatch_is_fatal_and_never_retried() {
        let policy = zero_policy(3);
        let attempts = Arc::new(AtomicU32::new(0));
        let result = retry_verification_call(&policy, |_n| {
            let attempts = Arc::clone(&attempts);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(VerifyError::Snapshot("diverged".to_string()))
            }
        })
        .await;
        assert!(matches!(result, Err(VerifyError::Snapshot(m)) if m == "diverged"));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn other_errors_are_fatal_and_never_retried() {
        let policy = zero_policy(3);
        let attempts = Arc::new(AtomicU32::new(0));
        let result = retry_verification_call(&policy, |_n| {
            let attempts = Arc::clone(&attempts);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(VerifyError::NotConfigured("missing url".to_string()))
            }
        })
        .await;
        assert!(matches!(result, Err(VerifyError::NotConfigured(_))));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn rpc_exhaustion_returns_the_last_error() {
        let policy = zero_policy(2);
        let attempts = Arc::new(AtomicU32::new(0));
        let result = retry_verification_call(&policy, |_n| {
            let attempts = Arc::clone(&attempts);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(VerifyError::Rpc("down".to_string()))
            }
        })
        .await;
        assert!(matches!(result, Err(VerifyError::Rpc(m)) if m == "down"));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn validate_rejects_out_of_range_knobs() {
        assert!(RetryPolicy::verification_default().validate().is_ok());
        assert!(zero_policy(0).validate().is_err());
        let bad_jitter = RetryPolicy {
            jitter: 2.0,
            ..RetryPolicy::verification_default()
        };
        assert!(bad_jitter.validate().is_err());
        let bad_order = RetryPolicy {
            base_delay: 5.0,
            max_delay: 1.0,
            ..RetryPolicy::verification_default()
        };
        assert!(bad_order.validate().is_err());
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
}
