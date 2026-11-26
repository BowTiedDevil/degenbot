//! Shared concentrated liquidity row parsing for the pure-concentrated-liquidity replay
//! (`concentrated_liquidity_solve_replay`): a captured `hops` array is an array of hops, each
//! hop an array of concentrated liquidity ranges. The mixed replay's rows carry a per-hop
//! V2/`concentrated_liquidity` discriminant instead and keep their own parser.

use degenbot_pools::state::int_v3_hop::IntV3TickRangeSequence;
use serde_json::Value;

/// Parse a captured `hops` array into one [`IntV3TickRangeSequence`] per hop.
/// The error strings are part of the replays' observable output
/// (`path {pid}: skip ({e})` / `line {n}: parse failure: {e}`), so they are the
/// parser's contract rather than diagnostics.
///
/// # Errors
///
/// Returns the shared capture parser's hop/range/field error or its sequence
/// constructor error.
pub fn parse_concentrated_liquidity_hops(
    hops: &[Value],
) -> Result<Vec<IntV3TickRangeSequence>, String> {
    degenbot_pools::solve::capture::parse_capture_hops(hops).map_err(|error| error.to_string())
}
