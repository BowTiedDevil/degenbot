//! V2 constant-product (x*y=k) swap math `PyO3` wrappers over the `degenbot-v2-math` pure core.
//!
//! Thin binding layer: extract Python ints, call the pure-Rust V2
//! constant-product primitives (`v2_swap_exact_in` / `v2_swap_exact_out`),
//! and convert results back to Python ints. Mirrors the `solidly_math`
//! binding shape. The GIL is held during computation (cheap integer math,
//! no I/O). The volatile V2 swap math (Uniswap V2 family + Aerodrome
//! volatile) used to be a parallel pure-Python Fraction implementation;
//! the companion layer now delegates here so Python and the Rust solver
//! round identically (RH3L24).

pub mod lib; // nudge
