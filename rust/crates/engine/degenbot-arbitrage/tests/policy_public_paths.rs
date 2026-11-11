//! The pre-simulation policy's leaf helpers resolve at BOTH published paths.
//!
//! `candidate_is_stale`, `filter_thin_margin_results`, `BPS_DENOM`, and
//! `MAX_SIMULATE_CONCURRENT` moved into the crate-internal `admission` module.
//! The `degenbot_arbitrage::dispatch::*` paths they were published at stay
//! valid, and so do the crate-root paths. This target is an EXTERNAL crate, so a
//! path that stops resolving is a compile error here rather than a silent break
//! for a `cargo add degenbot-arbitrage` consumer.

use degenbot_arbitrage::dispatch::{
    candidate_is_stale as candidate_is_stale_at_dispatch,
    filter_thin_margin_results as filter_thin_margin_results_at_dispatch, BPS_DENOM,
    MAX_SIMULATE_CONCURRENT,
};
use degenbot_arbitrage::{candidate_is_stale, filter_thin_margin_results, DispatchCandidate};
use degenbot_execution::{CandidateId, DeploymentId, PathInfo};
use degenbot_substrate::BotState;

/// A hopless row: no hop to resolve, so nothing about it is stale.
fn hopless_candidate() -> DispatchCandidate {
    DispatchCandidate {
        candidate_id: CandidateId::new(7, DeploymentId::new(1)),
        optimal_input: 1_000_000,
        engine_profit: 5_000,
        steps: Box::new([]),
        solve_block: 100,
        path_info: PathInfo::new(Vec::new()),
        prepared: None,
    }
}

#[test]
fn the_thin_margin_leaf_resolves_at_both_public_paths() {
    // Empty input short-circuits at any floor: nothing kept, nothing dropped —
    // asserted through each path so both are genuinely called, not just named.
    let (kept, dropped) = filter_thin_margin_results(Vec::new(), 50);
    assert!(kept.is_empty(), "crate-root path");
    assert_eq!(dropped, 0, "crate-root path");

    let (kept, dropped) = filter_thin_margin_results_at_dispatch(Vec::new(), 50);
    assert!(kept.is_empty(), "dispatch-module path");
    assert_eq!(dropped, 0, "dispatch-module path");
}

#[test]
fn the_staleness_leaf_resolves_at_both_public_paths() {
    let core = BotState::new();
    let candidate = hopless_candidate();
    assert!(!candidate_is_stale(&core, &candidate), "crate-root path");
    assert!(
        !candidate_is_stale_at_dispatch(&core, &candidate),
        "dispatch-module path"
    );
}

#[test]
fn the_policy_constants_are_one_value_at_both_public_paths() {
    assert_eq!(BPS_DENOM, degenbot_arbitrage::BPS_DENOM);
    assert_eq!(
        MAX_SIMULATE_CONCURRENT,
        degenbot_arbitrage::MAX_SIMULATE_CONCURRENT
    );
}
