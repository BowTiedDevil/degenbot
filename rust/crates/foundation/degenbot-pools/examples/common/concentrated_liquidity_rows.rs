//! Shared concentrated liquidity row parsing for the pure-concentrated-liquidity replay harnesses
//! (`concentrated_liquidity_solve_replay`, `walkmemo_readout`): a captured `hops` array is an
//! array of hops, each hop an array of concentrated liquidity ranges. The mixed replay's rows
//! carry a per-hop V2/`concentrated_liquidity` discriminant instead and keep their own parser.
//!
//! Included by each pure-concentrated-liquidity example via
//! `#[path = "common/concentrated_liquidity_rows.rs"] mod concentrated_liquidity_rows;`.

use crate::common::range;
use degenbot_pools::int_v3_hop::IntV3TickRangeSequence;
use serde_json::Value;

/// Parse a captured `hops` array into one [`IntV3TickRangeSequence`] per hop.
/// The error strings are part of the replays' observable output
/// (`path {pid}: skip ({e})` / `line {n}: parse failure: {e}`) — identical to
/// the pre-extraction per-example parsers.
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
        seqs.push(IntV3TickRangeSequence { ranges });
    }
    Ok(seqs)
}
