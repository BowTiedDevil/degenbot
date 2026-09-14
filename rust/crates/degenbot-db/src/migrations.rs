//! Rust-owned forward-only schema migrations — the task 6AV4YT verdict
//! mechanism.
//!
//! The schema is Rust-owned (ADR-052). A [`RUST_SCHEMA_VERSION`] bump is
//! advanced at open by applying the pending embedded steps strictly in order
//! under the D2 forward version-lock: each step in its own transaction, a
//! failed step rolls back to the last-good stamp and refuses loudly, a stamp
//! NEWER than the binary refuses with the exact text below, and nothing is
//! ever written ahead of the running binary.
//!
//! # Verdict (spike, 2026-09, ergo 6AV4YT; ADR-052 D3)
//!
//! The default candidate was `rusqlite_migration` 2.6.0 (Apache-2.0, MSRV 1.95
//! < workspace 1.97, `PRAGMA user_version` stamping). It compiles against this
//! workspace's `rusqlite` 0.40 and its behavior was probed against a spike DB —
//! and it was **rejected** in favor of the hand-rolled forward-only loop below.
//! The blockers are recorded here so nobody re-litigates without an upstream
//! API change:
//!
//! 1. **`to_latest` cannot express D2's per-step transaction.** Its `goto_up`
//!    runs every pending step inside one transaction and sets `user_version`
//!    once at the end, so a poisoned step rolls the whole run back. The spike
//!    confirmed a failed step 2 also discarded the committed-good step 1's
//!    table — contradicting "each step its own transaction; a failed step rolls
//!    back to the last-good stamp". Satisfying D2 means bypassing `to_latest`
//!    and looping `to_version(v)` per step, i.e. not using the library's
//!    headline API.
//! 2. **The ahead refusal carries no versions.** `to_latest` returns
//!    `MigrationDefinitionError::DatabaseTooFarAhead`, whose `Display` is a
//!    generic sentence with neither the DB's nor the binary's schema number.
//!    D2 requires the exact "the binary is older than the database
//!    (schema N > binary M)", so the version-lock must read the stamp and
//!    render the refusal itself regardless.
//! 3. **The reconciliation burden is pure transitional churn.** Choosing it
//!    would require a one-shot map of the existing `_degenbot_db_schema_version`
//!    table into `user_version`, plus dual-writing both surfaces through this
//!    release — and the dual-write forces `up_with_hook` per step, which is not
//!    `const`, so the registry could not be a `const &[M]`. The existing table
//!    is already stamped by heal/cutover; keeping it as the single surface is
//!    simpler and needs no reconciliation at all.
//!
//! What remained useful in `rusqlite_migration` was `M::up` (a `&str` wrapper)
//! and ~5-line `PRAGMA user_version` accessors — not worth a new dependency and
//! its lockfile entry. This loop is D2's policy-as-code directly: it reads and
//! writes the existing stamp, applies each step in its own transaction, and
//! renders the exact refusal with both versions.
//!
//! (The pre-existing rejection of `sqlx::migrate!` / `refinery` in
//! [`crate::migrate`] — own tracking table + DDL replay against unknown DBs —
//! was never the blocker for `rusqlite_migration`, which uses `user_version`;
//! the blockers above are. Post-Alembic-retirement that "unknown DB" objection
//! is weaker, but the single-surface argument stands.)
//!
//! # Precondition
//!
//! [`apply_forward_migrations`] assumes the caller has already classified the
//! DB as Rust-owned (post-heal / post-cutover) and is authorized to write — it
//! never sets `query_only`. Version `0` means "no stamp row" (a fresh
//! Rust-owned file); the baseline step (version 1) applies
//! [`SCHEMA_HEAD`].

use rusqlite::{Connection, OptionalExtension};

use crate::error::DbError;
use crate::schema::{RUST_SCHEMA_VERSION, SCHEMA_HEAD, SCHEMA_VERSION_TABLE};

/// One forward-only Rust-owned schema step.
///
/// Step `version` is the schema version the step *produces* (the version stamped
/// after it commits). The production registry is strictly ascending and
/// contiguous from `1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationStep {
    /// The schema version this step produces.
    pub version: u32,
    /// Stable label for logs and the failure error.
    pub name: &'static str,
    /// The step's SQL, executed inside its own transaction.
    pub sql: &'static str,
}

/// The production forward-only registry.
///
/// Step 1 is the consolidated baseline ([`SCHEMA_HEAD`], the same DDL the
/// fresh-standalone path applies); a future [`RUST_SCHEMA_VERSION`] bump appends
/// the next step here. `RUST_MIGRATIONS.last().version == RUST_SCHEMA_VERSION`
/// is asserted in this module's tests.
pub const RUST_MIGRATIONS: &[MigrationStep] = &[MigrationStep {
    version: 1,
    name: "baseline",
    sql: SCHEMA_HEAD,
}];

/// What [`apply_forward_migrations`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationOutcome {
    /// The stamp already equaled the target — nothing was applied (idempotent
    /// second open).
    AlreadyCurrent {
        /// The current stamp.
        schema_version: u32,
    },
    /// Pending steps were applied in order, each in its own transaction.
    Applied {
        /// The stamp before the run (`0` for a fresh file).
        from: u32,
        /// The stamp after the run (`== target`).
        to: u32,
    },
}

/// Apply the pending forward-only steps up to `target`, under the ADR-052 D2
/// version-lock (see the module docs for the mechanism verdict).
///
/// Steps are selected by [`MigrationStep::version`] for every version in
/// `(stamp, target]`, applied in ascending order, each inside its own
/// transaction with the stamp advanced before the commit. A step failure rolls
/// back only that step and leaves the stamp at the last-good version.
///
/// # Errors
///
/// - [`DbError::SchemaAhead`] when the stamp is NEWER than `target` (the
///   binary is older than the database) — nothing is written;
/// - [`DbError::MissingMigrationStep`] when the registry has a gap;
/// - [`DbError::MigrationStepFailed`] when a step's SQL fails;
/// - [`DbError::Sqlite`] on a connection/query failure.
pub fn apply_forward_migrations(
    conn: &Connection,
    steps: &[MigrationStep],
    target: u32,
) -> Result<MigrationOutcome, DbError> {
    let current = read_schema_version(conn)?;

    if current > target {
        // Refuse-newer is a hard halt (ADR-052 D2) — never a warning, and
        // critically never a write-ahead. `read_schema_version` wrote nothing.
        return Err(DbError::SchemaAhead {
            db: current,
            binary: target,
        });
    }
    if current == target {
        return Ok(MigrationOutcome::AlreadyCurrent {
            schema_version: current,
        });
    }

    for version in (current + 1)..=target {
        let step = steps
            .iter()
            .find(|s| s.version == version)
            .ok_or(DbError::MissingMigrationStep { version })?;
        // `unchecked_transaction` borrows `&Connection`; the tx is rolled back
        // if it is dropped without committing (error path below).
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(step.sql)
            .map_err(|cause| DbError::MigrationStepFailed {
                version,
                name: step.name,
                at: version - 1,
                cause,
            })?;
        write_schema_version(&tx, version)?;
        tx.commit()?;
    }

    Ok(MigrationOutcome::Applied {
        from: current,
        to: target,
    })
}

/// Apply the production [`RUST_MIGRATIONS`] registry up to
/// [`RUST_SCHEMA_VERSION`]. The convenience entry point the open path
/// (`ensure_schema`, task IOGST2) will call once the version-lock is wired.
///
/// # Errors
///
/// As [`apply_forward_migrations`].
pub fn apply_rust_migrations(conn: &Connection) -> Result<MigrationOutcome, DbError> {
    apply_forward_migrations(conn, RUST_MIGRATIONS, RUST_SCHEMA_VERSION)
}

/// Read the [`SCHEMA_VERSION_TABLE`] stamp, `0` when the table (or its row) is
/// absent — i.e. a fresh Rust-owned file that has not been stamped yet.
fn read_schema_version(conn: &Connection) -> Result<u32, DbError> {
    if !table_exists(conn, SCHEMA_VERSION_TABLE)? {
        return Ok(0);
    }
    let raw: Option<i64> = conn
        .query_row(
            &format!("SELECT schema_version FROM {SCHEMA_VERSION_TABLE} LIMIT 1"),
            [],
            |row| row.get(0),
        )
        .optional()?;
    match raw {
        None => Ok(0),
        Some(v) => u32::try_from(v)
            .map_err(|_| DbError::Decode(format!("schema version out of range: {v}"))),
    }
}

/// Create the stamp table if absent and set it to `version` (single-row
/// `DELETE` + `INSERT`, mirroring [`crate::migrate`]'s private stamper).
fn write_schema_version(conn: &Connection, version: u32) -> Result<(), DbError> {
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {SCHEMA_VERSION_TABLE} (schema_version INTEGER NOT NULL);\n\
         DELETE FROM {SCHEMA_VERSION_TABLE};"
    ))?;
    conn.execute(
        &format!("INSERT INTO {SCHEMA_VERSION_TABLE} (schema_version) VALUES (?1)"),
        rusqlite::params![i64::from(version)],
    )?;
    Ok(())
}

/// Returns `true` if a table named `name` exists in `conn`'s `sqlite_master`.
fn table_exists(conn: &Connection, name: &str) -> Result<bool, DbError> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        rusqlite::params![name],
        |row| row.get(0),
    )?;
    Ok(exists > 0)
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::schema::table;

    /// Synthetic three-step registry. Step 3 inserts into the table step 2
    /// creates, so a correct run proves ordering (step 3 would fail if applied
    /// before step 2).
    const SYNTH: &[MigrationStep] = &[
        MigrationStep {
            version: 1,
            name: "base",
            sql: "CREATE TABLE base (x INTEGER);",
        },
        MigrationStep {
            version: 2,
            name: "add_b",
            sql: "CREATE TABLE b (x INTEGER);",
        },
        MigrationStep {
            version: 3,
            name: "seed_b",
            sql: "INSERT INTO b (x) VALUES (42);",
        },
    ];

    fn stamp(conn: &Connection) -> u32 {
        read_schema_version(conn).unwrap()
    }

    fn seed_at_one(conn: &Connection) {
        conn.execute_batch("CREATE TABLE base (x INTEGER);")
            .unwrap();
        write_schema_version(conn, 1).unwrap();
    }

    #[test]
    fn fresh_applies_all_steps_and_stamps_target() {
        let conn = Connection::open_in_memory().unwrap();
        let out = apply_forward_migrations(&conn, SYNTH, 3).unwrap();
        assert_eq!(out, MigrationOutcome::Applied { from: 0, to: 3 });
        assert_eq!(stamp(&conn), 3);
        for t in ["base", "b"] {
            assert!(table_exists(&conn, t).unwrap(), "{t} should exist");
        }
        let seeded: i64 = conn
            .query_row("SELECT COUNT(*) FROM b", [], |r| r.get(0))
            .unwrap();
        assert_eq!(seeded, 1, "step 3 ran after step 2 created b");
    }

    #[test]
    fn behind_applies_pending_steps_in_order() {
        let conn = Connection::open_in_memory().unwrap();
        seed_at_one(&conn);
        let out = apply_forward_migrations(&conn, SYNTH, 3).unwrap();
        assert_eq!(out, MigrationOutcome::Applied { from: 1, to: 3 });
        assert_eq!(stamp(&conn), 3);
        // ordering proof: the seed row exists, so step 2 preceded step 3
        let seeded: i64 = conn
            .query_row("SELECT COUNT(*) FROM b", [], |r| r.get(0))
            .unwrap();
        assert_eq!(seeded, 1);
    }

    #[test]
    fn second_open_applies_nothing() {
        let conn = Connection::open_in_memory().unwrap();
        apply_forward_migrations(&conn, SYNTH, 3).unwrap();
        let out = apply_forward_migrations(&conn, SYNTH, 3).unwrap();
        assert_eq!(out, MigrationOutcome::AlreadyCurrent { schema_version: 3 });
        // idempotent: no duplicate seed row
        let seeded: i64 = conn
            .query_row("SELECT COUNT(*) FROM b", [], |r| r.get(0))
            .unwrap();
        assert_eq!(seeded, 1);
    }

    #[test]
    fn ahead_stamp_refuses_with_exact_text_and_writes_nothing() {
        let conn = Connection::open_in_memory().unwrap();
        seed_at_one(&conn);
        write_schema_version(&conn, 5).unwrap();

        let err = apply_forward_migrations(&conn, SYNTH, 3).unwrap_err();
        assert_eq!(
            format!("{err}"),
            "the binary is older than the database (schema 5 > binary 3)"
        );
        // no silent write-ahead: the ahead stamp is untouched
        assert_eq!(stamp(&conn), 5);
    }

    #[test]
    fn failed_step_rolls_back_to_last_good_stamp() {
        const POISON: &[MigrationStep] = &[
            MigrationStep {
                version: 1,
                name: "base",
                sql: "CREATE TABLE base (x INTEGER);",
            },
            MigrationStep {
                version: 2,
                name: "add_b",
                sql: "CREATE TABLE b (x INTEGER);",
            },
            MigrationStep {
                version: 3,
                name: "poison",
                sql: "SYNTAX ERROR;",
            },
        ];
        let conn = Connection::open_in_memory().unwrap();
        seed_at_one(&conn);

        let err = apply_forward_migrations(&conn, POISON, 3).unwrap_err();
        match err {
            DbError::MigrationStepFailed {
                version, name, at, ..
            } => {
                assert_eq!(version, 3);
                assert_eq!(name, "poison");
                assert_eq!(at, 2, "error names the last-good stamp");
            }
            other => panic!("expected MigrationStepFailed, got {other:?}"),
        }
        // step 2 committed (its own transaction), step 3 rolled back, stamp at 2
        assert_eq!(stamp(&conn), 2);
        assert!(table_exists(&conn, "b").unwrap());
        let seeded: i64 = conn
            .query_row("SELECT COUNT(*) FROM b", [], |r| r.get(0))
            .unwrap();
        assert_eq!(seeded, 0, "poisoned step left no row");
    }

    #[test]
    fn missing_registry_step_refuses() {
        // registry skips version 2 → the gap must be refused, not skipped
        const GAPPY: &[MigrationStep] = &[
            MigrationStep {
                version: 1,
                name: "base",
                sql: "CREATE TABLE base (x INTEGER);",
            },
            MigrationStep {
                version: 3,
                name: "c",
                sql: "CREATE TABLE c (x INTEGER);",
            },
        ];
        let conn = Connection::open_in_memory().unwrap();
        let err = apply_forward_migrations(&conn, GAPPY, 3).unwrap_err();
        assert!(matches!(err, DbError::MissingMigrationStep { version: 2 }));
        // step 1 committed, stamp at 1 — no silent gap-jump
        assert_eq!(stamp(&conn), 1);
    }

    #[test]
    fn production_registry_is_contiguous_and_matches_const() {
        for (i, step) in RUST_MIGRATIONS.iter().enumerate() {
            assert_eq!(
                step.version,
                u32::try_from(i + 1).unwrap(),
                "registry must be contiguous from 1"
            );
        }
        assert_eq!(
            RUST_MIGRATIONS.last().unwrap().version,
            RUST_SCHEMA_VERSION,
            "production registry tip must equal RUST_SCHEMA_VERSION"
        );
    }

    #[test]
    fn apply_rust_migrations_creates_full_schema_at_current() {
        let conn = Connection::open_in_memory().unwrap();
        let out = apply_rust_migrations(&conn).unwrap();
        assert_eq!(
            out,
            MigrationOutcome::Applied {
                from: 0,
                to: RUST_SCHEMA_VERSION
            }
        );
        assert_eq!(stamp(&conn), RUST_SCHEMA_VERSION);
        for t in [
            table::EXCHANGES,
            table::ERC20_TOKENS,
            table::POOLS,
            table::LIQUIDITY_POSITIONS,
        ] {
            assert!(table_exists(&conn, t).unwrap(), "{t} should exist");
        }
        // second open is a no-op
        assert_eq!(
            apply_rust_migrations(&conn).unwrap(),
            MigrationOutcome::AlreadyCurrent {
                schema_version: RUST_SCHEMA_VERSION
            }
        );
    }
}
