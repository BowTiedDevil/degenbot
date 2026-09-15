//! Bounded retry-with-backoff for transient verification RPC failures.
//!
//! Retry *classification* stays here — only [`VerifyErrorKind::Rpc`] is
//! retried; [`VerifyErrorKind::Mismatch`] (genuine on-chain divergence) and
//! every other failure propagate immediately. The backoff *policy* is the
//! workspace-canonical [`RetryPolicy`] (degenbot-core), shared with the RPC
//! provider's wait loops.
//!
//! After exhausting attempts the last RPC failure is re-raised so the bot
//! crashes loudly rather than silently continuing on unverified data.

use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use degenbot::core::retry::RetryPolicy;

use crate::claims::{VerificationError, VerifyErrorKind};

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
    policy: &RetryPolicy,
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
#[expect(clippy::unwrap_used, reason = "tests assert on known-valid inputs")]
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
