//! Balancer V2 math `PyO3` wrappers over the `degenbot-balancer-math` pure core.
//!
//! Thin binding layer that extracts Python arguments, calls the pure-Rust
//! math leaf, and converts results back to Python `int`s. Mirrors the
//! `concentrated_liquidity_math` binding shape (the swap-path functions
//! exported on the declarative `balancer_math` `#[pymodule]` in `lib.rs`).
//! The GIL is held during computation — the Balancer math operations are
//! cheap and never block on I/O.

pub mod lib;
