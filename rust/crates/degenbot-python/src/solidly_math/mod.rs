//! Solidly-stable math `PyO3` wrappers over the `degenbot-solidly-math` pure
//! core.
//!
//! Thin binding layer that extracts Python arguments, calls the pure-Rust math
//! leaf (`degenbot_solidly_math`), and converts results back to Python `int`s.
//! Mirrors the `balancer_math` / `curve_math` binding shape (top-level
//! functions added to the umbrella module via `add_solidly_math_module`). The
//! GIL is held during computation — the Solidly math operations are cheap and
//! never block on I/O.
//!
//! Wraps the nine Solidly / Aerodrome / Camelot stable-pool math functions
//! exposed as the pre-baked orchestration (`calc_exact_in_stable_solidly` /
//! `calc_exact_in_stable_camelot`); the closure-injected Python
//! `calc_exact_in_stable` form is replaced by these two pre-baked variants
//! (one per (`k_func`, `get_y_func`) pair the Aerodrome / Camelot companions
//! actually use).

pub mod lib; // nudge
