//! Invariant suite for the brute-force `ScanTopK` baseline. It must satisfy the
//! same top-K contract as the envelope (it is the independent reference).

mod common;
use common::{check_topk_and_best, point};
use degenbot_order_index::ScanTopK;
use proptest::prelude::*;

proptest! {
    #[test]
    fn topk_matches_brute_force(points in prop::collection::vec(point(), 1..60),
                                x in 0u64..2_000_000_000_000,
                                k in 1usize..8) {
        prop_assert!(check_topk_and_best::<ScanTopK<u64>>(
            &points, alloy_primitives::U256::from(x), k));
    }
}
