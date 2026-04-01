//! Shared Tokio runtime management.
//!
//! This module provides a singleton runtime instance that can be shared
//! across multiple Python-bound objects, avoiding the overhead of creating
//! a separate runtime for each contract or provider instance.
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
