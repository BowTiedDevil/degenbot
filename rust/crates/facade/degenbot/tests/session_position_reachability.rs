//! A pure-Rust consumer installs the Aave position reader on a session and
//! reads a position — through the umbrella crate alone.
//!
//! Every name below is reached as `degenbot::…`, so this is also the reach
//! proof for the re-exports: remove `AavePositionObserver` or the position types
//! from the umbrella and this stops compiling, which is the point of pinning the
//! consumer path rather than the crate-internal one.
//!
//! It is also the pure-Rust consumer's own wiring site, and the reason the
//! umbrella has no install of its own: it has no Aave updater, so it has no
//! database handle and no session to bind against. A consumer that runs the
//! updater composes its session and installs its reader in the two lines below,
//! at its own composition point — the same two lines the Python-driven bot's
//! boot performs. See `docs/architecture/session-object-registry.md`, *Who
//! installs it, and where*.

#![expect(clippy::expect_used, reason = "test assertions fail loudly")]

use std::sync::Arc;

use alloy::primitives::Address;
use degenbot::{
    aave::AavePositionObserver, db::DegenbotDb, Freshness, HealthFactor, PositionRefusal,
    SessionObjectRegistry,
};

const CHAIN_ID: u64 = 1;
const APPLIED_AT: u64 = 21_000_000;

fn market() -> Address {
    Address::from([0x7bu8; 20])
}

fn account() -> Address {
    Address::from([0x8bu8; 20])
}

/// The consumer's whole wiring: a session, a database handle, one install, and a
/// read whose freshness requirement the caller states.
#[test]
fn a_consumer_installs_the_aave_position_reader_on_a_session_and_reads() {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().expect("open an Aave DB");
    let db = Arc::new(db);
    seed_position(&db);

    let session = SessionObjectRegistry::new(CHAIN_ID);
    let identity = session.position_identity(market(), account());

    // Before the install the session has no reader, and says so rather than
    // answering with a position nobody read.
    assert!(!session.has_position_observer());
    assert_eq!(
        session
            .read_position(&identity, &Freshness::Any)
            .expect_err("no reader yet"),
        PositionRefusal::NoPositionOwner
    );

    assert!(
        session
            .install_position_observer(Arc::new(AavePositionObserver::new(Arc::clone(&db))))
            .is_ok(),
        "the first position observer installed wins"
    );

    let reading = session
        .read_position(
            &identity,
            &Freshness::AtMost {
                max_age: 10,
                head: APPLIED_AT,
            },
        )
        .expect("the consumer reads its position");

    assert_eq!(reading.identity(), &identity);
    assert_eq!(reading.observed_block(), APPLIED_AT);
    assert!(
        matches!(reading.health_factor(), HealthFactor::Ratio(_)),
        "a borrowed account has a finite ratio, got {:?}",
        reading.health_factor()
    );

    // The same read under a requirement the market cannot satisfy is refused as
    // stale, from the consumer's side, with the block it actually observed.
    assert_eq!(
        session
            .read_position(
                &identity,
                &Freshness::AtOrAfter {
                    block: APPLIED_AT + 1
                }
            )
            .expect_err("the market is behind the requirement"),
        PositionRefusal::StaleObservation {
            identity,
            observed_block: Some(APPLIED_AT),
            required: Freshness::AtOrAfter {
                block: APPLIED_AT + 1
            },
        }
    );
}

/// One market, one supplied-and-borrowed account, applied at `APPLIED_AT`.
/// Written through the handle the database crate exposes, so the consumer path
/// needs no dependency of its own beyond the umbrella.
fn seed_position(db: &DegenbotDb) {
    let user = account().to_checksum(None);
    let conn = db.lock();
    conn.execute_batch(&format!(
        "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
         VALUES (1, 1, 'Aave Ethereum Market', 1, {APPLIED_AT}); \
         INSERT INTO erc20_tokens (id, chain, address) \
         VALUES (1, 1, '0x0000000000000000000000000000000000000001'); \
         INSERT INTO aave_v3_assets \
         (id, market_id, underlying_asset_id, a_token_id, a_token_revision, \
          v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
          borrow_index, borrow_rate) \
         VALUES (1, 1, 1, 1, 1, 1, 1, '1000000000000000000000000000', '0', \
                 '1000000000000000000000000000', '0'); \
         INSERT INTO aave_v3_asset_configs (id, asset_id, ltv, liquidation_threshold, \
              liquidation_bonus, borrowing_enabled, stable_borrowing_enabled, \
              flash_loan_enabled, isolation_mode, borrowable_in_isolation) \
         VALUES (1, 1, 8000, 8500, 10500, 1, 1, 1, 0, 0); \
         INSERT INTO aave_v3_users (id, market_id, address, e_mode, gho_discount, \
              isolation_mode_debt) VALUES (1, 1, '{user}', 0, 0, '0'); \
         INSERT INTO aave_v3_collateral_positions (id, user_id, asset_id, balance) \
         VALUES (1, 1, 1, '1000000000000000000000'); \
         INSERT INTO aave_v3_debt_positions (id, user_id, asset_id, balance) \
         VALUES (1, 1, 1, '1000000000000000000000');"
    ))
    .expect("seed the Aave market and position rows");
    conn.execute(
        "INSERT INTO aave_v3_contracts (id, market_id, name, address, revision) \
         VALUES (1, 1, 'POOL', ?1, 1)",
        [market().to_checksum(None)],
    )
    .expect("seed the pool contract");
}
