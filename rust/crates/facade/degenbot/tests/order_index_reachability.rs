//! B5L2XA: the net-profit order index must be reachable from the
//! standalone Rust consumer (the `degenbot` umbrella).
//!
//! ADR-024's deterministic profit-descending pre-sim top-K selection runs
//! inside `degenbot_arbitrage::dispatch_profitable` (no FFI surface), so the
//! consumer-side proof lives at the build/manifest layer: the umbrella both
//! re-exports `degenbot_order_index` AND enables
//! `degenbot-arbitrage/order-index` on its dep edge. This test is the
//! compile-time half (the re-export); the feature-edge half is verified by
//! the `cargo tree -e features` probe in the task's validation gates.

/// The order-index types must be nameable from the umbrella crate root
/// (compile-time reachability — sibling to the scan-based gate in
/// `tests/reachability.rs`).
#[test]
fn order_index_reachable_from_umbrella() {
    fn reachable<T>() {}
    reachable::<Box<dyn degenbot::order_index::OrderIndex<u64>>>();
    reachable::<degenbot::order_index::ScanTopK<u64>>();
    reachable::<degenbot::order_index::EnvelopeIndex<u64>>();
}
