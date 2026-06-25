//! Re-export shim — the pure helpers now live in
//! `crate::bot_core::snapshot_verify` (ADR-006 D4 relocation).
//!
//! This file is kept so existing `solvers::uniswap_engine::snapshot_verify::*`
//! imports continue to resolve. New code should import from
//! `degenbot_bot::bot_core::snapshot_verify` directly.
//!
//! `register_with_cl_buffers` was made generic over the engine type `E`
//! during the move (it never inspects the engine); the concrete
//! `E = UniswapEngine` is supplied at the call sites in
//! `degenbot-python::bot::engine::register`, so existing callers are
//! unaffected.
pub use crate::bot_core::snapshot_verify::{
    register_with_cl_buffers, run_cl_verification, SnapshotStore, VerifyError, VerifyRpc,
};