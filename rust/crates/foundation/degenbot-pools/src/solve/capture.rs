use alloy::primitives::U256;
use serde_json::Value;

use crate::state::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureError {
    pub hop: usize,
    pub range: Option<usize>,
    pub field: &'static str,
    pub reason: String,
}

impl CaptureError {
    fn new(
        hop: usize,
        range: Option<usize>,
        field: &'static str,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            hop,
            range,
            field,
            reason: reason.into(),
        }
    }
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(range) = self.range {
            write!(
                f,
                "hop {} range {range} field {}: {}",
                self.hop, self.field, self.reason
            )
        } else {
            write!(f, "hop {} field {}: {}", self.hop, self.field, self.reason)
        }
    }
}

impl std::error::Error for CaptureError {}

pub fn parse_capture_hops(hops: &[Value]) -> Result<Vec<IntV3TickRangeSequence>, CaptureError> {
    let mut sequences = Vec::with_capacity(hops.len());
    for (hop_index, hop) in hops.iter().enumerate() {
        let ranges = hop
            .as_array()
            .ok_or_else(|| CaptureError::new(hop_index, None, "hops", "not an array"))?;
        if ranges.is_empty() {
            return Err(CaptureError::new(hop_index, None, "hops", "empty hop"));
        }
        let ranges = ranges
            .iter()
            .enumerate()
            .map(|(range_index, value)| parse_capture_range(value, hop_index, range_index))
            .collect::<Result<Vec<_>, _>>()?;
        let sequence = IntV3TickRangeSequence::new(ranges)
            .map_err(|error| CaptureError::new(hop_index, None, "sequence", error))?;
        sequences.push(sequence);
    }
    Ok(sequences)
}

pub fn parse_capture_range(
    value: &Value,
    hop: usize,
    range: usize,
) -> Result<IntV3TickRangeHop, CaptureError> {
    let liquidity = required_string(value, "liquidity", hop, range)?
        .trim()
        .parse::<u128>()
        .map_err(|error| CaptureError::new(hop, Some(range), "liquidity", error.to_string()))?;
    let word_boundary_prices = value
        .get("word_boundary_prices")
        .and_then(Value::as_array)
        .ok_or_else(|| CaptureError::new(hop, Some(range), "word_boundary_prices", "not an array"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| {
                    CaptureError::new(hop, Some(range), "word_boundary_prices", "not a string")
                })
                .and_then(|text| parse_u256(text, hop, range, "word_boundary_prices"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(IntV3TickRangeHop {
        liquidity,
        sqrt_price_x96: parse_u256(
            required_string(value, "sqrt_price_x96", hop, range)?,
            hop,
            range,
            "sqrt_price_x96",
        )?,
        sqrt_price_lower_x96: parse_u256(
            required_string(value, "sqrt_price_lower_x96", hop, range)?,
            hop,
            range,
            "sqrt_price_lower_x96",
        )?,
        sqrt_price_upper_x96: parse_u256(
            required_string(value, "sqrt_price_upper_x96", hop, range)?,
            hop,
            range,
            "sqrt_price_upper_x96",
        )?,
        gamma_numer: required_u64(value, "gamma_numer", hop, range)?,
        fee_denom: required_u64(value, "fee_denom", hop, range)?,
        zero_for_one: value
            .get("zero_for_one")
            .and_then(Value::as_bool)
            .ok_or_else(|| CaptureError::new(hop, Some(range), "zero_for_one", "not a bool"))?,
        word_boundary_prices,
    })
}

fn required_string<'a>(
    value: &'a Value,
    field: &'static str,
    hop: usize,
    range: usize,
) -> Result<&'a str, CaptureError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| CaptureError::new(hop, Some(range), field, "not a string"))
}

fn required_u64(
    value: &Value,
    field: &'static str,
    hop: usize,
    range: usize,
) -> Result<u64, CaptureError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| CaptureError::new(hop, Some(range), field, "not a u64"))
}

fn parse_u256(
    text: &str,
    hop: usize,
    range: usize,
    field: &'static str,
) -> Result<U256, CaptureError> {
    text.trim()
        .parse::<U256>()
        .map_err(|error| CaptureError::new(hop, Some(range), field, error.to_string()))
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn range() -> Value {
        json!({
            "liquidity": "1",
            "sqrt_price_x96": "1",
            "sqrt_price_lower_x96": "1",
            "sqrt_price_upper_x96": "1",
            "gamma_numer": 997_000,
            "fee_denom": 1_000_000,
            "zero_for_one": true,
            "word_boundary_prices": []
        })
    }

    #[test]
    fn parses_one_capture_hop() {
        let sequences = parse_capture_hops(&[json!([range()])]).unwrap();
        assert_eq!(sequences.len(), 1);
        assert_eq!(sequences[0].ranges.len(), 1);
    }

    #[test]
    fn reports_hop_range_and_missing_field() {
        let mut value = range();
        value.as_object_mut().unwrap().remove("liquidity");
        let error = parse_capture_range(&value, 2, 3).unwrap_err();
        assert_eq!(error.hop, 2);
        assert_eq!(error.range, Some(3));
        assert_eq!(error.field, "liquidity");
    }

    #[test]
    fn reports_invalid_u256_field() {
        let mut value = range();
        value["sqrt_price_x96"] = json!("not-a-number");
        let error = parse_capture_range(&value, 0, 1).unwrap_err();
        assert_eq!(error.field, "sqrt_price_x96");
    }

    #[test]
    fn rejects_non_array_and_empty_hops() {
        assert_eq!(parse_capture_hops(&[json!(42)]).unwrap_err().field, "hops");
        assert_eq!(parse_capture_hops(&[json!([])]).unwrap_err().field, "hops");
    }

    #[test]
    fn rejects_non_string_word_boundary_price() {
        let mut value = range();
        value["word_boundary_prices"] = json!([1]);
        let error = parse_capture_range(&value, 0, 0).unwrap_err();
        assert_eq!(error.field, "word_boundary_prices");
    }
}
