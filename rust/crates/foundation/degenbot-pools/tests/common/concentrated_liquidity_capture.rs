use degenbot_pools::state::int_v3_hop::IntV3TickRangeSequence;
use serde_json::Value;

/// Rebuild sequences with the shared capture parser; this integration harness
/// treats any malformed field as no reconstruction.
#[must_use]
pub fn parse_capture_hops(hops: &[Value]) -> Option<Vec<IntV3TickRangeSequence>> {
    degenbot_pools::solve::capture::parse_capture_hops(hops).ok()
}
