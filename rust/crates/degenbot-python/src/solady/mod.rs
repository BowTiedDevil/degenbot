//! Solady `LibZip` (`FastLZ`) `PyO3` wrappers over the pure `degenbot_core::libzip`
//! core. Mirrors the core surface; no per-domain feature gate (the libzip code
//! lives in `degenbot-core`, which is always a dependency).

pub mod libzip;
