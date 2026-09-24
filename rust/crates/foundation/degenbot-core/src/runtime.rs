//! Shared Tokio runtime management.
//!
//! This module provides a singleton multi-threaded Tokio runtime instance
//! that can be shared across multiple Python-bound objects, avoiding the
//! overhead of creating a separate runtime for each contract or provider instance.
//!
//! # Why Multi-Threaded?
//!
//! The runtime uses `Builder::new_multi_thread()` rather than
//! `new_current_thread()` to support concurrent RPC calls from multiple Python
//! threads. With Python 3.13+ free-threading (no GIL), multiple threads can
//! call into Rust provider/contract methods simultaneously. A multi-threaded
//! Tokio runtime enables true parallelism for these I/O-bound operations,
//! while a current-thread runtime would serialize them into a bottleneck.
//!
//! Two-runtime sizing: the worker count comes from the cgroup-aware CPU
//! budget (`crate::cpu_budget`) — the leftover after the solve bins take
//! theirs — and NOT from `available_parallelism`, which reads 24 host cores
//! inside an 8-core cgroup quota in this devcontainer. Operators pin it
//! with the typed `runtime.io_workers` key (env `DEGENBOT_IO_WORKERS`).
//!
//! # Lazy Initialization
//!
//! The runtime is only created on first call to `get_runtime()`. Pure Rust
//! functions (`tick_math`, `decoder`, `address_utils`) never initialize
//! it, so scripts that don't use provider/contract code pay no runtime cost.
//!
//! # Usage
//!
//! ```no_run
//! use degenbot_core::runtime::get_runtime;
//!
//! let runtime = get_runtime();
//! let result = runtime.block_on(async {
//!     // async code here
//!     42
//! });
//! ```

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use tokio::runtime::{Builder, Runtime};

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// Worker-thread sequence for the distinct thread names below. tokio 1.53
/// spawns every worker through the blocking pool's `spawn_thread`, calling
/// the `thread_name_fn` closure once per spawned thread — so an internal
/// counter yields the distinct `degenbot-io-rt-N` names the census declares
/// (GOQWCL: two defaulting `tokio-runtime-worker` pools made thread dumps
/// unattributable).
static IO_RT_SEQ: AtomicUsize = AtomicUsize::new(0);

fn io_runtime_thread_name() -> String {
    format!(
        "degenbot-io-rt-{}",
        IO_RT_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

fn build_runtime() -> Result<Runtime, std::io::Error> {
    // the cgroup-aware budget is the single sizing authority for the
    // ambient runtime (see `crate::cpu_budget::ambient_io_worker_count`).
    let workers = crate::cpu_budget::ambient_io_worker_count();
    // self-register in the worker census; fail-destructive-free (the
    // registry never rejects — see worker_census module docs).
    crate::worker_census::register(crate::worker_census::WorkerCensusEntry {
        resource: "io_runtime_workers",
        kind: "tokio multi-thread runtime (ambient I/O — pump, dispatch, delivery, pyo3-async)",
        count: workers,
        thread_name: "degenbot-io-rt-{n}",
        sizing: "cpu_budget::ambient_io_worker_count — cgroup budget minus the solve bins, floored at 1; override `runtime.io_workers` (env DEGENBOT_IO_WORKERS)",
        binding: "shared",
    });
    Builder::new_multi_thread()
        .worker_threads(workers)
        .thread_name_fn(io_runtime_thread_name)
        .enable_all()
        .build()
}

/// Get the shared Tokio runtime instance.
///
/// This function lazily initializes a multi-threaded Tokio runtime
/// on first call. Subsequent calls return the same runtime instance.
///
/// Worker thread count is sized by the cgroup-aware CPU budget
/// ([`crate::cpu_budget`]) and can be pinned explicitly with the typed
/// `runtime.io_workers` key (env `DEGENBOT_IO_WORKERS`).
///
/// # Panics
///
/// Panics if the runtime fails to create (e.g., if the system cannot
/// spawn the required threads).
pub fn get_runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        // Targeted expect (fulfilled): a global `&'static Runtime` initializer
        // has no error channel, so a failed spawn is panic loudly, as the
        // `# Panics` doc above documents.
        #[expect(clippy::panic)]
        build_runtime().unwrap_or_else(|e| panic!("Failed to create Tokio runtime: {e}"))
    })
}

#[cfg(test)]
#[expect(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_build_runtime_sizes_from_cgroup_budget_policy() {
        // the ambient runtime is sized by the relocated cpu_budget
        // policy (cgroup+affinity budget, minus the solve bins, floored at
        // 1) — never from tokio's available_parallelism default, which reads
        // 24 host cores inside an 8-core cgroup quota here.
        let expected = crate::cpu_budget::ambient_io_worker_count_from(
            ::degenbot_config::holder::config().runtime.io_workers,
            crate::cpu_budget::solve_worker_count(),
            crate::cpu_budget::effective_cpu_budget(),
        );
        let rt = build_runtime().unwrap();
        assert_eq!(
            rt.metrics().num_workers(),
            expected,
            "ambient runtime workers must follow the CPU-budget policy"
        );
    }

    /// Ambient runtime workers must carry the DISTINCT
    /// census thread name (no `tokio-runtime-worker` collisions with the
    /// inline-sim runtime or the solve fleet).
    #[test]
    fn ambient_runtime_workers_carry_the_census_thread_name() {
        let rt = build_runtime().unwrap();
        let name = rt.block_on(async {
            tokio::spawn(async move { std::thread::current().name().map(str::to_owned) })
                .await
                .expect("worker spawn")
        });
        let name = name.expect("worker thread name");
        assert!(
            name.starts_with("degenbot-io-rt-"),
            "ambient worker must be census-named, got {name}"
        );
    }

    /// the runtime self-registers; the census row must agree with
    /// the built runtime (count == worker count, thread-name pattern).
    #[test]
    fn ambient_runtime_registers_a_census_row_matching_its_worker_count() {
        let rt = build_runtime().unwrap();
        let row = crate::worker_census::snapshot()
            .into_iter()
            .find(|e| e.resource == "io_runtime_workers")
            .expect("ambient runtime must self-register in the census");
        assert_eq!(row.count, rt.metrics().num_workers());
        assert_eq!(row.thread_name, "degenbot-io-rt-{n}");
        assert_eq!(row.sizing, row.sizing); // sizing text is documentation; presence is the contract
    }

    #[test]
    fn test_runtime_singleton() {
        let rt1 = get_runtime();
        let rt2 = get_runtime();

        assert!(std::ptr::eq(rt1, rt2));
    }

    #[test]
    fn test_runtime_can_spawn_tasks() {
        let runtime = get_runtime();

        let result = runtime.block_on(async {
            let handle = tokio::spawn(async { 42 });
            handle
                .await
                .expect("spawned task should complete successfully")
        });

        assert_eq!(result, 42);
    }
}
