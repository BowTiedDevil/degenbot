//! ADR-052 D5 fixture matrix: every **released** Alembic revision self-heals
//! to current at open.
//!
//! One fixture per released Alembic head, under `tests/fixtures/alembic_revs/`
//! (provenance in that directory's `README.md`):
//!
//! | fixture | released as |
//! |---|---|
//! | `9347bbfcd47a` | the 0.5-era initial revision |
//! | `756fba1f75f4` | 0.5.0a2 |
//! | `9c411aeeb15e` | 0.5.1b1 |
//! | `b0b9e84d5527` | 0.6.0a1 |
//! | `e0aaad8ad486` | 0.6.0a2 |
//! | `2606a6c7f5ee` | 0.6.0a3+ (the last released Alembic head) |
//!
//! For every fixture this test runs the two halves of the ADR-052 D5 promise:
//!
//! 1. **Auto-heal at open** (the default — `DEGENBOT_DB_AUTO_HEAL` unset):
//!    `DegenbotDb::open` lands `RustOwned` at the current
//!    `RUST_SCHEMA_VERSION`, every head table is present, the seeded rows are
//!    preserved per table, the `alembic_version` stamp is gone and the Rust
//!    stamp table is written, and the pre-heal file survives as `*.bak`
//!    (still stamped at the released revision).
//! 2. **Killswitch** (`DEGENBOT_DB_AUTO_HEAL=0`): the pre-D1 posture persists —
//!    every fixture opens `LegacyAlembic` read-only with no heal and no
//!    `*.bak` (the revision is never inspected; ADR-052 D6 presence detection).
//!
//! The fixture is always copied to a fresh temp dir first: a committed `.db`
//! is never mutated by this suite.
//!
//! Env is process-global, so the per-fixture tests serialize on `ENV_LOCK`
//! and restore `DEGENBOT_DB_AUTO_HEAL` on scope exit (RAII), even on panic.

#![expect(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use rusqlite::{Connection, OpenFlags};
use tempfile::TempDir;

use degenbot_db::schema::RUST_SCHEMA_VERSION;
use degenbot_db::{DegenbotDb, SchemaState, AUTO_HEAL_ENV};

/// Committed fixture directory (never mutated — every case copies out first).
const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/alembic_revs");

/// The private Rust-owned stamp table (not re-exported as a constant).
const RUST_STAMP_TABLE: &str = "_degenbot_db_schema_version";

/// Serializes `DEGENBOT_DB_AUTO_HEAL` mutation across the tests in this binary
/// (env is process-global; cargo runs these tests on parallel threads).
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// RAII env scope: holds the serialization lock for its lifetime and restores
/// `DEGENBOT_DB_AUTO_HEAL` (removes it) on drop, panic included.
struct EnvGuard {
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    /// The default ADR-052 D1 posture: heal-at-open enabled (env unset).
    fn heal_on() -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        std::env::remove_var(AUTO_HEAL_ENV);
        Self { _lock: lock }
    }

    /// The pinned posture: `DEGENBOT_DB_AUTO_HEAL=0` restores the pre-D1
    /// semantics (a legacy-marker DB opens `LegacyAlembic`, unhealed).
    fn killswitch() -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        std::env::set_var(AUTO_HEAL_ENV, "0");
        Self { _lock: lock }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        std::env::remove_var(AUTO_HEAL_ENV);
    }
}

/// Every content table the Rust head schema (`SCHEMA_HEAD`) defines. The
/// healed DB must carry all of them (the D5 "expected table set present").
const HEAD_TABLES: &[&str] = &[
    "aave_gho_tokens",
    "aave_v3_asset_configs",
    "aave_v3_assets",
    "aave_v3_collateral_positions",
    "aave_v3_contracts",
    "aave_v3_debt_positions",
    "aave_v3_emode_categories",
    "aave_v3_markets",
    "aave_v3_user_collateral_configs",
    "aave_v3_users",
    "aerodrome_v2_pools",
    "aerodrome_v3_pools",
    "camelot_v2_pools",
    "erc20_tokens",
    "exchanges",
    "initialization_maps",
    "lfj_pools",
    "liquidity_positions",
    "managed_pool_initialization_maps",
    "managed_pool_liquidity_positions",
    "managed_pools",
    "pancakeswap_v2_pools",
    "pancakeswap_v3_pools",
    "pool_managers",
    "pools",
    "sushiswap_v2_pools",
    "sushiswap_v3_pools",
    "swapbased_v2_pools",
    "uniswap_v2_pools",
    "uniswap_v3_pools",
    "uniswap_v4_pools",
];

/// The synthetic seed every fixture carries (table, row count). These are
/// tables whose head shape is unchanged from the initial revision, so the row
/// transport is the thing under test, not a column-mapping edge.
const SEEDED: &[(&str, i64)] = &[
    ("erc20_tokens", 2),
    ("initialization_maps", 1),
    ("liquidity_positions", 2),
];

fn fixture_path(rev: &str) -> PathBuf {
    Path::new(FIXTURE_DIR).join(format!("{rev}.db"))
}

fn bak_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap().to_owned();
    name.push(".bak");
    path.with_file_name(name)
}

/// Copy a committed fixture into a fresh temp dir. Returns the dir so it stays
/// alive for the test's duration.
fn copy_fixture(rev: &str) -> (TempDir, PathBuf) {
    let src = fixture_path(rev);
    assert!(
        src.exists(),
        "{rev}: committed fixture missing at {}",
        src.display()
    );
    let dir = TempDir::new().unwrap();
    let dst = dir.path().join(src.file_name().unwrap());
    std::fs::copy(&src, &dst).unwrap();
    (dir, dst)
}

fn has_table(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        rusqlite::params![name],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        == 1
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |r| {
        r.get(0)
    })
    .unwrap()
}

fn open_readonly(path: &Path) -> Connection {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

fn stamped_rev(conn: &Connection) -> String {
    conn.query_row("SELECT version_num FROM alembic_version", [], |r| r.get(0))
        .unwrap()
}

/// One fixture, both halves of the D5 promise.
fn run_fixture(rev: &str) {
    // ── 1. Auto-heal at open (default; killswitch unset). ────────────────
    {
        let _env = EnvGuard::heal_on();
        let (_dir, path) = copy_fixture(rev);

        let (db, state) =
            DegenbotDb::open(&path).unwrap_or_else(|e| panic!("{rev}: auto-heal open failed: {e}"));
        match state {
            SchemaState::RustOwned { schema_version } => assert_eq!(
                schema_version, RUST_SCHEMA_VERSION,
                "{rev}: healed DB must be at the current Rust schema version",
            ),
            other => panic!("{rev}: expected RustOwned after auto-heal, got {other:?}"),
        }

        {
            let conn = db.lock();
            // The Alembic stamp is gone; the Rust stamp table is written.
            assert!(
                !has_table(&conn, "alembic_version"),
                "{rev}: alembic_version must be dropped after heal",
            );
            assert!(
                has_table(&conn, RUST_STAMP_TABLE),
                "{rev}: Rust stamp table must be present after heal",
            );
            let stamp: i64 = conn
                .query_row(
                    &format!("SELECT schema_version FROM {RUST_STAMP_TABLE}"),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(stamp, i64::from(RUST_SCHEMA_VERSION), "{rev}: stamp");

            for t in HEAD_TABLES {
                assert!(
                    has_table(&conn, t),
                    "{rev}: healed DB is missing head table {t}",
                );
            }
            for (t, n) in SEEDED {
                assert_eq!(count(&conn, t), *n, "{rev}: seeded row count for {t}");
            }
        }

        // The pre-heal file survives as `.bak`, still stamped at the released
        // revision (the recoverable snapshot ADR-011 promises).
        let bak = bak_path(&path);
        assert!(bak.exists(), "{rev}: .bak must exist after heal");
        let bak_conn = open_readonly(&bak);
        assert_eq!(stamped_rev(&bak_conn), rev, "{rev}: .bak stamp");
        assert!(
            !has_table(&bak_conn, RUST_STAMP_TABLE),
            "{rev}: .bak must be the pre-heal Alembic file",
        );
    }

    // ── 2. Killswitch: the pre-D1 posture must persist. ──────────────────
    {
        let _env = EnvGuard::killswitch();
        let (_dir, path) = copy_fixture(rev);

        // Legacy marker present → opens LegacyAlembic read-only, writes
        // nothing. The revision is never inspected (ADR-052 D6).
        let (db, state) = DegenbotDb::open(&path)
            .unwrap_or_else(|e| panic!("{rev}: killswitch open failed: {e}"));
        assert_eq!(
            state,
            SchemaState::LegacyAlembic,
            "{rev}: must open LegacyAlembic under the killswitch",
        );
        let conn = db.lock();
        assert!(has_table(&conn, "alembic_version"));
        assert!(!has_table(&conn, RUST_STAMP_TABLE));
        drop(conn);

        assert!(
            !bak_path(&path).exists(),
            "{rev}: killswitch must never take a .bak (no heal ran)",
        );
        // The fixture on disk is untouched: still stamped at the revision.
        let conn = open_readonly(&path);
        assert_eq!(
            stamped_rev(&conn),
            rev,
            "{rev}: killswitch left the DB unchanged"
        );
    }
}

/// Generate one `#[test]` per fixture so a failure names the revision.
macro_rules! fixture_case {
    ($name:ident, $rev:literal) => {
        #[test]
        fn $name() {
            run_fixture($rev);
        }
    };
}

fixture_case!(rev_9347bbfcd47a, "9347bbfcd47a");
fixture_case!(rev_756fba1f75f4, "756fba1f75f4");
fixture_case!(rev_9c411aeeb15e, "9c411aeeb15e");
fixture_case!(rev_b0b9e84d5527, "b0b9e84d5527");
fixture_case!(rev_e0aaad8ad486, "e0aaad8ad486");
fixture_case!(rev_2606a6c7f5ee, "2606a6c7f5ee");
