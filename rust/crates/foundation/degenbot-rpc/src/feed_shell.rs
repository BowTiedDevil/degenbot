//! The shared deferred-start shell every feed pump embeds.
//!
//! A feed pump provisions its ring and counters at build time and launches its
//! reconnect loop later, at the host's start boundary. That deferred-start
//! discipline is the same for every feed: a one-shot start latch, a stop
//! channel that ends the loop without tearing the ring down, and the runtime
//! dispatch that launches the loop on the current tokio runtime or the
//! crate-global one. The shell holds that plumbing once; a feed embeds it and
//! delegates `start`/`stop` to it, and its `Drop` carries the stop send.
//!
//! The shell owns only the lifecycle discipline — the ring ends and the
//! telemetry counters stay with the feed that provisions them.

use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::watch;

/// The one-shot latch, stop channel, and launch dispatch a deferred feed owns.
pub(crate) struct DeferredFeedShell {
    stop_tx: watch::Sender<bool>,
    started: AtomicBool,
}

impl DeferredFeedShell {
    /// Mint the shell with its stop channel. Nothing is dialed or spawned.
    pub(crate) fn new() -> Self {
        let (stop_tx, _stop_rx) = watch::channel(false);
        Self {
            stop_tx,
            started: AtomicBool::new(false),
        }
    }

    /// Launch the loop exactly once: the first call takes the latch, subscribes
    /// the loop's stop receiver, and hands it to `spawn`; every later call is a
    /// no-op. The receiver is subscribed AFTER the latch is taken, so a stop
    /// before or after the start is honored.
    pub(crate) fn start_once(&self, spawn: impl FnOnce(watch::Receiver<bool>)) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        spawn(self.stop_tx.subscribe());
    }

    /// Politely stop the pump (idempotent): every stop receiver observes the
    /// change; a receiver that is already gone makes this a plain no-op.
    pub(crate) fn stop(&self) {
        let _ = self.stop_tx.send(true);
    }
}

impl Drop for DeferredFeedShell {
    fn drop(&mut self) {
        let _ = self.stop_tx.send(true);
    }
}

/// Spawn a feed's reconnect loop on the current tokio runtime when one is
/// entered, else the crate-global runtime — the dispatch every feed's launch
/// shares.
pub(crate) fn spawn_feed_loop(fut: impl std::future::Future<Output = ()> + Send + 'static) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(fut);
    } else {
        degenbot_core::runtime::get_runtime().spawn(fut);
    }
}
