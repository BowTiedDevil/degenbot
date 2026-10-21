//! The construction-time tick-RPC pace gate.
//!
//! `builders.tick_fetch_min_interval_ms` is declared once in the config schema;
//! this gate is its implementation. A constrained fork/archive source (dRPC's
//! free plan advertises 100 req/s) has to be paced below its advertised rate,
//! and pacing is an I/O concern of the construction seam — not of whichever
//! driver remembered to sleep. Every construction tick fetch crosses here, so
//! the operator's declared interval paces a `cargo add degenbot` bot exactly
//! as it paces the Python companion.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The minimum interval between two construction tick RPCs.
///
/// The gate reserves the next fetch slot under a short lock and sleeps outside
/// it, so concurrent fetchers space themselves instead of all waking at once.
/// An interval of zero disables the gate entirely (the declared default).
#[derive(Debug)]
pub struct TickFetchPace {
    min_interval: Duration,
    /// The instant the next fetch is allowed to start.
    next_slot: Mutex<Option<Instant>>,
}

impl TickFetchPace {
    /// The pace the resolved verdict declares (`builders.tick_fetch_min_interval_ms`).
    #[must_use]
    pub fn from_config(cfg: &degenbot_config::schema::BuildersConfig) -> Self {
        Self::from_millis(cfg.tick_fetch_min_interval_ms)
    }

    /// A pace of `min_interval_ms` between fetches; `0` disables it.
    #[must_use]
    pub fn from_millis(min_interval_ms: u64) -> Self {
        Self {
            min_interval: Duration::from_millis(min_interval_ms),
            next_slot: Mutex::new(None),
        }
    }

    /// No pacing — the declared default and the test posture.
    #[must_use]
    pub fn disabled() -> Self {
        Self::from_millis(0)
    }

    /// The configured interval.
    #[must_use]
    pub fn min_interval(&self) -> Duration {
        self.min_interval
    }

    /// Reserve the next fetch slot and wait for it.
    ///
    /// The lock is held only long enough to claim the slot; the sleep happens
    /// outside it, so a slow fetch never blocks a sibling's reservation.
    pub async fn wait(&self) {
        if self.min_interval.is_zero() {
            return;
        }
        let wait = {
            let now = Instant::now();
            let mut slot = self
                .next_slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let start = match *slot {
                Some(reserved) if reserved > now => reserved,
                _ => now,
            };
            *slot = Some(start + self.min_interval);
            start.saturating_duration_since(now)
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}

impl Default for TickFetchPace {
    fn default() -> Self {
        Self::disabled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_declared_default_is_disabled() {
        assert!(
            TickFetchPace::from_config(&degenbot_config::schema::BuildersConfig::default())
                .min_interval()
                .is_zero()
        );
    }

    #[tokio::test]
    async fn a_zero_interval_never_blocks() {
        let pace = TickFetchPace::disabled();
        let started = Instant::now();
        for _ in 0..50 {
            pace.wait().await;
        }
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a disabled gate stays off the fast path"
        );
    }

    #[tokio::test]
    async fn the_gate_spaces_two_fetches() {
        let pace = TickFetchPace::from_millis(40);
        pace.wait().await;
        let second = Instant::now();
        pace.wait().await;
        assert!(
            second.elapsed() >= Duration::from_millis(30),
            "the second fetch waited for its reserved slot, got {:?}",
            second.elapsed()
        );
    }

    #[tokio::test]
    async fn concurrent_fetches_slot_behind_each_other() {
        let pace = std::sync::Arc::new(TickFetchPace::from_millis(30));
        let started = Instant::now();
        let mut handles = Vec::new();
        for _ in 0..3 {
            let pace = std::sync::Arc::clone(&pace);
            handles.push(tokio::spawn(async move { pace.wait().await }));
        }
        for handle in handles {
            handle.await.expect("no panic");
        }
        assert!(
            started.elapsed() >= Duration::from_millis(55),
            "three fetch slots are two intervals apart, got {:?}",
            started.elapsed()
        );
    }
}
