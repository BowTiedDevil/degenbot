//! Name-level pin for the flattened per-process dispatch surfaces: the
//! `NonceAuthority` is the sole nonce issuer, so the retired `NonceSource`
//! switch and the dispatcher's private reservation table must not reappear.
//!
//! Textual `include_str!` keeps the check compile-error-free while still
//! catching a re-introduction at the symbol level; the behavioral authority
//! contract lives in the `submit` / `submission_ledger` unit tests.

const SUBMIT_RS: &str = include_str!("../src/submit.rs");
const DISPATCHER_RS: &str = include_str!("../src/dispatcher.rs");
const SUBMISSION_LEDGER_RS: &str = include_str!("../src/submission_ledger.rs");
const BACKRUN_DRIVER_RS: &str = include_str!("../src/backrun_driver.rs");
const BACKRUN_DRIVER_BOOT_RS: &str = include_str!("../src/backrun_driver/driver_boot.rs");
const BACKRUN_DRIVER_LOOP_RS: &str = include_str!("../src/backrun_driver/driver_loop.rs");
const BACKRUN_DRIVER_POLICY_RS: &str = include_str!("../src/backrun_driver/driver_policy.rs");
const BACKRUN_SIDECAR_RS: &str = include_str!("../src/bin/backrun_sidecar.rs");

/// The sign path names exactly one nonce source: a `NonceLane` over the
/// process authority. The `NonceSource` union that also admitted a private
/// dispatcher table is gone.
#[test]
fn the_nonce_source_switch_is_gone() {
    for (file, text) in [
        ("submit.rs", SUBMIT_RS),
        ("dispatcher.rs", DISPATCHER_RS),
        ("submission_ledger.rs", SUBMISSION_LEDGER_RS),
        ("backrun_driver.rs", BACKRUN_DRIVER_RS),
        ("backrun_driver/driver_boot.rs", BACKRUN_DRIVER_BOOT_RS),
        ("backrun_driver/driver_loop.rs", BACKRUN_DRIVER_LOOP_RS),
        ("backrun_driver/driver_policy.rs", BACKRUN_DRIVER_POLICY_RS),
        ("bin/backrun_sidecar.rs", BACKRUN_SIDECAR_RS),
    ] {
        assert!(
            !text.contains("NonceSource"),
            "{file} still names the retired NonceSource switch"
        );
    }
}

/// The dispatcher coordinates pools and monitor tasks only; it owns no nonce
/// table and takes no part in issuance.
#[test]
fn the_dispatcher_owns_no_nonce_table() {
    for token in [
        "claim_nonce",
        "pending_nonces",
        "release_nonce",
        "pending_nonce_count",
    ] {
        assert!(
            !DISPATCHER_RS.contains(token),
            "dispatcher.rs still names the retired nonce table token {token:?}"
        );
    }
    assert!(
        DISPATCHER_RS.contains("Nonce coordination does **not** live here"),
        "dispatcher.rs must document the ownership boundary"
    );
}

/// Every runtime shape stamps through a lane: the submit path takes one, and
/// the standalone sidecar mints one over its own authority.
#[test]
fn every_signing_path_stamps_through_a_nonce_lane() {
    assert!(SUBMIT_RS.contains("Arc<NonceLane>"));
    assert!(SUBMIT_RS.contains("nonce_lane.stamp()"));
    assert!(BACKRUN_SIDECAR_RS.contains("NonceLane::new"));
    assert!(BACKRUN_DRIVER_BOOT_RS.contains("nonce_lane: Arc<NonceLane>"));
    assert!(BACKRUN_DRIVER_LOOP_RS.contains("nonce_lane: Arc<NonceLane>"));
}
