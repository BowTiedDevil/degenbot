//! Owned dedicated pubsub runtime: the ONE home of the dedicated-runtime
//! teardown discipline.
//!
//! An engine that owns a WS/IPC transport whose alloy pubsub service (and any
//! task driving that transport) must be cancelled DETERMINISTICALLY when the
//! owner drops — rather than left to a contended shared-runtime worker — builds
//! an [`OwnedPubsubRuntime`]. The type packages the whole discipline:
//!
//! 1. a single-worker multi-thread `tokio` runtime with a DISTINCT named
//!    thread (`worker_census` module rule: two runtimes must never share
//!    `tokio-runtime-worker`),
//! 2. a live-instance census registration (see below), and
//! 3. the [`Drop`] split that JOINS the work the owner spawned, bounded by
//!    [`SHUTDOWN_GRACE`], so a task that finishes on the closed request channel
//!    reaches its completion edge BEFORE the runtime drop cancels it.
//!
//! The discipline was derived twice, verbatim, at the two shipping owners —
//! `AnvilFork` (IPC pubsub) and `HostedSources` (head-watch WS pubsub). Porting
//! a future WS-owning actor by re-deriving it from those two examples is the
//! exact failure mode the original leak came from; importing this type is not.
//!
//! # The teardown split
//!
//! Dropping a multi-thread runtime BLOCKS while it joins its workers, and
//! `block_on`ing one from inside an async execution environment PANICS. The
//! split handles both call shapes:
//!
//! - OFF an async context (the common case: a `PyO3` handle dropped by `CPython`
//!   refcounting on the main thread): `block_on` the runtime across
//!   [`SHUTDOWN_GRACE`], joining the in-flight work, then drop the runtime here.
//! - INSIDE an async context: run the same join-and-drop on a DETACHED `std`
//!   thread, where blocking (the join, then the worker join) is allowed.
//!
//! # Joining spawned work
//!
//! [`OwnedPubsubRuntime::spawn`] is this type's only spawn door: no raw
//! [`Handle`] escapes it, so an owner cannot spawn onto the dedicated runtime
//! untracked. Every future handed to it is registered as in-flight work: a
//! guard increments the count on spawn and decrements it when the task's body
//! ends — on completion OR on cancellation, whichever comes first — and the
//! last one out notifies the drop. [`Drop`] awaits that edge for no longer than
//! [`SHUTDOWN_GRACE`], so a task that reacts to the closed request channel is
//! joined deterministically and one that will not finish is still cancelled at
//! the bound.
//!
//! A future that itself spawns through `tokio::spawn` or `Handle::current()` —
//! as alloy's pubsub service and the head-watch driver do — is not counted; the
//! runtime drop is what cancels it.
//!
//! # Live-instance census counting
//!
//! [`crate::worker_census::register`] UPSERTS by `resource`: registering a fixed
//! `count: 1` from each of N owners leaves ONE row still saying `1`, so the
//! census UNDERCOUNTS the live fleet. This type instead keeps a process-global
//! per-resource live counter, incremented on [`OwnedPubsubRuntime::build`] and
//! decremented in [`Drop`], and re-publishes the CURRENT value on every change.
//! The census snapshot therefore reports the true fleet size at dump time, not
//! a stale count frozen at first registration.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::runtime::{Handle, Runtime};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::worker_census::{self, WorkerCensusEntry};

/// The bound [`OwnedPubsubRuntime`]'s [`Drop`] waits across the dedicated
/// runtime for the work it spawned to reach its completion edge, before the
/// runtime drop cancels what is left.
///
/// A task reacting to a closed request channel has to be woken, polled and run
/// its last step inside this window; a task that will not finish is cancelled
/// when the window expires. The duration is a deadline, not a sleep the teardown
/// hopes the worker wakes under.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(200);

/// Build parameters for one [`OwnedPubsubRuntime`].
///
/// `resource` / `thread_name` / `kind` / `sizing` / `binding` are exactly the
/// [`WorkerCensusEntry`] fields the owning spawn site must declare (see the
/// `worker_census` module docs).
#[derive(Clone, Copy, Debug)]
pub struct OwnedPubsubRuntimeSpec {
    /// Stable, low-cardinality census resource id (one per owning subsystem).
    pub resource: &'static str,
    /// Census kind label for this runtime surface.
    pub kind: &'static str,
    /// Distinct OS thread name for the single worker.
    pub thread_name: &'static str,
    /// Census sizing rule in words.
    pub sizing: &'static str,
    /// Census binding vocabulary entry (`pinned` for a dedicated thread).
    pub binding: &'static str,
}

/// Process-global live-instance count per census resource id.
static LIVE_COUNTS: OnceLock<Mutex<BTreeMap<&'static str, usize>>> = OnceLock::new();

fn live_counts() -> &'static Mutex<BTreeMap<&'static str, usize>> {
    LIVE_COUNTS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Increment (`increment == true`) or decrement the live count for `resource`
/// and return the new value.
fn adjust_live_count(resource: &'static str, increment: bool) -> usize {
    let mut counts = live_counts().lock();
    let slot = counts.entry(resource).or_insert(0);
    if increment {
        *slot += 1;
    } else {
        *slot = slot.saturating_sub(1);
    }
    *slot
}

/// Publish `spec` into the census with the given CURRENT `count` (upsert).
fn publish(spec: OwnedPubsubRuntimeSpec, count: usize) {
    worker_census::register(WorkerCensusEntry {
        resource: spec.resource,
        kind: spec.kind,
        count,
        thread_name: spec.thread_name,
        sizing: spec.sizing,
        binding: spec.binding,
    });
}

/// The unfinished-work count of one owned runtime, and the edge its [`Drop`]
/// waits on.
struct InFlight {
    count: AtomicUsize,
    settled: Notify,
}

impl InFlight {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            count: AtomicUsize::new(0),
            settled: Notify::new(),
        })
    }

    /// Register one more task and return the guard that unregisters it at its
    /// completion edge.
    fn guard(self: &Arc<Self>) -> InFlightGuard {
        self.count.fetch_add(1, Ordering::AcqRel);
        InFlightGuard(Arc::clone(self))
    }

    /// Wait, no longer than `bound`, for every registered task to finish.
    async fn wait_settled(&self, bound: Duration) {
        let drained = async {
            while self.count.load(Ordering::Acquire) != 0 {
                self.settled.notified().await;
            }
        };
        let _bounded = tokio::time::timeout(bound, drained).await;
    }
}

/// Unregisters one task from [`InFlight`] when its body ends — on completion or
/// on cancellation, whichever comes first.
struct InFlightGuard(Arc<InFlight>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if self.0.count.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.settled.notify_one();
        }
    }
}

/// A dedicated single-worker `tokio` runtime whose owning type is the only
/// handle to it, carrying the repo's deterministic teardown discipline and a
/// live census count. See the module docs.
pub struct OwnedPubsubRuntime {
    runtime: Option<Runtime>,
    spec: OwnedPubsubRuntimeSpec,
    in_flight: Arc<InFlight>,
}

impl OwnedPubsubRuntime {
    /// Build the dedicated runtime (single worker, `spec.thread_name`, all
    /// drivers enabled), register its worker in [`worker_census`], and record
    /// one more live instance for `spec.resource`.
    ///
    /// # Errors
    ///
    /// The OS refused to spawn the worker thread (resource exhaustion).
    pub fn build(spec: OwnedPubsubRuntimeSpec) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name(spec.thread_name)
            .enable_all()
            .build()?;
        let live = adjust_live_count(spec.resource, true);
        publish(spec, live);
        Ok(Self {
            runtime: Some(runtime),
            spec,
            in_flight: InFlight::new(),
        })
    }

    /// The live owned-instance count for `resource` — the census truth this type
    /// keeps, also readable by tests.
    #[must_use]
    pub fn live_count(resource: &'static str) -> usize {
        live_counts().lock().get(resource).copied().unwrap_or(0)
    }

    /// Spawn `future` onto the dedicated runtime, registered as in-flight work
    /// so [`Drop`] joins it before the runtime teardown cancels what is left.
    ///
    /// # Panics
    ///
    /// Never in practice: the runtime is `None` only after [`Drop`] has taken
    /// it, at which point no method can be called.
    #[expect(
        clippy::expect_used,
        reason = "the inner runtime is None only after Drop's take; live callers always see Some"
    )]
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let guard = self.in_flight.guard();
        self.runtime
            .as_ref()
            .expect("owned runtime is taken only in Drop")
            .spawn(async move {
                let _guard = guard;
                future.await
            })
    }
}

impl Drop for OwnedPubsubRuntime {
    fn drop(&mut self) {
        // Publish the decremented live count first: the census (and its export
        // hook) must reflect the fleet size at dump time.
        let live = adjust_live_count(self.spec.resource, false);
        publish(self.spec, live);

        // The teardown split (see the module docs). `Handle::try_current`
        // distinguishes "dropped off an async context" (the Python path) from
        // "dropped inside a task" (where `block_on` AND the runtime drop would
        // both panic).
        let Some(runtime) = self.runtime.take() else {
            return;
        };
        let in_flight = Arc::clone(&self.in_flight);
        let settle_then_drop = move || {
            runtime.block_on(in_flight.wait_settled(SHUTDOWN_GRACE));
            drop(runtime);
        };
        if Handle::try_current().is_ok() {
            std::thread::spawn(settle_then_drop);
        } else {
            settle_then_drop();
        }
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    /// Unique per test so the process-global live counter is never touched by a
    /// parallel test.
    const FLEET_RESOURCE: &str = "core_pubsub_runtime_fleet_test";
    const OFF_ASYNC_RESOURCE: &str = "core_pubsub_runtime_off_async_test";
    const INSIDE_ASYNC_RESOURCE: &str = "core_pubsub_runtime_inside_async_test";
    const JOIN_RESOURCE: &str = "core_pubsub_runtime_join_test";

    /// What a task needs after its request channel closes: long past any fixed
    /// sleep a teardown could spend hoping, and far inside the join bound, so
    /// the outcome is a property of the join rather than of timing luck.
    const CLEAN_EXIT_WORK: Duration = Duration::from_millis(50);

    fn spec(resource: &'static str, thread_name: &'static str) -> OwnedPubsubRuntimeSpec {
        OwnedPubsubRuntimeSpec {
            resource,
            kind: "test dedicated pubsub runtime",
            thread_name,
            sizing: "test: exactly one worker",
            binding: "pinned",
        }
    }

    /// Sets its flag when the wrapped future is DROPPED — the witness that a
    /// runtime teardown cancelled the parked task.
    struct DropWitness(Arc<AtomicBool>);

    impl Drop for DropWitness {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn wait_for(flag: &AtomicBool, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if flag.load(Ordering::SeqCst) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        flag.load(Ordering::SeqCst)
    }

    fn census_count(resource: &'static str) -> usize {
        worker_census::snapshot()
            .into_iter()
            .find(|entry| entry.resource == resource)
            .map_or(0, |entry| entry.count)
    }

    /// The census-count fix: N live instances must read as N, not the last
    /// registration's frozen `1` (the upsert-by-resource undercount).
    #[test]
    fn live_count_and_census_report_true_fleet_size() {
        assert_eq!(OwnedPubsubRuntime::live_count(FLEET_RESOURCE), 0);
        let first = OwnedPubsubRuntime::build(spec(FLEET_RESOURCE, "degenbot-core-pubsub-t1"))
            .expect("build first");
        assert_eq!(OwnedPubsubRuntime::live_count(FLEET_RESOURCE), 1);
        let second = OwnedPubsubRuntime::build(spec(FLEET_RESOURCE, "degenbot-core-pubsub-t2"))
            .expect("build second");
        assert_eq!(OwnedPubsubRuntime::live_count(FLEET_RESOURCE), 2);
        assert_eq!(
            census_count(FLEET_RESOURCE),
            2,
            "two live runtimes must publish count 2, not a frozen 1"
        );
        drop(first);
        assert_eq!(OwnedPubsubRuntime::live_count(FLEET_RESOURCE), 1);
        assert_eq!(census_count(FLEET_RESOURCE), 1);
        drop(second);
        assert_eq!(OwnedPubsubRuntime::live_count(FLEET_RESOURCE), 0);
        assert_eq!(census_count(FLEET_RESOURCE), 0);
    }

    /// OFF an async context: dropping the runtime cancels a parked task (its
    /// future is dropped by the runtime teardown).
    #[test]
    fn drop_off_async_cancels_a_parked_task() {
        let runtime =
            OwnedPubsubRuntime::build(spec(OFF_ASYNC_RESOURCE, "degenbot-core-pubsub-t3"))
                .expect("build");
        let started = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let started_task = Arc::clone(&started);
        let cancelled_task = Arc::clone(&cancelled);
        runtime.spawn(async move {
            let _witness = DropWitness(cancelled_task);
            started_task.store(true, Ordering::SeqCst);
            std::future::pending::<()>().await;
        });
        assert!(
            wait_for(&started, Duration::from_secs(2)),
            "the parked task must start before the drop"
        );
        drop(runtime);
        assert!(
            wait_for(&cancelled, Duration::from_secs(2)),
            "dropping the runtime off-async must cancel the parked task"
        );
    }

    /// INSIDE an async context: the split sleeps the grace and drops the
    /// runtime DETACHED, still cancelling the parked task.
    #[tokio::test]
    async fn drop_inside_async_cancels_a_parked_task() {
        let runtime =
            OwnedPubsubRuntime::build(spec(INSIDE_ASYNC_RESOURCE, "degenbot-core-pubsub-t4"))
                .expect("build");
        let started = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let started_task = Arc::clone(&started);
        let cancelled_task = Arc::clone(&cancelled);
        runtime.spawn(async move {
            let _witness = DropWitness(cancelled_task);
            started_task.store(true, Ordering::SeqCst);
            std::future::pending::<()>().await;
        });
        assert!(
            wait_for(&started, Duration::from_secs(2)),
            "the parked task must start before the drop"
        );
        drop(runtime);
        assert!(
            wait_for(&cancelled, Duration::from_secs(2)),
            "dropping the runtime inside async must cancel the parked task"
        );
    }

    /// The deterministic race the sleep-then-hope teardown lost: a task that
    /// finishes on channel close is joined by the drop, so its completion is
    /// visible before the drop returns. Against a fixed sleep it fails — the
    /// sleep expires, the runtime drop cancels the task mid-flight, and the flag
    /// is never set.
    #[test]
    fn drop_joins_spawned_work_before_returning() {
        let runtime = OwnedPubsubRuntime::build(spec(JOIN_RESOURCE, "degenbot-core-pubsub-t5"))
            .expect("build");
        let finished = Arc::new(AtomicBool::new(false));
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel::<()>();
        let finished_task = Arc::clone(&finished);
        runtime.spawn(async move {
            let _closed = closed_rx.await;
            tokio::time::sleep(CLEAN_EXIT_WORK).await;
            finished_task.store(true, Ordering::SeqCst);
        });
        closed_tx
            .send(())
            .expect("the request channel close reaches the spawned task");

        drop(runtime);

        assert!(
            finished.load(Ordering::SeqCst),
            "the drop must join a task that finishes on channel close, not cancel it mid-flight"
        );
    }
}
