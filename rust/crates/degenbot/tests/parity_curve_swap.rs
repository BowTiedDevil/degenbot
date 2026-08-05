//! Tier-2 behavioral dual-driver parity — Curve `get_dy` (ADR-005 standalone
//! claim, the behavioral tier; epic `TV72EG`, task `SGJR2W`).
//!
//! The Curve StableSwap dy math has no simple closed form (`stableswap_get_y`
//! is a Newton solve), so — like the V3/V4 CL tests — this asserts the direct
//! **FFI-seam-lossless claim**: the **same** canonical fixture driven through
//! the **Rust consumer** (`degenbot_curve_math::calculate_dy`, as a `cargo add
//! degenbot` user) produces the **same** `dy` as the **Python consumer**
//! (`degenbot.curve.dy.calculate_dy`, the PyO3 binding). The shared oracle is
//! the recorded constant in the shared fixture file.
//!
//! ## Fixture (single source of truth — HRT356)
//!
//! Loaded from the SHARED file `tests/standalone_parity/fixtures/curve_swap.json`,
//! which the Python dual-driver test
//! (`tests/standalone_parity/test_curve_swap_dual_driver.py`) ALSO loads. A
//! fixture edit that drifts an expected output fails BOTH sides mechanically.
//!
//! Oracle strength: the seed constants are recorded (the Curve invariant has
//! no closed form), so the parity claim is Rust-consumer == Python-consumer ==
//! recorded constant — the shared-bug-breaking re-derivation is the Tier-3
//! on-chain oracle (SWAP event byte-parity), tracked as a follow-on.

#![allow(clippy::panic_in_result_fn, clippy::doc_markdown)]

use alloy::primitives::U256;
use degenbot_curve_math::curve_dy_calculator::{calculate_dy, DyCalculationInputs};
use degenbot_curve_math::{DVariant, YVariant};

/// Path to the shared Curve fixture (loaded by both this Rust test and the
/// Python dual-driver test — HRT356, the single source of truth).
const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/standalone_parity/fixtures/curve_swap.json"
);

/// One dual-driver probe: a full `DyCalculationInputs` snapshot + (i, j, dx)
/// + the recorded expected dy.
#[derive(Debug, serde::Deserialize)]
struct Probe {
    name: String,
    inputs: ProbeInputs,
    args: ProbeArgs,
    expected: ProbeExpected,
}

#[derive(Debug, serde::Deserialize)]
struct ProbeInputs {
    precision: String,
    fee_denominator: String,
    fee: String,
    n_coins: usize,
    balances: Vec<String>,
    rate_multipliers: Vec<String>,
    precision_multipliers: Vec<String>,
    resolved_rates: Vec<String>,
    xp: Vec<String>,
    amp: String,
    a_precision: String,
    d_variant: u8,
    y_variant: u8,
    swap_style: u8,
    metapool: bool,
    metapool_rate_style: u8,
    metapool_underlying_style: u8,
    virtual_price: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct ProbeArgs {
    i: usize,
    j: usize,
    dx: String,
}

#[derive(Debug, serde::Deserialize)]
struct ProbeExpected {
    dy: String,
}

#[derive(Debug, serde::Deserialize)]
struct FixtureFile {
    probes: Vec<Probe>,
}

/// Load + parse the shared fixture. Panics on any parse/IO failure.
fn load_shared_curve_fixture() -> FixtureFile {
    let text = std::fs::read_to_string(FIXTURE_PATH)
        .unwrap_or_else(|e| panic!("read shared Curve fixture {FIXTURE_PATH}: {e}"));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("parse shared Curve fixture {FIXTURE_PATH}: {e}"))
}

fn u256(s: &str) -> U256 {
    s.parse().unwrap_or_else(|e| panic!("bad u256 {s}: {e}"))
}

fn u256_vec(v: &[String]) -> Vec<U256> {
    v.iter().map(|s| u256(s)).collect()
}

fn build_inputs(p: &ProbeInputs) -> DyCalculationInputs {
    DyCalculationInputs {
        precision: u256(&p.precision),
        fee_denominator: u256(&p.fee_denominator),
        fee: u256(&p.fee),
        n_coins: p.n_coins,
        balances: u256_vec(&p.balances),
        rate_multipliers: u256_vec(&p.rate_multipliers),
        precision_multipliers: u256_vec(&p.precision_multipliers),
        offpeg_fee_multiplier: U256::ZERO,
        fee_gamma: U256::ZERO,
        mid_fee: U256::ZERO,
        out_fee: U256::ZERO,
        address: alloy::primitives::Address::ZERO,
        resolved_rates: u256_vec(&p.resolved_rates),
        xp: u256_vec(&p.xp),
        block_number: 0,
        block_timestamp: 0,
        amp: u256(&p.amp),
        d_variant: DVariant::try_from_u8(p.d_variant).expect("valid d_variant"),
        y_variant: YVariant::try_from_u8(p.y_variant).expect("valid y_variant"),
        a_precision: u256(&p.a_precision),
        swap_style: p.swap_style,
        metapool: p.metapool,
        metapool_rate_style: p.metapool_rate_style,
        metapool_underlying_style: p.metapool_underlying_style,
        d: None,
        gamma: None,
        price_scale: None,
        live_balances: None,
        admin_balances: None,
        effective_balances: None,
        virtual_price: p.virtual_price.as_deref().map(u256),
        scaled_redemption_price: None,
    }
}

#[test]
fn standalone_rust_consumer_curve_dy_matches_recorded_constant() {
    // Tier-2 dual-driver gate — Rust consumer side (Curve get_dy path).
    //
    // A `cargo add degenbot` standalone consumer, driving the canonical Curve
    // fixture (loaded from the shared file), MUST reproduce every recorded
    // `dy`. The Python side drives the SAME file through the PyO3 seam and
    // asserts the same constants. Divergence = a lossy FFI seam on the Curve
    // swap-arg extraction.
    let fx = load_shared_curve_fixture();
    assert!(!fx.probes.is_empty(), "fixture must contain probes");

    for probe in &fx.probes {
        let inputs = build_inputs(&probe.inputs);
        let dx = u256(&probe.args.dx);
        let expected = u256(&probe.expected.dy);
        let dy = calculate_dy(probe.args.i, probe.args.j, dx, &inputs)
            .unwrap_or_else(|e| panic!("{}: calculate_dy failed: {e:?}", probe.name));
        assert_eq!(
            dy, expected,
            "Rust consumer dy mismatch for probe `{}`",
            probe.name
        );
    }
}
