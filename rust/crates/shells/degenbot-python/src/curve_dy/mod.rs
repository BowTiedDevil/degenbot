//! `PyO3` wrappers for the Curve `get_dy` calculator layer.
//!
//! Thin binding layer that extracts Python arguments into a
//! [`DyCalculationInputs`] snapshot, calls the pure Rust core
//! (`degenbot_math::curve::curve_dy_calculator`), and converts the result back
//! to a Python `int`. The I/O orchestration (amp resolution, lending-rate
//! fetch, xp construction) stays in the Python companion; this seam only
//! performs the pure calculation. Mirrors the `curve_math` binding shape.
//!
//! `calculate_dy` is the pure single-entry `get_dy`; `calculate_dy_underlying`
//! additionally delegates base-pool ops through a Python base-pool object.

pub mod lib; // nudge
