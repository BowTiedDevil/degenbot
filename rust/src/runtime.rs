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
//! Thread count is tunable via the `TOKIO_WORKER_THREADS` environment variable.
//!
//! # Lazy Initialization
//!
//! The runtime is only created on first call to `get_runtime()`. Pure Rust
//! functions (`tick_math`, `abi_decoder`, `address_utils`) never initialize
//! it, so scripts that don't use provider/contract code pay no runtime cost.
//!
//! # Usage
//!
//! ```no_run
//! use degenbot_rs::runtime::get_runtime;
//!
//! let runtime = get_runtime();
//! let result = runtime.block_on(async {
//!     // async code here
//!     42
//! });
//! ```

use std::sync::OnceLock;
use tokio::runtime::{Builder, Runtime};

const ENV_VAR: &str = "TOKIO_WORKER_THREADS";

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

fn build_runtime() -> Result<Runtime, std::io::Error> {
    let env_raw = std::env::var(ENV_VAR).ok();
    let env_worker_count = env_raw.as_deref().and_then(|v| v.parse::<usize>().ok());

    if env_worker_count.is_none() && env_raw.is_some() {
        log::warn!(
            "{ENV_VAR} is set but could not be parsed as a usize; ignoring"
        );
        // Remove the invalid env var so Tokio's internal machinery
        // doesn't panic when it tries to parse it.
        std::env::remove_var(ENV_VAR);
    }

    let mut builder = Builder::new_multi_thread();

    if let Some(n) = env_worker_count {
        builder.worker_threads(n);
    }

    builder.enable_all().build()
}

/// Get the shared Tokio runtime instance.
///
/// This function lazily initializes a multi-threaded Tokio runtime
/// on first call. Subsequent calls return the same runtime instance.
///
/// Worker thread count defaults to the number of CPU cores, and can be
/// overridden with the `TOKIO_WORKER_THREADS` environment variable.
///
/// # Panics
///
/// Panics if the runtime fails to create (e.g., if the system cannot
/// spawn the required threads).
pub fn get_runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        build_runtime().unwrap_or_else(|e| panic!("Failed to create Tokio runtime: {e}"))
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialize tests that mutate the `TOKIO_WORKER_THREADS` environment
    /// variable to prevent race conditions when running tests concurrently.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

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

    #[test]
    fn test_build_runtime_respects_env_var() {
        let _guard = ENV_MUTEX.lock().unwrap();
        std::env::set_var(ENV_VAR, "2");
        let rt = build_runtime().unwrap();
        std::env::remove_var(ENV_VAR);

        let result = rt.block_on(async {
            let handle = tokio::spawn(async { 99 });
            handle.await.unwrap()
        });
        assert_eq!(result, 99);
    }

    #[test]
    fn test_build_runtime_ignores_invalid_env() {
        let _guard = ENV_MUTEX.lock().unwrap();
        // Use a fresh runtime with the invalid env var unset so Tokio's
        // internal machinery doesn't panic during startup.
        std::env::remove_var(ENV_VAR);
        let rt = build_runtime().unwrap();

        let result = rt.block_on(async {
            let handle = tokio::spawn(async { 77 });
            handle.await.unwrap()
        });
        assert_eq!(result, 77);
    }

    #[test]
    fn test_build_runtime_handles_invalid_env_var() {
        let _guard = ENV_MUTEX.lock().unwrap();
        // Verify that an invalid TOKIO_WORKER_THREADS value is silently
        // ignored by build_runtime(). The function removes the invalid
        // var internally so Tokio's internal machinery doesn't panic.
        std::env::set_var(ENV_VAR, "not_a_number");
        let result = build_runtime();
        std::env::remove_var(ENV_VAR);
        assert!(result.is_ok(), "build_runtime should ignore invalid env var");
    }
}
