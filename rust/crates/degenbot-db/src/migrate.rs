//! The Alembic-aware migration runner.
//!
//! The HARD REQUIREMENT (from the SLHSM4 binding): open an existing
//! Alembic-stamped degenbot `SQLite` DB, read the `alembic_version` table head,
//! and treat a stamped DB as current WITHOUT re-running DDL or clobbering its
//! revision. Both [`sqlx::migrate!`] and [`refinery`] fail this — each writes
//! its own migration-tracking table and would replay DDL against a DB it does
//! not recognize. This module is the small custom alternative (Decision 2).
//!
//! Algorithm ([`ensure_schema`]):
//! 1. If the `alembic_version` table exists:
//!    - read its single `version_num` row, compare against [`ALEMBIC_HEAD`];
//!    - match → [`SchemaState::AlembicCurrent`];
//!    - older rev → [`SchemaState::AlembicStale`];
//!    - both classifications are then HEALED at open by
//!      `ensure_schema_at_open` (ADR-052 D1): the out-of-place rebuild lands
//!      `RustOwned`, unless `DEGENBOT_DB_AUTO_HEAL=0` pins the pre-D1 posture
//!      (current opens read-only; stale refuses);
//! 2. If `alembic_version` is absent but the file already has tables (a foreign
//!    `SQLite` file passed by mistake) → [`SchemaState::Unrecognized`], refuse;
//! 3. If `alembic_version` is absent AND the file is empty (a fresh standalone
//!    DB with no Alembic history) → apply the embedded DDL
//!    ([`SCHEMA_HEAD`]) + stamp the private [`SCHEMA_VERSION_TABLE`], return
//!    [`SchemaState::FreshStandalone`].

//! # Forward version-lock (ADR-052 D2)
//!
//! The D2 lock ([`crate::migrations::apply_forward_migrations`]) runs once,
//! AFTER the heal decision, for a genuine [`SchemaState::RustOwned`] DB: a
//! stamp behind the binary applies the pending embedded steps in order (each
//! its own transaction; a failure rolls back to the last-good stamp and
//! refuses), a stamp ahead refuses with [`DbError::SchemaAhead`], and a
//! stamp at current is a no-op. A freshly-healed DB is stamped at
//! [`RUST_SCHEMA_VERSION`], so it is
//! [`MigrationOutcome::AlreadyCurrent`] on the same code path.

use std::path::Path;

use degenbot_core::{op_info, op_warn};
use rusqlite::Connection;

use crate::error::DbError;
use crate::heal::{heal_database, HealReport};
use crate::migrations::{
    apply_forward_migrations, MigrationOutcome, MigrationStep, RUST_MIGRATIONS,
};
use crate::schema::{ALEMBIC_HEAD, RUST_SCHEMA_VERSION, SCHEMA_HEAD, SCHEMA_VERSION_TABLE};

/// The schema disposition [`ensure_schema`] reports for an opened DB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaState {
    /// An Alembic-stamped DB at the expected head — the common hybrid-period
    /// case. The Rust core wrote nothing; every subsequent read honors
    /// `PRAGMA query_only=on`.
    AlembicCurrent,
    /// An Alembic-stamped DB at an OLDER revision — refuse to open. Run the
    /// Python Alembic upgrade (`alembic upgrade head`) to advance the stamp.
    AlembicStale {
        /// The `version_num` actually stamped in the DB.
        head: String,
        /// The constant the Rust core expects ([`ALEMBIC_HEAD`]).
        expected: String,
    },
    /// A fresh standalone DB (no Alembic history) — the embedded DDL was applied
    /// and [`SCHEMA_VERSION_TABLE`] was stamped with [`RUST_SCHEMA_VERSION`].
    FreshStandalone {
        /// The Rust-owned schema version written to [`SCHEMA_VERSION_TABLE`].
        schema_version: u32,
    },
    /// A Rust-owned DB (post-cutover, or a re-opened `FreshStandalone`) —
    /// tables present, no `alembic_version`, and [`SCHEMA_VERSION_TABLE`]
    /// stamped. The Rust core may write schema here through future Rust-owned
    /// migrations. `FreshStandalone` is the one-shot "I just created this"
    /// report; `RustOwned` is the steady state for any Rust-owned DB
    /// thereafter (ADR-010 §2).
    RustOwned {
        /// The value stamped in [`SCHEMA_VERSION_TABLE`].
        schema_version: u32,
    },
    /// `alembic_version` is absent but the file already holds tables — a foreign
    /// `SQLite` file passed by mistake. [`ensure_schema`] refuses; [`crate::connection::DegenbotDb::open`]
    /// maps this to [`crate::error::DbError::UnrecognizedSchema`].
    Unrecognized,
}

/// The result of [`convert_alembic_to_rust_owned`] — the DB is now Rust-owned
/// at [`RUST_SCHEMA_VERSION`]. A struct (vs. a bare `u32`) so future fields
/// (e.g. an "`alembic_revision_at_cutover`" audit stamp) can land without
/// breaking the signature — matching the crate's struct-return idiom (see
/// [`crate::rows::ExchangeRow`] / [`crate::rows::PoolManagerRow`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustOwnedInfo {
    /// The value now stamped in [`SCHEMA_VERSION_TABLE`].
    pub schema_version: u32,
}

/// Inspect `conn`'s schema and bring a fresh-standalone DB up to the embedded
/// head, WITHOUT touching an Alembic-stamped DB.
///
/// # Writes
///
/// Writes ONLY on the [`SchemaState::FreshStandalone`] path (applies the DDL +
/// stamps [`SCHEMA_VERSION_TABLE`]). The caller is expected to have set
/// `PRAGMA query_only=on` only AFTER this returns on the fresh-standalone path
/// (see [`crate::connection::DegenbotDb::open`]); the other branches write
/// nothing and tolerate `query_only=on` being already set.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] on a query/DDL failure.
pub fn ensure_schema(conn: &Connection) -> Result<SchemaState, DbError> {
    let state = classify_schema(conn)?;
    if matches!(state, SchemaState::FreshStandalone { .. }) {
        apply_fresh_standalone(conn)?;
    }
    Ok(state)
}

/// The ADR-052 D1 heal-at-open killswitch: `DEGENBOT_DB_AUTO_HEAL=0`
/// restores the pre-D1 posture (a head-stamped Alembic DB opens
/// [`SchemaState::AlembicCurrent`] read-only and a stale one refuses with
/// [`DbError::AlembicStale`]) so pinned-test environments — e.g. the
/// checked-in `tests/fixtures/*.db` parity databases — are never rewritten.
pub const AUTO_HEAL_ENV: &str = "DEGENBOT_DB_AUTO_HEAL";

/// Decide the heal-at-open policy from the raw [`AUTO_HEAL_ENV`] value,
/// mirroring `degenbot_config`'s boolean word lists (truthy:
/// 1/true/yes/on/y; falsey: 0/false/off/no/n, case-insensitive + trimmed):
/// only an explicit falsey token disables the heal. `None` (unset) and every
/// other value (including the empty string) leave it enabled. Split from the
/// env read so the policy is unit-testable without mutating process state.
#[must_use]
pub fn auto_heal_enabled_from(raw: Option<&str>) -> bool {
    !matches!(
        raw.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("0" | "false" | "off" | "no" | "n")
    )
}

/// Read [`AUTO_HEAL_ENV`] and return the heal-at-open policy (`true` when
/// unset). The ONE env read on the open path; it is enumerated in
/// `degenbot-config`'s `no_stray_env_reads` gate as a sanctioned DB-open
/// toggle.
#[must_use]
pub fn auto_heal_enabled() -> bool {
    auto_heal_enabled_from(std::env::var(AUTO_HEAL_ENV).ok().as_deref())
}

/// The path-aware open entry point (ADR-052 D1 + D2): classify `path` via
/// [`ensure_schema`], and when it is Alembic-owned and `auto_heal` is set,
/// run the ADR-011 out-of-place heal ([`crate::heal::heal_database`]), log
/// the report, reopen, and re-classify. A Rust-owned DB (steadily
/// [`SchemaState::RustOwned`], or a freshly-healed one) is then brought forward
/// under the ADR-052 D2 forward version-lock ([`ensure_schema_at_open_with`] with
/// the production [`RUST_MIGRATIONS`] registry). Returns the live
/// [`Connection`] and its post-open [`SchemaState`].
///
/// `open` is the caller's connection factory — it must return a connection to
/// `path` with the concurrency PRAGMAs already applied, because this function
/// owns the close/heal/reopen cycle. A connection held across the atomic swap
/// would pin the pre-heal inode: a reader already holding the file mid-heal
/// keeps seeing the OLD inode until its next open (deliberate — the swap is
/// crash-safe by ADR-011; the next open lands on the healed inode).
/// [`ensure_schema`] itself cannot heal: it is handed an already-open
/// `&Connection` and no path, so this path-aware wrapper is the helper every
/// open path shares — read AND write, one rule (a stale read that did not heal
/// would be a lie about the schema).
///
/// Non-Rust-owned states (`FreshStandalone`, `Unrecognized`, and the
/// killswitch-pinned Alembic dispositions) pass through the lock untouched; the
/// caller applies its own refusal for `Unrecognized` (and for a stale Alembic
/// DB when the killswitch pinned `auto_heal` to `false`).
///
/// # Errors
///
/// [`DbError::Sqlite`] / [`DbError::Io`] / [`DbError::HealVerificationFailed`]
/// if the heal fails — the live DB is left untouched (ADR-011) and the
/// caller's dropped handle is not replaced. The D2 lock errors
/// ([`DbError::SchemaAhead`], [`DbError::MissingMigrationStep`],
/// [`DbError::MigrationStepFailed`]) propagate from the Rust-owned open.
pub(crate) fn ensure_schema_at_open<F>(
    path: &Path,
    auto_heal: bool,
    open: F,
) -> Result<(Connection, SchemaState), DbError>
where
    F: FnMut(&Path) -> Result<Connection, DbError>,
{
    ensure_schema_at_open_with(path, auto_heal, open, RUST_MIGRATIONS, RUST_SCHEMA_VERSION)
}

/// [`ensure_schema_at_open`] with an injectable forward-migration registry +
/// target — the seam the IOGST2 tests use to exercise the D2 version-lock
/// (pending apply / ordering / step failure / gap / ahead) without waiting for
/// a real [`RUST_SCHEMA_VERSION`] bump. Production always passes
/// [`RUST_MIGRATIONS`] / [`RUST_SCHEMA_VERSION`].
///
/// The lock runs in exactly one place, AFTER the heal decision, and only for a
/// genuine [`SchemaState::RustOwned`] DB:
/// 1. the healed DB (reopened + re-classified `RustOwned` at
///    [`RUST_SCHEMA_VERSION`]) — the same code path, so a heal never leaves
///    pending work;
/// 2. a Rust-owned open that did not heal (the steady-state reopen), including
///    under the `DEGENBOT_DB_AUTO_HEAL=0` killswitch (ownership is orthogonal
///    to heal policy).
///
/// `FreshStandalone` is skipped: [`ensure_schema`] already applied
/// [`SCHEMA_HEAD`] and stamped [`RUST_SCHEMA_VERSION`], so it is at current
/// by construction. `AlembicCurrent` / `AlembicStale` / `Unrecognized`
/// are never Rust-owned and are never written here.
pub(crate) fn ensure_schema_at_open_with<F>(
    path: &Path,
    auto_heal: bool,
    mut open: F,
    steps: &[MigrationStep],
    target: u32,
) -> Result<(Connection, SchemaState), DbError>
where
    F: FnMut(&Path) -> Result<Connection, DbError>,
{
    let conn = open(path)?;
    let state = ensure_schema(&conn)?;

    // Heal only the two Alembic-owned dispositions, and only when enabled (the
    // `DEGENBOT_DB_AUTO_HEAL=0` killswitch pins the pre-D1 behavior).
    if !auto_heal
        || !matches!(
            state,
            SchemaState::AlembicCurrent | SchemaState::AlembicStale { .. }
        )
    {
        // Non-heal path: a genuine Rust-owned DB is brought forward under the D2
        // lock; every other disposition passes through untouched.
        let state = apply_forward_lock(&conn, state, steps, target)?;
        return Ok((conn, state));
    }

    // The atomic swap below renames the source file: drop this handle first so
    // the rename has no reader pinning the old inode and the reopened
    // connection lands on the healed file.
    drop(conn);

    let report = heal_database(path)?;
    log_heal_report(&report);

    let reopened = open(path)?;
    let healed_state = ensure_schema(&reopened)?;
    // A freshly-healed DB is stamped at RUST_SCHEMA_VERSION by the heal's
    // cutover step, so the lock is AlreadyCurrent here — running it anyway is
    // the "same code path" guarantee that a heal never strands pending work.
    let healed_state = apply_forward_lock(&reopened, healed_state, steps, target)?;
    Ok((reopened, healed_state))
}

/// Run the ADR-052 D2 forward version-lock on `conn` when `state` is a genuine
/// [`SchemaState::RustOwned`] DB, returning the post-apply disposition
/// (re-classified from the stamp, since an apply advances it). Non-Rust-owned
/// states are returned unchanged and `conn` is never written.
///
/// # Errors
///
/// As [`apply_forward_migrations`]: [`DbError::SchemaAhead`] (stamp newer
/// than the binary — hard halt, nothing written),
/// [`DbError::MissingMigrationStep`] (registry gap),
/// [`DbError::MigrationStepFailed`] (a step rolled back, DB left usable at
/// the last-good stamp).
fn apply_forward_lock(
    conn: &Connection,
    state: SchemaState,
    steps: &[MigrationStep],
    target: u32,
) -> Result<SchemaState, DbError> {
    if !matches!(state, SchemaState::RustOwned { .. }) {
        return Ok(state);
    }
    if let MigrationOutcome::Applied { from, to } = apply_forward_migrations(conn, steps, target)? {
        op_info!(
            domain = state,
            from,
            to,
            "database schema migrated forward at open"
        );
    }
    // Re-read the stamp so the reported disposition reflects the applied steps.
    classify_schema(conn)
}

/// Emit the ADR-052 D1 heal report: one `op_info!` headline (detected
/// revision, tables/rows copied, warning count, `.bak` path) plus one
/// `op_warn!` per non-fatal heal warning.
///
/// Logged under the closed `state` telemetry domain: the domain set has no
/// dedicated DB domain, and a schema heal is a persistence-state lifecycle
/// event (the same domain the driver uses for its boot snapshot load).
fn log_heal_report(report: &HealReport) {
    let revision = match &report.old_state {
        SchemaState::AlembicStale { head, .. } => head.clone(),
        SchemaState::AlembicCurrent => ALEMBIC_HEAD.to_string(),
        other => format!("{other:?}"),
    };
    let tables = report.rows_copied.len();
    let rows: u64 = report.rows_copied.values().copied().sum();
    op_info!(
        domain = state,
        revision = %revision,
        tables,
        rows,
        warnings = report.warnings.len(),
        backup = %report.bak_path.display(),
        "database auto-healed at open"
    );
    for warning in &report.warnings {
        op_warn!(domain = state, warning = %warning, "database auto-heal warning");
    }
}

/// The pure-predicate half of [`ensure_schema`]: inspect `conn`'s schema and
/// report the [`SchemaState`] WITHOUT applying any DDL. The
/// [`SchemaState::FreshStandalone`] arm here reports the *would-be* state
/// (an empty file) but does NOT create tables — so a read-only inspector (the
/// `database cutover --dry-run` path, [`crate::ops::inspect_schema_state`]) can
/// ask "what state is this DB in?" without the DDL side effect.
///
/// [`ensure_schema`] delegates here and applies DDL only when the classification
/// is `FreshStandalone`, preserving its byte-for-byte behavior.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] on a query failure (never refuses — returns the
/// state for ALL cases including `AlembicStale` / `Unrecognized`).
pub fn classify_schema(conn: &Connection) -> Result<SchemaState, DbError> {
    let has_alembic = table_exists(conn, "alembic_version")?;

    if has_alembic {
        let head: String = conn.query_row(
            &format!("SELECT version_num FROM {}", "alembic_version"),
            [],
            |row| row.get(0),
        )?;
        if head == ALEMBIC_HEAD {
            Ok(SchemaState::AlembicCurrent)
        } else {
            Ok(SchemaState::AlembicStale {
                head,
                expected: ALEMBIC_HEAD.to_string(),
            })
        }
    } else {
        // No Alembic history. Is the file empty (fresh standalone), a Rust-owned
        // DB (tables present + the private stamp table), or a foreign SQLite file?
        //
        // The private [`SCHEMA_VERSION_TABLE`] is filtered from the content-table
        // count (ADR-010 §2: "it is filtered, like `sqlite_%`") so a Rust-owned
        // DB whose only tables are the content tables + the stamp is not
        // miscounted.
        let table_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' \
             AND name NOT LIKE 'sqlite_%' AND name <> ?1",
            rusqlite::params![SCHEMA_VERSION_TABLE],
            |row| row.get(0),
        )?;
        if table_count > 0 {
            // Tables present, no Alembic. Is this a Rust-owned DB (post-cutover
            // or a re-opened FreshStandalone)? The stamp table is the tell.
            if table_exists(conn, SCHEMA_VERSION_TABLE)? {
                // Read the stamped version. A present stamp table with no row is
                // a corrupt cutover → refuse as Unrecognized.
                let row_exists: i64 = conn.query_row(
                    &format!("SELECT COUNT(*) FROM {SCHEMA_VERSION_TABLE}"),
                    [],
                    |row| row.get(0),
                )?;
                if row_exists == 0 {
                    return Ok(SchemaState::Unrecognized);
                }
                let v: i64 = conn.query_row(
                    &format!("SELECT schema_version FROM {SCHEMA_VERSION_TABLE}"),
                    [],
                    |row| row.get(0),
                )?;
                Ok(SchemaState::RustOwned {
                    schema_version: u32::try_from(v).unwrap_or(0),
                })
            } else {
                Ok(SchemaState::Unrecognized)
            }
        } else {
            // Empty file: the would-be state is FreshStandalone. classify_schema
            // does NOT apply the DDL here (the caller — ensure_schema or a
            // read-only inspector — decides whether to).
            Ok(SchemaState::FreshStandalone {
                schema_version: RUST_SCHEMA_VERSION,
            })
        }
    }
}

/// The opt-in cutover operation (ADR-010 §2): flip an `AlembicCurrent` DB
/// into Rust ownership. Verifies `alembic_version.version_num ==
/// ALEMBIC_HEAD`, drops the `alembic_version` table, and stamps
/// [`SCHEMA_VERSION_TABLE`] with [`RUST_SCHEMA_VERSION`]. Refuses
/// [`SchemaState::AlembicStale`] (upgrade via Alembic first) and
/// [`SchemaState::Unrecognized`] (foreign file).
///
/// This is the ONE operation that writes schema to an otherwise-Alembic DB,
/// and only because the DB is LEAVING Alembic ownership. Already-Rust-owned
/// (and a fresh standalone) DB is an idempotent re-stamp — `ensure_schema`
/// returns [`SchemaState::RustOwned`] / [`SchemaState::FreshStandalone`], and
/// we just re-stamp the version table (a `DELETE + INSERT` no-op).
///
/// # Errors
///
/// Returns [`DbError::AlembicStale`] for a stale Alembic DB,
/// [`DbError::UnrecognizedSchema`] for a foreign file, or [`DbError::Sqlite`]
/// on a query/DDL failure.
pub fn convert_alembic_to_rust_owned(conn: &Connection) -> Result<RustOwnedInfo, DbError> {
    let state = ensure_schema(conn)?;
    match state {
        SchemaState::AlembicCurrent => {
            // Drop the Alembic stamp table + stamp the Rust-owned version table.
            conn.execute_batch("DROP TABLE IF EXISTS alembic_version;")?;
            stamp_rust_schema_version(conn)?;
            Ok(RustOwnedInfo {
                schema_version: RUST_SCHEMA_VERSION,
            })
        }
        SchemaState::AlembicStale { head, expected } => {
            Err(DbError::AlembicStale { head, expected })
        }
        SchemaState::Unrecognized => Err(DbError::UnrecognizedSchema),
        // Idempotent re-stamp: the DB is already Rust-owned (or freshly
        // standalone) — reassert the stamp table (DELETE + INSERT no-op).
        SchemaState::RustOwned { .. } | SchemaState::FreshStandalone { .. } => {
            stamp_rust_schema_version(conn)?;
            Ok(RustOwnedInfo {
                schema_version: RUST_SCHEMA_VERSION,
            })
        }
    }
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

/// Apply the embedded DDL and stamp the private Rust-owned schema-version
/// table (fresh-standalone path only).
fn apply_fresh_standalone(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch(SCHEMA_HEAD)?;
    stamp_rust_schema_version(conn)?;
    Ok(())
}

/// Create (if absent) and stamp the private Rust-owned schema-version table
/// with [`RUST_SCHEMA_VERSION`] (`CREATE TABLE IF NOT EXISTS` + `DELETE` +
/// `INSERT`). Idempotent — safe to re-assert on an already-stamped DB. Shared
/// by [`apply_fresh_standalone`] (fresh-standalone first open) and
/// [`convert_alembic_to_rust_owned`] (the cutover op).
fn stamp_rust_schema_version(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {SCHEMA_VERSION_TABLE} (schema_version INTEGER NOT NULL);\n\
         DELETE FROM {SCHEMA_VERSION_TABLE};"
    ))?;
    conn.execute(
        &format!("INSERT INTO {SCHEMA_VERSION_TABLE} (schema_version) VALUES (?1)"),
        rusqlite::params![i64::from(RUST_SCHEMA_VERSION)],
    )?;
    Ok(())
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::schema::table;

    #[test]
    fn fresh_in_memory_applies_ddl_and_stamps_version() {
        let conn = Connection::open_in_memory().unwrap();
        let state = ensure_schema(&conn).unwrap();
        assert_eq!(
            state,
            SchemaState::FreshStandalone {
                schema_version: RUST_SCHEMA_VERSION,
            }
        );
        // every core table is present
        for t in [
            table::EXCHANGES,
            table::ERC20_TOKENS,
            table::POOLS,
            table::LIQUIDITY_POSITIONS,
            table::INITIALIZATION_MAPS,
            table::POOL_MANAGERS,
            table::MANAGED_POOLS,
            table::UNISWAP_V4_POOLS,
            table::MANAGED_POOL_LIQUIDITY_POSITIONS,
            table::MANAGED_POOL_INITIALIZATION_MAPS,
        ] {
            assert!(
                table_exists(&conn, t).unwrap(),
                "{t} should exist after ensure_schema"
            );
        }
        // the private stamp table holds the version
        let v: i64 = conn
            .query_row(
                &format!("SELECT schema_version FROM {SCHEMA_VERSION_TABLE}"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v, i64::from(RUST_SCHEMA_VERSION));
    }

    #[test]
    fn alembic_current_writes_nothing() {
        let conn = Connection::open_in_memory().unwrap();
        // stamp an alembic_version row at the head
        conn.execute_batch(
            "CREATE TABLE alembic_version (version_num VARCHAR(32) NOT NULL);\n\
             INSERT INTO alembic_version (version_num) VALUES ('2606a6c7f5ee');",
        )
        .unwrap();
        let state = ensure_schema(&conn).unwrap();
        assert_eq!(state, SchemaState::AlembicCurrent);
        // no degenbot tables should have been created
        assert!(!table_exists(&conn, table::POOLS).unwrap());
        assert!(!table_exists(&conn, SCHEMA_VERSION_TABLE).unwrap());
    }

    #[test]
    fn alembic_stale_refuses() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE alembic_version (version_num VARCHAR(32) NOT NULL);\n\
             INSERT INTO alembic_version (version_num) VALUES ('deadbeefdead');",
        )
        .unwrap();
        let state = ensure_schema(&conn).unwrap();
        match state {
            SchemaState::AlembicStale { head, expected } => {
                assert_eq!(head, "deadbeefdead");
                assert_eq!(expected, ALEMBIC_HEAD);
            }
            other => panic!("expected AlembicStale, got {other:?}"),
        }
    }

    #[test]
    fn unrecognized_foreign_db_refuses() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE other_table (x INTEGER);")
            .unwrap();
        let state = ensure_schema(&conn).unwrap();
        assert_eq!(state, SchemaState::Unrecognized);
    }

    // ── RustOwned + the cutover (ADR-010 §2) ──────────────────────────────

    #[test]
    fn rustowned_on_reopen_after_fresh_standalone() {
        // apply_fresh_standalone applies DDL + stamps the version table.
        let conn = Connection::open_in_memory().unwrap();
        let first = ensure_schema(&conn).unwrap();
        assert_eq!(
            first,
            SchemaState::FreshStandalone {
                schema_version: RUST_SCHEMA_VERSION,
            }
        );

        // re-open the SAME conn — tables now present, no alembic, stamp table
        // stamped → RustOwned (the re-open bug the cutover closes).
        let second = ensure_schema(&conn).unwrap();
        assert_eq!(
            second,
            SchemaState::RustOwned {
                schema_version: RUST_SCHEMA_VERSION,
            }
        );
    }

    #[test]
    fn convert_alembic_current_to_rust_owned() {
        // Start from a Rust-owned DB (apply DDL + stamp), then flip it back to
        // an AlembicCurrent-shaped DB: drop the Rust stamp table, create +
        // stamp alembic_version at the head.
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap(); // FreshStandalone applies the full schema
        conn.execute_batch(&format!(
            "DROP TABLE {SCHEMA_VERSION_TABLE};\n\
             CREATE TABLE alembic_version (version_num VARCHAR(32) NOT NULL);\n\
             INSERT INTO alembic_version (version_num) VALUES ('{ALEMBIC_HEAD}');"
        ))
        .unwrap();
        assert_eq!(ensure_schema(&conn).unwrap(), SchemaState::AlembicCurrent);

        // Cutover.
        let info = convert_alembic_to_rust_owned(&conn).unwrap();
        assert_eq!(
            info,
            RustOwnedInfo {
                schema_version: RUST_SCHEMA_VERSION
            }
        );

        // alembic_version is GONE, the Rust stamp table is stamped.
        assert!(!table_exists(&conn, "alembic_version").unwrap());
        assert!(table_exists(&conn, SCHEMA_VERSION_TABLE).unwrap());
        let v: i64 = conn
            .query_row(
                &format!("SELECT schema_version FROM {SCHEMA_VERSION_TABLE}"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v, i64::from(RUST_SCHEMA_VERSION));

        // re-open → RustOwned.
        assert_eq!(
            ensure_schema(&conn).unwrap(),
            SchemaState::RustOwned {
                schema_version: RUST_SCHEMA_VERSION,
            }
        );
    }

    #[test]
    fn convert_refuses_alembic_stale() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE alembic_version (version_num VARCHAR(32) NOT NULL);\n\
             INSERT INTO alembic_version (version_num) VALUES ('deadbeefdead');",
        )
        .unwrap();
        match convert_alembic_to_rust_owned(&conn) {
            Err(DbError::AlembicStale { head, expected }) => {
                assert_eq!(head, "deadbeefdead");
                assert_eq!(expected, ALEMBIC_HEAD);
            }
            other => panic!("expected AlembicStale, got {other:?}"),
        }
    }

    #[test]
    fn convert_refuses_unrecognized() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE other_table (x INTEGER);")
            .unwrap();
        match convert_alembic_to_rust_owned(&conn) {
            Err(DbError::UnrecognizedSchema) => {}
            other => panic!("expected UnrecognizedSchema, got {other:?}"),
        }
    }

    #[test]
    fn convert_idempotent_on_rustowned() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap(); // FreshStandalone → tables + stamp
        assert!(matches!(
            ensure_schema(&conn).unwrap(),
            SchemaState::RustOwned { .. }
        ));

        // Convert an already-Rust-owned DB → succeeds, re-stamps, still RustOwned.
        let info = convert_alembic_to_rust_owned(&conn).unwrap();
        assert_eq!(
            info,
            RustOwnedInfo {
                schema_version: RUST_SCHEMA_VERSION
            }
        );
        assert_eq!(
            ensure_schema(&conn).unwrap(),
            SchemaState::RustOwned {
                schema_version: RUST_SCHEMA_VERSION,
            }
        );
    }

    #[test]
    fn classify_schema_on_empty_returns_fresh_standalone_without_creating_tables() {
        // classify_schema is the pure-predicate half of ensure_schema: it must
        // report the would-be state (FreshStandalone) WITHOUT applying DDL.
        let conn = Connection::open_in_memory().unwrap();
        let state = classify_schema(&conn).unwrap();
        assert_eq!(
            state,
            SchemaState::FreshStandalone {
                schema_version: RUST_SCHEMA_VERSION,
            }
        );
        // no tables were created — the file is still empty.
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "classify_schema must not apply DDL");
    }
}

/// ADR-052 D1 heal-at-open tests: the path-aware open cycle (head + stale
/// auto-heal), the `DEGENBOT_DB_AUTO_HEAL=0` killswitch policy, idempotence,
/// the D2 forward-lock no-op, and the ADR-011 failure guarantee.
#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod open_tests {
    use super::*;
    use crate::migrations::{apply_rust_migrations, MigrationOutcome};
    use crate::ops::create_new_database;
    use crate::schema::SCHEMA_VERSION_TABLE;

    /// The connection factory the open paths pass: a connection with the three
    /// concurrency PRAGMAs (mirrors `connection::PRE_SCHEMA_PRAGMAS`).
    fn primed(path: &Path) -> Result<Connection, DbError> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;\n\
             PRAGMA busy_timeout=5000;\n\
             PRAGMA synchronous=NORMAL;",
        )?;
        Ok(conn)
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

    fn bak_path(path: &Path) -> std::path::PathBuf {
        let mut s = path.file_name().unwrap().to_owned();
        s.push(".bak");
        path.with_file_name(s)
    }

    /// A head-schema DB stamped one revision below `ALEMBIC_HEAD`.
    fn build_stale_fixture(path: &Path) {
        create_new_database(path).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.execute("DROP INDEX ix_erc20_tokens_chain", [])
            .unwrap();
        conn.execute("UPDATE alembic_version SET version_num='e0aaad8ad486'", [])
            .unwrap();
    }

    #[test]
    fn killswitch_env_policy_matches_falsey_words() {
        assert!(auto_heal_enabled_from(None), "unset -> heal on");
        assert!(auto_heal_enabled_from(Some("1")));
        assert!(auto_heal_enabled_from(Some("")));
        assert!(auto_heal_enabled_from(Some("  ")));
        for off in ["0", "false", "off", "no", "n", " FALSE "] {
            assert!(!auto_heal_enabled_from(Some(off)), "{off:?} must disable");
        }
        assert_eq!(AUTO_HEAL_ENV, "DEGENBOT_DB_AUTO_HEAL");
    }

    #[test]
    fn head_stamped_db_auto_heals_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("head.db");
        create_new_database(&db_path).unwrap();

        let (conn, state) = ensure_schema_at_open(&db_path, true, primed).unwrap();
        assert!(
            matches!(state, SchemaState::RustOwned { .. }),
            "auto-heal must land RustOwned, got {state:?}"
        );
        assert!(!has_table(&conn, "alembic_version"));
        assert!(has_table(&conn, SCHEMA_VERSION_TABLE));
        drop(conn);
        // `.bak` preserves the original Alembic-stamped file.
        let bak = Connection::open(bak_path(&db_path)).unwrap();
        assert!(has_table(&bak, "alembic_version"));
    }

    #[test]
    fn stale_rev_db_auto_heals_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("stale.db");
        build_stale_fixture(&db_path);
        assert!(matches!(
            classify_schema(&Connection::open(&db_path).unwrap()).unwrap(),
            SchemaState::AlembicStale { .. }
        ));

        let (_conn, state) = ensure_schema_at_open(&db_path, true, primed).unwrap();
        assert!(
            matches!(state, SchemaState::RustOwned { .. }),
            "got {state:?}"
        );
    }

    #[test]
    fn killswitch_disabled_leaves_alembic_db_untouched() {
        // `auto_heal=false` is the `DEGENBOT_DB_AUTO_HEAL=0` path.
        let dir = tempfile::tempdir().unwrap();
        let stale = dir.path().join("stale.db");
        build_stale_fixture(&stale);
        let (conn, disposition) = ensure_schema_at_open(&stale, false, primed).unwrap();
        assert!(matches!(disposition, SchemaState::AlembicStale { .. }));
        assert!(has_table(&conn, "alembic_version"));
        drop(conn);
        assert!(!bak_path(&stale).exists());

        let head = dir.path().join("head.db");
        create_new_database(&head).unwrap();
        let (_conn, disposition) = ensure_schema_at_open(&head, false, primed).unwrap();
        assert_eq!(disposition, SchemaState::AlembicCurrent);
        assert!(!bak_path(&head).exists());
    }

    #[test]
    fn subsequent_open_lands_rust_owned_without_re_healing() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("idem.db");
        create_new_database(&db_path).unwrap();

        let (_c1, first) = ensure_schema_at_open(&db_path, true, primed).unwrap();
        assert!(matches!(first, SchemaState::RustOwned { .. }));
        let bak_before = std::fs::read(bak_path(&db_path)).unwrap();

        let (c2, second) = ensure_schema_at_open(&db_path, true, primed).unwrap();
        assert_eq!(second, first, "second open is already RustOwned");
        assert!(!has_table(&c2, "alembic_version"));
        assert_eq!(
            std::fs::read(bak_path(&db_path)).unwrap(),
            bak_before,
            "no second heal may rewrite the `.bak`"
        );
    }

    #[test]
    fn healed_db_passes_apply_rust_migrations_as_noop() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("lock.db");
        create_new_database(&db_path).unwrap();
        let (_conn, state) = ensure_schema_at_open(&db_path, true, primed).unwrap();
        assert!(matches!(state, SchemaState::RustOwned { .. }));

        let probe = Connection::open(&db_path).unwrap();
        let outcome = apply_rust_migrations(&probe).unwrap();
        assert_eq!(
            outcome,
            MigrationOutcome::AlreadyCurrent {
                schema_version: RUST_SCHEMA_VERSION
            },
            "heal leaves the file at the forward-lock's current stamp"
        );
    }

    #[test]
    fn heal_failure_leaves_original_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("poison.db");
        create_new_database(&db_path).unwrap();
        {
            let c = Connection::open(&db_path).unwrap();
            c.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (1, 1, '0xabc')",
                [],
            )
            .unwrap();
            // Drop the old DB's UNIQUE index so a duplicate can sneak in; the
            // fresh head-schema DB still enforces it -> the copy fails mid-table.
            c.execute("DROP INDEX ix_erc20_tokens_address_chain", [])
                .unwrap();
            c.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (2, 1, '0xabc')",
                [],
            )
            .unwrap();
        }
        let before = std::fs::read(&db_path).unwrap();

        let err = ensure_schema_at_open(&db_path, true, primed).unwrap_err();
        assert!(matches!(err, DbError::Sqlite(_)), "got {err:?}");

        // ADR-011: live DB untouched (same bytes), no `.bak`, no temp file.
        assert_eq!(std::fs::read(&db_path).unwrap(), before);
        assert!(!bak_path(&db_path).exists());
        assert!(!db_path.with_file_name("poison.db.heal-tmp").exists());
        let probe = Connection::open(&db_path).unwrap();
        assert!(has_table(&probe, "alembic_version"));
        let n: i64 = probe
            .query_row("SELECT COUNT(*) FROM erc20_tokens", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
    }
}
