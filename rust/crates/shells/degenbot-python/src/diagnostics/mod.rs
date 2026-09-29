//! Diagnostics instrumentation: GIL-probe + main-loop stuck-watchdog
//! . See [`gil_probe`] for the deadlock measurement rationale.

pub mod gil_probe;
pub mod thread_registry;
