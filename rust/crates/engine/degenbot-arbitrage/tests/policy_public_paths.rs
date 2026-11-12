//! The pre-simulation policy's vocabulary resolves at every published path.
//!
//! `candidate_is_stale`, `filter_thin_margin_results`, `BPS_DENOM`, and
//! `MAX_SIMULATE_CONCURRENT` live in the crate-internal `admission` module, and
//! `DispatchCandidate` / `SolveStep` live in the `candidate` module. The
//! `degenbot_arbitrage::dispatch::*` paths they were published at stay valid,
//! `degenbot_arbitrage::candidate::*` names them directly, and so do the
//! crate-root paths. This target is an EXTERNAL crate, so a path that stops
//! resolving is a compile error here rather than a silent break for a
//! `cargo add degenbot-arbitrage` consumer.

use degenbot_arbitrage::candidate::{
    DispatchCandidate as CandidateAtCandidate, SolveStep as SolveStepAtCandidate,
};
use degenbot_arbitrage::dispatch::{
    candidate_is_stale as candidate_is_stale_at_dispatch,
    filter_thin_margin_results as filter_thin_margin_results_at_dispatch,
    DispatchCandidate as CandidateAtDispatch, SolveStep as SolveStepAtDispatch, BPS_DENOM,
    MAX_SIMULATE_CONCURRENT,
};
use degenbot_arbitrage::{
    candidate_is_stale, filter_thin_margin_results, DispatchCandidate, SolveStep,
};
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

/// The candidate vocabulary resolves at every published path — the neutral
/// `candidate` module, the historical `dispatch` module, and the crate root —
/// and a value built through one path IS the value the others name, so the
/// three spellings are one type rather than three.
#[test]
fn the_candidate_vocabulary_resolves_at_every_published_path() {
    let step_at_candidate: SolveStepAtCandidate = SolveStep {
        output: 10,
        consumed_input: 9,
        state_nonce: 3,
    };
    let step_at_dispatch: SolveStepAtDispatch = step_at_candidate;
    let step_at_root: SolveStep = step_at_dispatch;
    assert_eq!(step_at_root.state_nonce, 3, "one per-hop row type");

    let row_at_candidate: CandidateAtCandidate = hopless_candidate();
    let row_at_dispatch: CandidateAtDispatch = row_at_candidate;
    let row_at_root: DispatchCandidate = row_at_dispatch;
    assert_eq!(row_at_root.steps.len(), 0);
    assert!(
        row_at_root.expected_outputs_vec().is_empty(),
        "the shared type's accessor keeps resolving"
    );
}
