//! Shared capture-JSONL parsing for this package's replay instruments
//! (`concentrated_liquidity_solve_replay`, `mixed_solve_replay`).
//!
//! The module owns row parsing and fixture-path resolution only; error policy
//! (fatal vs skip-and-continue) and the per-harness env knobs stay in each
//! program's `main`. Error strings are part of the replays' observable output,
//! so they are the parsers' contract, not diagnostics.

use alloy::primitives::U256;
use degenbot_pools::state::int_v3_hop::IntV3TickRangeHop;
use serde_json::Value;

/// Parse a captured decimal `u256` string (the capture format serializes
/// `U256` fields as decimal strings).
///
/// # Errors
///
/// Returns the parser's message when `s` is not a decimal integer that fits a
/// `U256`.
pub fn u256(s: &str) -> Result<U256, String> {
    s.trim().parse::<U256>().map_err(|e| e.to_string())
}

/// Read a required string field from a capture row.
///
/// # Errors
///
/// Returns `missing {k}` when the field is absent or is not a JSON string.
pub fn str_field(v: &Value, k: &str) -> Result<String, String> {
    v.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing {k}"))
        .map(String::from)
}

/// Parse one captured concentrated liquidity range into an [`IntV3TickRangeHop`] (decimal-string
/// prices, `u128` liquidity, `u64` gamma/fee, word-boundary price list).
///
/// # Errors
///
/// Returns a message naming the missing or malformed field (`word_boundary_prices`,
/// `liquidity`, `sqrt_price_x96`, `gamma_numer`, `fee_denom`, `zero_for_one`, a
/// non-string word-boundary price, or the integer parser's own message).
pub fn range(v: &Value) -> Result<IntV3TickRangeHop, String> {
    let wbp = v
        .get("word_boundary_prices")
        .and_then(Value::as_array)
        .ok_or("word_boundary_prices")?
        .iter()
        .map(|w| {
            w.as_str()
                .ok_or_else(|| "wbp not a string".to_string())
                .and_then(u256)
        })
        .collect::<Result<Vec<_>, String>>()?;
    let liquidity = str_field(v, "liquidity")?
        .parse::<u128>()
        .map_err(|e| e.to_string())?;
    Ok(IntV3TickRangeHop {
        liquidity,
        sqrt_price_x96: u256(&str_field(v, "sqrt_price_x96")?)?,
        sqrt_price_lower_x96: u256(&str_field(v, "sqrt_price_lower_x96")?)?,
        sqrt_price_upper_x96: u256(&str_field(v, "sqrt_price_upper_x96")?)?,
        gamma_numer: v
            .get("gamma_numer")
            .and_then(Value::as_u64)
            .ok_or("gamma_numer")?,
        fee_denom: v
            .get("fee_denom")
            .and_then(Value::as_u64)
            .ok_or("fee_denom")?,
        zero_for_one: v
            .get("zero_for_one")
            .and_then(Value::as_bool)
            .ok_or("zero_for_one")?,
        word_boundary_prices: wbp,
    })
}

/// Resolve the capture path: CLI arg 1 if present, else the named default
/// fixture resolved through `capture_fixture::fixture_path`.
#[must_use]
pub fn capture_arg(args: &[String], default_fixture: &str) -> String {
    args.get(1).cloned().unwrap_or_else(|| {
        degenbot_pools::solve::fixtures::fixture_path(default_fixture)
            .to_string_lossy()
            .into_owned()
    })
}
