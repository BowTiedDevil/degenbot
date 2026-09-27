//! The priority-fee market percentiles the fee-history oracle polls and the
//! settlement-arbitrage strategy sizes against.
//!
//! `eth_feeHistory` returns one reward sample per requested percentile. The
//! settlement-arbitrage priority-fee sizing clamps the computed fee between the
//! latest block's p10 and p50 samples, and the RPC oracle requests exactly
//! those two percentiles, so the request pair and the clamp bounds are one
//! fact. It lives in this shared foundation layer because the RPC crate must
//! not depend on the engine, and neither should re-declare a pair both reach
//! through this crate.
//!
//! The pair is ordered p10-then-p50; the index constants name the positions in
//! the reward vector `eth_feeHistory` returns.

/// The percentile pair `eth_feeHistory` is polled for, in reward-vector order.
pub const PRIORITY_FEE_PERCENTILES: [u64; 2] = [10, 50];

/// The p10 percentile index in [`PRIORITY_FEE_PERCENTILES`].
pub const P10_INDEX: usize = 0;

/// The p50 percentile index in [`PRIORITY_FEE_PERCENTILES`].
pub const P50_INDEX: usize = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pair_is_ordered_p10_then_p50() {
        assert_eq!(PRIORITY_FEE_PERCENTILES[P10_INDEX], 10);
        assert_eq!(PRIORITY_FEE_PERCENTILES[P50_INDEX], 50);
        assert!(PRIORITY_FEE_PERCENTILES[P10_INDEX] < PRIORITY_FEE_PERCENTILES[P50_INDEX]);
    }
}
