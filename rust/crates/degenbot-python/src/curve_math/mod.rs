//! Curve `StableSwap` math `PyO3` wrappers over the `degenbot-curve-math` pure core.
//!
//! Thin binding layer that extracts Python arguments, calls the pure-Rust math
//! leaf, and converts results back to Python `int`s. Mirrors the `balancer_math`
//! binding shape (top-level functions added to the umbrella module via
//! `add_curve_math_module`). The GIL is held during computation — the Curve
//! math operations are cheap and never block on I/O.
//!
//! Only the five iterative solvers (`stableswap_get_d`/`_y`/`_y_d`/
//! `newton_y`/`reduction_coefficient`) are wrapped — these are the pure-math
//! surface the `DyCalculator` strategy seam invokes (per-variant orchestration
//! + the `CurveDataProvider` I/O fetches stay Python). The variant enums
//! (`DVariant`/`YVariant`/`YDVariant`) cross the seam as `u8` (1-based,
//! matching the Python `auto()` enum values + the Rust `try_from_u8`).

pub mod lib;// nudge
