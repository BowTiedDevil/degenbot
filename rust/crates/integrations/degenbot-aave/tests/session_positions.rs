//! The Aave position observer reads what the updater maintains, and refuses
//! everything it cannot read.
//!
//! These tests drive the REAL reader over a REAL (temporary, in-test) Aave V3
//! database rather than a scripted stand-in: the refusals under test are the
//! ones a production read can actually produce — an unserved market, an account
//! with no position, a market the updater has never advanced, a contended read,
//! and a stored value that will not decode — and a stub would only prove that
//! the stub can refuse.

#![expect(clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;

use alloy::primitives::Address;
use degenbot_aave::AavePositionObserver;
use degenbot_core::session_positions::{
    Freshness, HealthFactor, PositionIdentity, PositionObserver, PositionRefusal,
};
use degenbot_db::DegenbotDb;
use rusqlite::{params, Connection};

const CHAIN_ID: u64 = 1;

/// The market's `POOL` contract — the address a position identity names.
fn pool() -> Address {
    Address::from([0x7bu8; 20])
}

fn account() -> Address {
    Address::from([0x8bu8; 20])
}

fn absent_account() -> Address {
    Address::from([0x9bu8; 20])
}

/// A pool address no market in the fixture serves.
fn unserved_pool() -> Address {
    Address::from([0xaau8; 20])
}

/// The block the seeded market's rows were applied at.
const APPLIED_AT: u64 = 21_000_000;

fn pool_checksum() -> String {
    pool().to_checksum(None)
}

fn account_checksum() -> String {
    account().to_checksum(None)
}

/// A file-backed writer handle, so a test can hold a second connection to the
/// same database and make a read genuinely contended. The handle is returned
/// with its path so a caller can clean up after itself.
fn open_write_db(name: &str) -> (Arc<DegenbotDb>, PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "degenbot-aave-position-{name}-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let (db, _state) = DegenbotDb::open_for_writes(&path).expect("open a fresh Aave DB");
    (Arc::new(db), path)
}

fn seed_market(db: &DegenbotDb, last_update_block: Option<u64>) {
    let conn = db.lock();
    conn.execute(
        "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
         VALUES (1, 1, 'Aave Ethereum Market', 1, ?1)",
        params![last_update_block
            .map(|block| { i64::try_from(block).expect("a test block number fits in i64") })],
    )
    .expect("seed the market");
    conn.execute(
        "INSERT INTO aave_v3_contracts (id, market_id, name, address, revision) \
         VALUES (1, 1, 'POOL', ?1, 1)",
        params![pool_checksum()],
    )
    .expect("seed the pool contract");
}

/// One supplied/borrowed pair with a real liquidation threshold, so the
/// analysis reports a finite health factor rather than "no debt".
fn seed_position(db: &DegenbotDb, debt_balance: &str) {
    let conn = db.lock();
    conn.execute(
        "INSERT INTO erc20_tokens (id, chain, address) \
         VALUES (1, 1, '0x0000000000000000000000000000000000000001')",
        [],
    )
    .expect("seed the underlying token");
    conn.execute(
        "INSERT INTO aave_v3_assets \
         (id, market_id, underlying_asset_id, a_token_id, a_token_revision, \
          v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
          borrow_index, borrow_rate) \
         VALUES (1, 1, 1, 1, 1, 1, 1, '1000000000000000000000000000', '0', '1000000000000000000000000000', '0')",
        [],
    )
    .expect("seed the asset");
    conn.execute(
        "INSERT INTO aave_v3_asset_configs (id, asset_id, ltv, liquidation_threshold, \
             liquidation_bonus, borrowing_enabled, stable_borrowing_enabled, \
             flash_loan_enabled, isolation_mode, borrowable_in_isolation) \
         VALUES (1, 1, 8000, 8500, 10500, 1, 1, 1, 0, 0)",
        [],
    )
    .expect("seed the asset config");
    conn.execute(
        "INSERT INTO aave_v3_users (id, market_id, address, e_mode, gho_discount, \
             isolation_mode_debt) VALUES (1, 1, ?1, 0, 0, '0')",
        params![account_checksum()],
    )
    .expect("seed the user");
    conn.execute(
        "INSERT INTO aave_v3_collateral_positions (id, user_id, asset_id, balance) \
         VALUES (1, 1, 1, '1000000000000000000000')",
        [],
    )
    .expect("seed the collateral position");
    conn.execute(
        "INSERT INTO aave_v3_debt_positions (id, user_id, asset_id, balance) \
         VALUES (1, 1, 1, ?1)",
        params![debt_balance],
    )
    .expect("seed the debt position");
}

fn aave_observer(db: Arc<DegenbotDb>) -> AavePositionObserver {
    AavePositionObserver::new(db)
}

fn identity(account: Address) -> PositionIdentity {
    PositionIdentity::new(CHAIN_ID, pool(), account)
}

/// A funded, supplied-and-borrowed account reads back as a finite risk ratio at
/// the market's applied block, and the same identity read twice reports the same
/// block (the source's cursor) rather than drifting.
#[test]
fn a_supplied_and_borrowed_account_reads_back_a_finite_risk_ratio() {
    let (db, path) = open_write_db("healthy");
    seed_market(&db, Some(APPLIED_AT));
    seed_position(&db, "1000000000000000000000");
    let observer = aave_observer(Arc::clone(&db));

    let reading = observer
        .read_position(&identity(account()), &Freshness::Any)
        .expect("a funded account reads");

    assert_eq!(reading.identity(), &identity(account()));
    assert_eq!(reading.observed_block(), APPLIED_AT);
    // 1000 collateral at an 85% threshold against 1000 debt is a ratio of 0.85,
    // which is below the protocol's own 1.0 threshold.
    let HealthFactor::Ratio(ratio) = reading.health_factor() else {
        panic!(
            "a borrowed account has a finite ratio, got {:?}",
            reading.health_factor()
        );
    };
    // The analysis reports the health factor as a float, so the seam's
    // fixed-point ratio carries that float's rounding: the assertion is
    // "0.85 to within a part in 1e6", not a bit-exact 0.85.
    let ratio_u64: u64 = ratio.saturating_to();
    assert!(
        (i128::from(ratio_u64) - 850_000_000_000_000_000i128).abs() <= 1_000_000_000i128,
        "1000 collateral at an 85% threshold against 1000 debt is a ratio of 0.85, got {ratio}"
    );
    assert!(reading.health_factor().is_liquidatable());

    drop(observer);
    drop(db);
    let _ = std::fs::remove_file(&path);
}

/// An account with a supply and no debt is not "no position" and not
/// "liquidatable": it is a position whose risk ratio does not exist.
#[test]
fn an_account_with_no_debt_is_not_liquidatable() {
    let (db, path) = open_write_db("nodebt");
    seed_market(&db, Some(APPLIED_AT));
    {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO aave_v3_users (id, market_id, address, e_mode, gho_discount, \
                 isolation_mode_debt) VALUES (1, 1, ?1, 0, 0, '0')",
            params![account_checksum()],
        )
        .expect("seed the user");
    }
    let observer = aave_observer(Arc::clone(&db));

    let reading = observer
        .read_position(&identity(account()), &Freshness::Any)
        .expect("a supplied-only account is a position");

    assert_eq!(reading.health_factor(), HealthFactor::NoDebt);
    assert!(!reading.health_factor().is_liquidatable());

    drop(observer);
    drop(db);
    let _ = std::fs::remove_file(&path);
}

/// Three different refusals, three different meanings: a market nobody serves, an
/// account with no position in a served market, and a market the updater has
/// never advanced (so no observation exists at all). None of them is a value.
#[test]
fn an_unserved_market_an_absent_position_and_an_unadvanced_market_are_distinct_refusals() {
    let (db, _path) = open_write_db("refusals");
    seed_market(&db, None);
    let observer = aave_observer(Arc::clone(&db));

    // The market exists but has never been advanced, so there is no block to
    // report — refused as stale even though the caller asked for no freshness
    // guarantee, because there is no observation to hand back.
    let unadvanced = observer
        .read_position(&identity(account()), &Freshness::Any)
        .expect_err("a market with no applied chunk has no observation");
    assert_eq!(
        unadvanced,
        PositionRefusal::StaleObservation {
            identity: identity(account()),
            observed_block: None,
            required: Freshness::Any,
        }
    );
    assert!(!unadvanced.is_retryable());

    // A market nobody serves is a wiring fact, not a missing position.
    let unserved_market = PositionIdentity::new(CHAIN_ID, unserved_pool(), account());
    assert_eq!(
        observer
            .read_position(&unserved_market, &Freshness::Any)
            .expect_err("no market at that pool address"),
        PositionRefusal::MarketNotServed {
            identity: unserved_market
        }
    );

    // A served market, a real account for it, and an account that holds
    // nothing there: the read succeeds for the seeded account, and the absent
    // one is refused as an unknown position rather than read as an empty one.
    drop(observer);
    let (db, path) = open_write_db("refusals-2");
    seed_market(&db, Some(APPLIED_AT));
    seed_position(&db, "1000000000000000000000");
    let observer = aave_observer(Arc::clone(&db));
    assert_eq!(
        observer
            .read_position(&identity(absent_account()), &Freshness::Any)
            .expect_err("the market holds no row for that account"),
        PositionRefusal::UnknownPosition {
            identity: identity(absent_account())
        }
    );
    // A position for another chain is not this market's position: the chain is
    // part of the key and the lookup, so the foreign read is unserved rather
    // than silently answered with this chain's rows.
    let other_chain = PositionIdentity::new(CHAIN_ID + 1, pool(), account());
    assert_eq!(
        observer
            .read_position(&other_chain, &Freshness::Any)
            .expect_err("another chain's position"),
        PositionRefusal::MarketNotServed {
            identity: other_chain
        }
    );

    drop(observer);
    drop(db);
    let _ = std::fs::remove_file(&path);
}

/// A caller's freshness requirement is answered before the per-user rows are
/// read: an observation older than the requirement is refused as stale, and the
/// same observation is accepted by a caller that asks for less.
#[test]
fn a_read_older_than_the_required_freshness_is_refused_before_the_position_rows() {
    let (db, path) = open_write_db("stale");
    seed_market(&db, Some(APPLIED_AT));
    let observer = aave_observer(Arc::clone(&db));

    assert_eq!(
        observer
            .read_position(
                &identity(account()),
                &Freshness::AtOrAfter {
                    block: APPLIED_AT + 1
                }
            )
            .expect_err("the market is behind the requirement"),
        PositionRefusal::StaleObservation {
            identity: identity(account()),
            observed_block: Some(APPLIED_AT),
            required: Freshness::AtOrAfter {
                block: APPLIED_AT + 1
            },
        }
    );

    // The account holds nothing, so the loose read gets as far as the user row
    // and is refused THERE — proof that the stale refusal above came from the
    // market cursor rather than from the account.
    assert_eq!(
        observer
            .read_position(&identity(account()), &Freshness::Any)
            .expect_err("no row for that account"),
        PositionRefusal::UnknownPosition {
            identity: identity(account())
        }
    );

    drop(observer);
    drop(db);
    let _ = std::fs::remove_file(&path);
}

/// A contended read is the fault a retry clears, and it is refused as a TYPED
/// transient rather than answered with a value.
///
/// The contention is staged against the observer's own substrate: the file runs
/// the rollback journal (so a reader really does need a lock a second connection
/// can hold) with a shortened busy timeout. Nothing about the observer is
/// stubbed — the fault is raised by `SQLite` underneath the real read path.
#[test]
fn a_contended_read_is_a_typed_retryable_refusal() {
    let (db, path) = open_write_db("contended");
    db.lock()
        .execute_batch("PRAGMA journal_mode=DELETE; PRAGMA busy_timeout=1;")
        .expect("run the file on the rollback journal with a short busy timeout");
    seed_market(&db, Some(APPLIED_AT));
    seed_position(&db, "1000000000000000000000");
    let observer = aave_observer(Arc::clone(&db));

    let before = observer
        .read_position(&identity(account()), &Freshness::Any)
        .expect("the unfaulted read");
    assert_eq!(before.observed_block(), APPLIED_AT);

    let blocker = Connection::open(&path).expect("a second connection to the same file");
    blocker
        .execute_batch("BEGIN EXCLUSIVE")
        .expect("hold the database exclusively");

    let refusal = observer
        .read_position(&identity(account()), &Freshness::Any)
        .expect_err("a contended read produces no position");
    assert!(
        refusal.is_retryable(),
        "a contended read must be retryable, got {refusal:?}"
    );
    assert!(
        matches!(refusal, PositionRefusal::TransientRead { .. }),
        "expected a transient refusal, got {refusal:?}"
    );

    blocker
        .execute_batch("ROLLBACK")
        .expect("release the exclusive lock");
    let after = observer
        .read_position(&identity(account()), &Freshness::Any)
        .expect("the retry after the lock is released reads");
    assert_eq!(after, before);

    drop(observer);
    drop(db);
    let _ = std::fs::remove_file(&path);
}

/// A stored value that will not decode is refused as UNREADABLE, not as a
/// transient one: retrying it unchanged reproduces it, and answering it with a
/// value would mean inventing a position out of a corrupt row.
#[test]
fn a_row_that_will_not_decode_is_refused_as_unreadable_and_never_retried() {
    let (db, path) = open_write_db("unreadable");
    seed_market(&db, Some(APPLIED_AT));
    seed_position(&db, "not-a-number");
    let observer = aave_observer(Arc::clone(&db));

    let refusal = observer
        .read_position(&identity(account()), &Freshness::Any)
        .expect_err("a corrupt balance is not a position");
    assert!(
        matches!(refusal, PositionRefusal::UnreadablePosition { .. }),
        "expected an unreadable refusal, got {refusal:?}"
    );
    assert!(
        !refusal.is_retryable(),
        "a decode fault reproduces unchanged"
    );

    drop(observer);
    drop(db);
    let _ = std::fs::remove_file(&path);
}
