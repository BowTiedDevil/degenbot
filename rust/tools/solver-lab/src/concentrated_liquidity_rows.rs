//! Shared concentrated liquidity row parsing for the pure-concentrated-liquidity replay
//! (`concentrated_liquidity_solve_replay`): a captured `hops` array is an array of hops, each
//! hop an array of concentrated liquidity ranges. The mixed replay's rows carry a per-hop
//! V2/`concentrated_liquidity` discriminant instead and keep their own parser.

use crate::capture_rows::range;
use degenbot_pools::state::int_v3_hop::IntV3TickRangeSequence;
use serde_json::Value;

/// Parse a captured `hops` array into one [`IntV3TickRangeSequence`] per hop.
/// The error strings are part of the replays' observable output
/// (`path {pid}: skip ({e})` / `line {n}: parse failure: {e}`), so they are the
/// parser's contract rather than diagnostics.
///
/// # Errors
///
/// Returns `hop not an array`, `empty hop`, a range field error from
/// [`crate::capture_rows::range`], or the sequence constructor's own message.
pub fn parse_concentrated_liquidity_hops(
    hops: &[Value],
) -> Result<Vec<IntV3TickRangeSequence>, String> {
    let mut seqs: Vec<IntV3TickRangeSequence> = Vec::with_capacity(hops.len());
    for hop in hops {
        let ra = hop.as_array().ok_or("hop not an array")?;
        if ra.is_empty() {
            return Err("empty hop".into());
        }
        let ranges = ra.iter().map(range).collect::<Result<Vec<_>, String>>()?;
        seqs.push(IntV3TickRangeSequence::new(ranges)?);
    }
    Ok(seqs)
}
