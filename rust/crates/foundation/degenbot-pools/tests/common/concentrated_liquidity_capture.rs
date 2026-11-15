//! Shared capture-row reconstruction for the concentrated-liquidity replay
//! tests (`offline_heavy_replay`, `concentrated_liquidity_golden_replay`).
//!
//! The one fixture parser the pools-side replay tests share: a captured
//! `hops` array is an array of hops, each hop an array of concentrated-
//! liquidity ranges carrying the per-range fields the corpora record. (The
//! solver-lab instruments keep their own parser with tool-owned error
//! strings; the pools tests share this one so reconstruction is never
//! duplicated.)

// The parse mirrors the fixture reader's fatal-by-design convention for the
// golden-arm tests: a malformed range field stops the test rather than
// silently dropping a row out of a correctness check.
#![expect(clippy::expect_used)]

use alloy::primitives::U256;
use degenbot_pools::state::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
use serde_json::Value;

fn u256(s: &str) -> U256 {
    s.trim().parse::<U256>().expect("valid U256")
}

/// Rebuild one [`IntV3TickRangeSequence`] per hop from a captured `hops`
/// array. `None` when any hop or range field is missing or unparsable.
#[must_use]
pub fn parse_capture_hops(hops: &[Value]) -> Option<Vec<IntV3TickRangeSequence>> {
    let mut ranges_per_hop = Vec::with_capacity(hops.len());
    for hop in hops {
        let arr = hop.as_array()?;
        let mut ranges = Vec::with_capacity(arr.len());
        for item in arr {
            let liq: u128 = item.get("liquidity")?.as_str()?.parse().ok()?;
            let wbp: Vec<U256> = item
                .get("word_boundary_prices")?
                .as_array()?
                .iter()
                .filter_map(|w| w.as_str())
                .map(u256)
                .collect();
            ranges.push(IntV3TickRangeHop {
                liquidity: liq,
                sqrt_price_x96: u256(item.get("sqrt_price_x96")?.as_str()?),
                sqrt_price_lower_x96: u256(item.get("sqrt_price_lower_x96")?.as_str()?),
                sqrt_price_upper_x96: u256(item.get("sqrt_price_upper_x96")?.as_str()?),
                gamma_numer: item.get("gamma_numer")?.as_u64()?,
                fee_denom: item.get("fee_denom")?.as_u64()?,
                zero_for_one: item.get("zero_for_one")?.as_bool()?,
                word_boundary_prices: wbp,
            });
        }
        ranges_per_hop.push(IntV3TickRangeSequence::new(ranges).ok()?);
    }
    Some(ranges_per_hop)
}
