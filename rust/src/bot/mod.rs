//! Per-domain binding-crate modules.
//!
//! `engine/` is the first inhabitant; step 6 of the binding-layer reorg
//! relocates the other `py_bot` / `py_liquidity_pool` wrappers alongside.
//! (ergo UG6FKN task WXHGOH.)

pub mod engine;
