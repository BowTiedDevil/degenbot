//! Shared Tokio runtime management.
//!
//! This module provides a singleton multi-threaded Tokio runtime instance
//! that can be shared across multiple Python-bound objects, avoiding the
//! overhead of creating a separate runtime for each contract or provider instance.
//!
//! # Why Multi-Threaded?
//!
//! The runtime uses `Runtime::new()` (multi-threaded scheduler) rather than
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
//! ```rust,ignore
//! use crate::runtime::get_runtime;
//!
//! let runtime = get_runtime();
//! let result = runtime.block_on(async {
//!     // async code here
//! });
//! ```

use std::sync::OnceLock;
use tokio::runtime::Runtime;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// Get the shared Tokio runtime instance.
///
/// This function lazily initializes a multi-threaded Tokio runtime
/// on first call. Subsequent calls return the same runtime instance.
///
/// # Panics
///
/// Panics if the runtime fails to create (e.g., if the system cannot
/// spawn the required threads).
#[allow(clippy::expect_used)]
pub fn get_runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        Runtime::new().expect("Failed to create Tokio runtime")
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_singleton() {
        let rt1 = get_runtime();
        let rt2 = get_runtime();
        
        // Both should be the same instance
        assert!(std::ptr::eq(rt1, rt2));
    }

    #[test]
    fn test_runtime_can_spawn_tasks() {
        let runtime = get_runtime();
        
        let result = runtime.block_on(async {
            let handle = tokio::spawn(async {
                42
            });
            handle.await.expect("spawned task should complete successfully")
        });
        
        assert_eq!(result, 42);
    }
}
