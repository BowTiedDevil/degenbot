#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Tier-2 behavioral dual-driver parity — Aerodrome V2 pool builder identity
//! + state (ADR-005 standalone claim, the behavioral tier; task SSSXG6).
//!
//! The V3 builder parity (`parity_pool_builder.rs`) proves the FFI seam on
//! the pool a `PoolBuilder` emits is lossless for CL pools. This Aerodrome
//! twin does the same for the Aerodrome V2 family: the **same** canonical
//! identity+state fixture, driven through the **Rust consumer**
//! (`BotState`/`register_aerodrome_pool` — the exact `RegisterAerodromeV2PoolParams`
//! `build_aerodrome_v2` produces after its on-chain I/O), registers pool id 1
//! and reports IDENTICAL identity (token0/token1/factory/variant/stable/fee)
//! + state (reserve0/reserve1/update_block) as the fixture records.
//!
//! The matching Python side lives at
//! `tests/standalone_parity/test_aerodrome_builder_dual_driver.py` — it drives
//! the SAME fixture (loaded from the SAME shared JSON file, never copied)
//! through the Python consumer (`PyBot`). Both consumers must agree on the
//! pool id AND every identity/state field; divergence = a lossy FFI seam on
//! the registration/identity path.

#![expect(clippy::doc_markdown)]

use alloy::primitives::{aliases::U112, Address};
use degenbot::bot_core::BotState;
use degenbot::PoolEntry;
use degenbot::RegisterAerodromeV2PoolParams;
use degenbot_uniswap::dex_identity::DexVariant;

/// Path to the shared Aerodrome builder fixture (loaded by both this Rust test
/// and the Python dual-driver test — the single source of truth).
const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/standalone_parity/fixtures/aerodrome_pool_builder.json"
);

/// The shared fixture, deserialized once per test.
#[derive(Debug, serde::Deserialize)]
struct AerodromeFixtureFile {
    fixture: AerodromeFixtureInputs,
    expected: AerodromeExpected,
}

#[derive(Debug, serde::Deserialize)]
struct AerodromeFixtureInputs {
    token0: String,
    token1: String,
    pool: String,
    factory: String,
    variant: String,
    stable: bool,
    fee_numer: u64,
    fee_denom: u64,
    token0_decimals: u8,
    token1_decimals: u8,
    /// Decimal-string big integers (JSON has no u112/u256).
    reserve0: String,
    reserve1: String,
    update_block: u64,
}

#[derive(Debug, serde::Deserialize)]
struct AerodromeExpected {
    pool_id: u64,
}

/// Load + parse the shared fixture. Panics on IO/parse failure (a corrupt
/// fixture is a test-infra failure, not a skip).
fn load_shared_fixture() -> AerodromeFixtureFile {
    let text = std::fs::read_to_string(FIXTURE_PATH)
        .unwrap_or_else(|e| panic!("read shared Aerodrome builder fixture {FIXTURE_PATH}: {e}"));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("parse shared Aerodrome builder fixture {FIXTURE_PATH}: {e}"))
}

#[test]
fn standalone_rust_consumer_aerodrome_builder_identity_state_matches_fixture() {
    // Tier-2 dual-driver gate — Aerodrome V2 (Rust consumer side, pool builder
    // identity + state). Both consumers must register the SAME fixture under
    // the SAME pool id and report IDENTICAL identity + state accessors.
    let fx = load_shared_fixture();

    let token0: Address = fx.fixture.token0.parse().unwrap();
    let token1: Address = fx.fixture.token1.parse().unwrap();
    let pool: Address = fx.fixture.pool.parse().unwrap();
    let factory: Address = fx.fixture.factory.parse().unwrap();
    let reserve0: U112 = fx.fixture.reserve0.parse().unwrap();
    let reserve1: U112 = fx.fixture.reserve1.parse().unwrap();
    let variant = DexVariant::from_kebab(&fx.fixture.variant)
        .unwrap_or_else(|| panic!("unknown variant {}", fx.fixture.variant));

    let mut bot = BotState::new();
    let pid = bot.register_aerodrome_pool(&RegisterAerodromeV2PoolParams {
        address: pool,
        token0,
        token1,
        factory,
        variant,
        stable: fx.fixture.stable,
        fee: (fx.fixture.fee_numer, fx.fixture.fee_denom),
        token0_decimals: fx.fixture.token0_decimals,
        token1_decimals: fx.fixture.token1_decimals,
        reserve0,
        reserve1,
        update_block: fx.fixture.update_block,
    });
    assert_eq!(
        pid, fx.expected.pool_id,
        "pool id must match the shared fixture"
    );

    // Read the registered pool back through the AerodromeV2 projection and
    // assert the full identity + state matches the fixture (the Python side
    // asserts the same values from the same file).
    let entry: &PoolEntry = bot
        .pool_entry(pid)
        .expect("registered pool must be retrievable by id");
    let (aero_id, aero_state) = entry
        .aerodrome_v2()
        .expect("registered pool must be an Aerodrome V2 entry");
    assert_eq!(aero_id.token0, token0, "token0 identity diverged");
    assert_eq!(aero_id.token1, token1, "token1 identity diverged");
    assert_eq!(aero_id.factory, factory, "factory identity diverged");
    assert_eq!(aero_id.variant, variant, "variant identity diverged");
    assert_eq!(aero_id.stable, fx.fixture.stable, "stable flag diverged");
    assert_eq!(
        aero_id.fee,
        (fx.fixture.fee_numer, fx.fixture.fee_denom),
        "fee diverged"
    );
    assert_eq!(
        aero_id.token0_decimals, fx.fixture.token0_decimals,
        "token0_decimals diverged"
    );
    assert_eq!(
        aero_id.token1_decimals, fx.fixture.token1_decimals,
        "token1_decimals diverged"
    );
    assert_eq!(aero_state.reserve0, reserve0, "reserve0 diverged");
    assert_eq!(aero_state.reserve1, reserve1, "reserve1 diverged");
    assert_eq!(
        aero_state.update_block, fx.fixture.update_block,
        "update_block diverged"
    );
}
