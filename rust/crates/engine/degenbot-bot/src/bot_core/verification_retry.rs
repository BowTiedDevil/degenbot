//! Bounded retry-with-backoff for transient registration-verify failures.
//!
//! The retry *classification* is the fatal-vs-transient split the one
//! [`RefusalKind`] vocabulary carries: a verify failure with
//! `fatal == false` (a per-call RPC transport failure or a verify-provider
//! construction failure) is transient; a fatal verify failure (a genuine
//! on-chain divergence) is never retried. The one retryable
//! [`LifecyclePrecondition`] is a tracked pool that reached a verify step with
//! no provider configured — the historical provider-construction class. The
//! backoff *policy* is the caller's [`RetryPolicy`] — this module owns only
//! the dance.
//!
//! After exhausting attempts the last transient failure is returned so the
//! caller crashes loudly rather than continuing on unverified data.

use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use degenbot_core::retry::RetryPolicy;

use super::registration_ledger::{LifecyclePrecondition, RefusalKind};

/// Whether a core verify failure is transient and may be re-attempted.
///
/// A non-fatal verify failure (a per-call transport or provider-construction
/// refusal) is retriable; a fatal one is a genuine mismatch and is never
/// re-attempted. A tracked pool that reached a verify step with no provider
/// configured is the one retryable lifecycle precondition — the historical
/// provider-construction class.
#[must_use]
pub fn is_retryable(err: &RefusalKind) -> bool {
    matches!(err, RefusalKind::VerifyFailed { fatal: false, .. })
        || matches!(
            err,
            RefusalKind::LifecyclePrecondition {
                reason: LifecyclePrecondition::MissingVerifyProvider,
                ..
            }
        )
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
) -> Result<(), RefusalKind>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<(), RefusalKind>>,
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
    use crate::bot_core::registration_ledger::{RegistrationPhase, VerifyOrigin};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    /// A transient transport refusal.
    fn rpc(detail: &str) -> RefusalKind {
        RefusalKind::verify_failed(
            RegistrationPhase::Verify,
            false,
            VerifyOrigin::Transport,
            detail.to_owned(),
        )
    }

    /// A transient provider-construction refusal.
    fn provider(detail: &str) -> RefusalKind {
        RefusalKind::verify_failed(
            RegistrationPhase::Verify,
            false,
            VerifyOrigin::ProviderConstruction,
            detail.to_owned(),
        )
    }

    /// A fatal mismatch refusal.
    fn mismatch(detail: &str) -> RefusalKind {
        RefusalKind::verify_failed(
            RegistrationPhase::Verify,
            true,
            VerifyOrigin::Mismatch,
            detail.to_owned(),
        )
    }

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
                    Err(rpc("transient"))
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
                    Err(provider("provider init failed"))
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
                Err(mismatch("diverged"))
            }
        })
        .await;
        assert!(matches!(
            result,
            Err(RefusalKind::VerifyFailed { fatal: true, detail, .. }) if detail == "diverged"
        ));
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
                Err(RefusalKind::verify_not_configured(
                    RegistrationPhase::Verify,
                    "missing url".to_string(),
                ))
            }
        })
        .await;
        assert!(matches!(
            result,
            Err(RefusalKind::VerifyNotConfigured { .. })
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn other_verify_failures_are_fatal_and_never_retried() {
        let policy = zero_policy(3);
        let attempts = Arc::new(AtomicU32::new(0));
        let result = retry_verification_call(&policy, |_n| {
            let attempts = Arc::clone(&attempts);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(RefusalKind::verify_failed(
                    RegistrationPhase::Verify,
                    true,
                    VerifyOrigin::Other,
                    "driver failure".to_string(),
                ))
            }
        })
        .await;
        assert!(matches!(
            result,
            Err(RefusalKind::VerifyFailed { origin: VerifyOrigin::Other, detail, .. })
                if detail == "driver failure"
        ));
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
                Err(rpc("down"))
            }
        })
        .await;
        assert!(matches!(
            result,
            Err(RefusalKind::VerifyFailed { fatal: false, detail, .. }) if detail == "down"
        ));
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
