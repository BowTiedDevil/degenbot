//! Pin for the flattened per-process dispatch surface: every signing path

//! stamps through a `NonceLane` over the `NonceAuthority`.
//!
//! Textual `include_str!` keeps the check compile-error-free; the behavioral
//! authority contract lives in the `submit` / `submission_ledger` unit tests.

const SUBMIT_RS: &str = include_str!("../src/submit.rs");

const BACKRUN_DRIVER_BOOT_RS: &str =
    include_str!("../../degenbot-strategy/src/backrun_driver/driver_boot.rs");

const BACKRUN_DRIVER_LOOP_RS: &str =
    include_str!("../../degenbot-strategy/src/backrun_driver/driver_loop.rs");

const HOSTED_BOOT_RS: &str =
    include_str!("../../../shells/degenbot-python/src/bot/engine/strategy.rs");

/// Every signing path stamps through a lane: the submit path takes one, and
/// the hosted one-process boot mints the lanes over its single authority.
#[test]
fn every_signing_path_stamps_through_a_nonce_lane() {
    assert!(SUBMIT_RS.contains("Arc<NonceLane>"));
    assert!(SUBMIT_RS.contains("nonce_lane.stamp()"));
    assert!(HOSTED_BOOT_RS.contains("NonceLane::new"));
    assert!(BACKRUN_DRIVER_BOOT_RS.contains("nonce_lane: Arc<NonceLane>"));
    assert!(BACKRUN_DRIVER_LOOP_RS.contains("nonce_lane: Arc<NonceLane>"));
}
