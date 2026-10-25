//! The profit-envelope gate door (ADR-084 W2).
//!
//! The boundary types and entry points of the gate, hoisted from the former
//! `solve::profit_envelope` path so callers name
//! [`crate::solve::gate`] — one word, one door. The numeric kernel
//! (`kernel`), the per-hop line derivation (`derive`), and the capture row
//! serializer (`capture`) stay internal to
//! [`crate::solve::profit_envelope`], which remains the module home.

pub use crate::solve::profit_envelope::{
    path_output_bound_at, path_profit_bound, path_profit_bound_with_floor,
    path_profit_bound_with_stats, CapturedHop, ConcentratedLiquidityHop, DegeneratePathEvent,
    DegeneratePathSink, Envelope, GateDeps, GateSkipCause, GateStats, HopMath, Line,
    PathBoundLines, PrefixCache,
};

// Internal plumbing the family solve arms reach through the door module.
#[cfg(test)]
pub(crate) use crate::solve::profit_envelope::path_bound_lines;
pub(crate) use crate::solve::profit_envelope::{
    content_mix_u256, mobius_lines, path_bound_lines_with_stats, record_gate, refuse_lines,
    reserve_cap_hop_lines, v2_hop_lines,
};
