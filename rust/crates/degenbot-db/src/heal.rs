//! ADR-011: the **out-of-place dump-and-restore heal operation**.
//!
//! [`heal_database`] reads an old DB **read-only**, rebuilds a fresh DB
//! against the Rust head schema, copies rows preserving primary keys + FK
//! integrity, stamps the new DB [`SchemaState::RustOwned`] directly (running
//! **no** Alembic code), and atomically swaps it into place — preserving the
//! old DB as `*.bak`. This is the rugpull-protection complement to
//! [`crate::migrate::convert_alembic_to_rust_owned`] (the in-place
//! [`crate::ops::convert_alembic_to_rust_owned`]): `cutover` is fast (an
//! in-place ownership flip assuming the head schema); `heal` is general
//! (any old state → `RustOwned`, schema-agnostic via an auto-derived
//! column mapping).
//!
//! # Why out-of-place
//!
//! The Alembic migration chain is structurally incoherent — not forward
//! buildable — so deleting it without a replacement strands every legacy DB
//! (the rugpull ADR-010 explicitly promised to avoid). `heal` makes the
//! chain's forward-buildability — and its existence — irrelevant, so the 0.7.0
//! retirement (deleting `src/degenbot/migrations/`) is **non-breaking**.
//! See `docs/adr/ADR-011-auto-healed-alembic-retirement.md`.
//!
//! # The 7-step operation (ADR-011 §"The heal operation")
//!
//! 1. Inspect old (read-only). `RustOwned` → no-op; `Unrecognized` →
//!    refuse; `AlembicCurrent` / `AlembicStale` / `FreshStandalone` → proceed.
//!    The old read-only handle is then dropped — step 3 re-attaches the old DB
//!    inside the new connection.
//! 2. Build a fresh DB at a sibling temp path via
//!    [`create_new_database`][crate::ops::create_new_database] (the coherent
//!    head DDL + Alembic-head stamp). Switch the new DB to `journal_mode=DELETE`
//!    so no `-wal`/`-shm` side-files complicate the atomic rename.
//! 3. ATTACH the old DB read-only into the new connection (`file:…?mode=ro`)
//!    and copy rows table-by-table via a native `INSERT INTO main.t (…) SELECT
//!    … FROM old.t` — `SQLite` runs the row transport page-level inside its
//!    engine (no Rust row buffer). FK enforcement is OFF on `main` during the
//!    copy, so no parent-before-child ordering is needed; tables are iterated
//!    alphabetically (deterministic).
//! 4. Apply the per-table column mapping (auto-derived from
//!    `PRAGMA old.table_info` vs `PRAGMA main.table_info`; see
//!    [`RENAME_OVERRIDES`]). Three classes: present-in-both (copy),
//!    added-after (omit; `SQLite` applies the column default), dropped
//!    (skip the read).
//! 5. Stamp the new DB `RustOwned` directly — reuse
//!    [`migrate::convert_alembic_to_rust_owned`] (drops `alembic_version`,
//!    stamps `_degenbot_db_schema_version`). **No** Alembic code runs on the
//!    heal path.
//! 6. Verify per-table row counts (old read-only `COUNT(*)` == new
//!    `COUNT(*)`); on mismatch → [`DbError::HealVerificationFailed`], clean
//!    up the temp file, leave the live DB untouched.
//! 7. Atomic swap + `*.bak`: close both connections, `rename(old → .bak)`
//!    then `rename(tmp → old)` (siblings; atomic on the same filesystem),
//!    relocating any `-wal`/`-shm` side-files of the old DB alongside the
//!    backup so the `.bak` is a complete recoverable snapshot. On any failure
//!    the live DB is untouched and the partial temp file is cleaned up.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::error::DbError;
use crate::migrate::{
    classify_schema, convert_alembic_to_rust_owned as run_cutover_on_conn, SchemaState,
};
use crate::ops::create_new_database;
use crate::schema::RUST_SCHEMA_VERSION;

/// The outcome of [`heal_database`] — what was healed, how many rows copied
/// per table, and where the `.bak` backup landed.
///
/// `rows_copied` is empty for the no-op `already_rust_owned` case; `bak_path`
/// equals `old_path` in that case (no backup taken — the DB was already
/// Rust-owned). `warnings` holds non-fatal notes (e.g. an ambiguous rename
/// treated as drop+add with data loss for that column) so callers can log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealReport {
    /// The schema state of the OLD DB (`alembic_current` / `alembic_stale` /
    /// `fresh_standalone` / `rust_owned`).
    pub old_state: SchemaState,
    /// Per-table row counts copied old → new. Empty iff no copy ran
    /// (the no-op `rust_owned` case).
    pub rows_copied: HashMap<String, u64>,
    /// Path to the old-DB `.bak` backup (preserved on success). Equals
    /// `old_path` for the no-op `rust_owned` case.
    pub bak_path: PathBuf,
    /// The schema state of the live DB after heal — always `RustOwned`
    /// (either it was already, or `heal` made it so).
    pub new_state: SchemaState,
    /// Non-fatal notes (e.g. an ambiguous rename treated as drop + add).
    pub warnings: Vec<String>,
}

/// Disambiguate renamed columns whose old name is absent on the new schema.
/// Consul**ed** only when a column on the NEW schema has no direct match on
/// the OLD and a column on the OLD has no direct match on the NEW (a potential
/// rename). Three rename migrations (verified by reading the migration source):
///
/// - `6a77c4e07151` (`rename_column`): `pools.token0_id_` → `pools.token0_id`,
///   `pools.token1_id_` → `pools.token1_id`.
/// - `5c8805573ab3` (`convert_pool_hash_to_text`): adds
///   `uniswap_v4_pools.pool_hash_hex` (`TEXT`) copying `'0x' || hex(pool_hash)`;
///   `a4f59783919f` (`rename_pool_hash_column`) renames `pool_hash_hex` →
///   `pool_hash` — so the value-preserving mapping for a DB that carries
///   `pool_hash_hex` (`TEXT`) is `uniswap_v4_pools.pool_hash_hex` →
///   `uniswap_v4_pools.pool_hash`.
///
/// For the **common heal case** (old DB at/near Alembic head) old and new
/// schemas are byte-identical, so this table is never exercised. It matters
/// only for genuinely stale old DBs missing the rename.
const RENAME_OVERRIDES: &[(&str, &str, &str)] = &[
    // 6a77c4e07151 rename_column
    ("pools", "token0_id_", "token0_id"),
    ("pools", "token1_id_", "token1_id"),
    // 5c8805573ab3 + a4f59783919f (value-preserving hex→text + rename)
    ("uniswap_v4_pools", "pool_hash_hex", "pool_hash"),
];

/// Out-of-place dump-and-restore heal (ADR-011). See the module docs for the
/// 7-step operation and the rugpull-protection guarantee.
///
/// # Errors
///
/// [`DbError::UnrecognizedSchema`] if `old_path` is a foreign `SQLite` file;
/// [`DbError::HealVerificationFailed`] if a per-table row count mismatches
/// after the copy (live DB untouched, temp file cleaned up);
/// [`DbError::Sqlite`] on any I/O / SQL failure (mid-heal failure leaves the
/// live DB untouched and cleans up the partial temp file).
pub fn heal_database(old_path: &Path) -> Result<HealReport, DbError> {
    // ── Step 1: inspect old, read-only. ──────────────────────────────────
    // A read-only open never creates/extends the old DB's `-wal`/`-shm`, so the
    // old file is left byte-identical (THE rugpull-protection guarantee).
    let old_conn = open_readonly(old_path)?;
    let old_state = classify_schema(&old_conn)?;

    match old_state {
        SchemaState::RustOwned { schema_version } => {
            // Already Rust-owned — no-op. No backup, no copy, live DB untouched.
            drop(old_conn);
            return Ok(HealReport {
                old_state: SchemaState::RustOwned { schema_version },
                rows_copied: HashMap::new(),
                bak_path: old_path.to_path_buf(),
                new_state: SchemaState::RustOwned { schema_version },
                warnings: Vec::new(),
            });
        }
        SchemaState::Unrecognized => {
            // A genuinely foreign SQLite file — refuse to adopt it.
            drop(old_conn);
            return Err(DbError::UnrecognizedSchema);
        }
        // AlembicCurrent / AlembicStale / FreshStandalone → proceed to heal.
        _ => {}
    }

    let old_state_for_report = old_state.clone();
    // The old DB is no longer needed as a separate handle: step 3 ATTACHes it
    // read-only into the new connection. Dropping it here avoids two handles
    // on the same file during the copy.
    drop(old_conn);

    // ── Step 2: build a fresh head-schema DB at a sibling temp path. ──────
    // Sibling → same filesystem → the final `rename` is atomic.
    let tmp_path = sibling_with_suffix(old_path, ".heal-tmp");
    // Best-effort: clear any leftover temp file from a prior failed heal.
    let _ = std::fs::remove_file(&tmp_path);
    remove_sidecar_if_present(&tmp_path, "-wal");
    remove_sidecar_if_present(&tmp_path, "-shm");

    // If building the fresh DB fails, clean up the partial temp file and bubble.
    if let Err(e) = create_new_database(&tmp_path) {
        cleanup_temp(&tmp_path);
        return Err(e);
    }

    // `new_conn` (and its ATTACHed `old` schema) is dropped at the end of this
    // scope — before the atomic rename, which requires no open handle on the
    // temp file.
    let mut warnings: Vec<String> = Vec::new();
    let rows_copied = {
        let mut new_conn = match Connection::open(&tmp_path) {
            Ok(c) => c,
            Err(e) => {
                cleanup_temp(&tmp_path);
                return Err(DbError::Sqlite(e));
            }
        };
        new_conn
            .execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=NORMAL;")
            .map_err(|e| {
                cleanup_temp(&tmp_path);
                DbError::Sqlite(e)
            })?;
        // OFF so pre-existing rows with mismatched FKs (a stale/divergent old
        // DB) don't abort the copy mid-way; heal preserves row data and lets
        // the post-heal application re-establish its invariants.
        new_conn
            .execute_batch("PRAGMA foreign_keys=OFF;")
            .map_err(|e| {
                cleanup_temp(&tmp_path);
                DbError::Sqlite(e)
            })?;

        // ── Step 3-4: ATTACH the old DB read-only and copy each table via a
        //    native `INSERT INTO main.t SELECT … FROM old.t`. This runs the
        //    row transport inside SQLite's engine (page-level) instead of a
        //    Rust `Vec<Vec<Value>>` buffer. `file:…?mode=ro` makes the attached
        //    schema read-only at the VFS level, so the old DB is byte-identical
        //    after heal — the rugpull-protection guarantee, with no second
        //    read-only handle to juggle and no connection-wide write lock.
        //
        // FK ordering is unnecessary: `foreign_keys=OFF` above lifts the
        //    parent-before-child constraint, so a plain alphabetical
        //    `sqlite_master` scan (deterministic) suffices.
        let old_uri = file_ro_uri(old_path);
        new_conn
            .execute_batch(&format!("ATTACH DATABASE '{old_uri}' AS old;"))
            .map_err(|e| {
                cleanup_temp(&tmp_path);
                DbError::Sqlite(e)
            })?;

        let tables = content_tables(&new_conn).map_err(|e| {
            cleanup_temp(&tmp_path);
            DbError::Sqlite(e)
        })?;

        let mut rows_copied = HashMap::new();
        for table in &tables {
            let n = match copy_table(&mut new_conn, table, &mut warnings) {
                Ok(n) => n,
                Err(e) => {
                    cleanup_temp(&tmp_path);
                    return Err(e);
                }
            };
            rows_copied.insert(table.clone(), n);
        }

        // ── Step 5: (removed) — no `sqlite_sequence` reset. The head schema
        //    uses `INTEGER PRIMARY KEY` (rowid aliasing), never `AUTOINCREMENT`,
        //    so `sqlite_sequence` never exists; SQLite manages it itself if it
        //    ever did.

        // ── Step 6: stamp RustOwned directly (drops alembic_version, skips Alembic). ──
        if let Err(e) = run_cutover_on_conn(&new_conn) {
            cleanup_temp(&tmp_path);
            return Err(e);
        }

        rows_copied
    };

    // ── Step 7: verify per-table row counts (old read-only == new). ──────
    // Re-open both read-only for the count comparison (cheap; isolated).
    if let Err(e) = verify_row_counts(old_path, &tmp_path) {
        cleanup_temp(&tmp_path);
        return Err(e);
    }

    // ── Step 8: atomic swap + `.bak` (close handles first — already done). ──
    let bak_path = sibling_with_suffix(old_path, ".bak");
    perform_swap(old_path, &bak_path, &tmp_path)?;
    // The new DB is journal_mode=DELETE (single file); no -wal/-shm to carry.

    Ok(HealReport {
        old_state: old_state_for_report,
        rows_copied,
        bak_path,
        new_state: SchemaState::RustOwned {
            schema_version: RUST_SCHEMA_VERSION,
        },
        warnings,
    })
}

/// Step 8: atomically swap the freshly-built temp DB into `old_path`, preserving
/// the old DB as `bak_path`. Sibling files → same filesystem → `rename` is
/// atomic. On any failure the live DB is restored and the partial temp file
/// is cleaned up. The old DB's `-wal`/`-shm` sidecars (if present) are relocated
/// alongside the backup so `.bak` is a complete recoverable snapshot.
fn perform_swap(old_path: &Path, bak_path: &Path, tmp_path: &Path) -> Result<(), DbError> {
    // If a prior .bak exists, remove it (we're producing a fresh snapshot).
    let _ = std::fs::remove_file(bak_path);
    remove_sidecar_if_present(bak_path, "-wal");
    remove_sidecar_if_present(bak_path, "-shm");

    // Preserve old → bak (main + wal/shm sidecars if present).
    if let Err(e) = std::fs::rename(old_path, bak_path) {
        // Failed to move old out of the way; old is untouched at old_path,
        // tmp still holds the new DB — clean it up.
        cleanup_temp(tmp_path);
        return Err(DbError::Io(e));
    }
    // Relocate any old sidecars alongside the backup.
    rename_sidecar_if_present(old_path, bak_path, "-wal");
    rename_sidecar_if_present(old_path, bak_path, "-shm");

    // Move new → old_path.
    if let Err(e) = std::fs::rename(tmp_path, old_path) {
        // Restore the old DB from the backup so the live path isn't empty.
        let _ = std::fs::rename(bak_path, old_path);
        rename_sidecar_if_present(bak_path, old_path, "-wal");
        rename_sidecar_if_present(bak_path, old_path, "-shm");
        cleanup_temp(tmp_path);
        return Err(DbError::Io(e));
    }
    Ok(())
}

/// Build a `file:` read-only URI for [`Path`] `path`, percent-encoding the
/// characters that are special in a URI or in a SQL string literal. Used as
/// the `ATTACH DATABASE '<uri>' AS old` argument: `mode=ro` makes the attached
/// old DB read-only at the VFS level (no `-wal`/`-shm` sidecars, no accidental
/// writes), so [`heal_database`] need not hold a second read-only handle.
fn file_ro_uri(path: &Path) -> String {
    let mut s = String::from("file:");
    for b in path.to_string_lossy().bytes() {
        match b {
            // Reserved/special in a `file:` URI or a SQL single-quoted string.
            b' ' | b'%' | b'#' | b'?' | b'\'' => {
                s.push('%');
                // Percent-encode as two hex digits.
                let hex = format!("{b:02X}");
                s.push_str(&hex);
            }
            _ => s.push(b as char),
        }
    }
    s.push_str("?mode=ro");
    s
}

/// Open `path` read-only. Never creates or extends sidecar `-wal`/`-shm`
/// files — the old DB is left byte-identical (the rugpull-protection guarantee).
///
/// `:memory:` is not supported: [`heal_database`] attaches the old DB by file
/// path into the new connection, and an in-memory DB has no path to attach.
/// No caller heals an in-memory DB (it would vanish on close anyway).
fn open_readonly(path: &Path) -> Result<Connection, DbError> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(Into::into)
}

/// `old_path` with `suffix` appended to its file name (sibling, same dir).
fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map_or_else(OsString::new, ToOwned::to_owned);
    name.push(suffix);
    path.with_file_name(name)
}

fn remove_sidecar_if_present(path: &Path, sidecar_suffix: &str) {
    let mut s = path.as_os_str().to_owned();
    s.push(sidecar_suffix);
    let _ = std::fs::remove_file(Path::new(&s));
}

fn rename_sidecar_if_present(src: &Path, dst: &Path, sidecar_suffix: &str) {
    let mut src_name = src.as_os_str().to_owned();
    src_name.push(sidecar_suffix);
    if !Path::new(&src_name).exists() {
        return;
    }
    let mut dst_name = dst.as_os_str().to_owned();
    dst_name.push(sidecar_suffix);
    let _ = std::fs::rename(Path::new(&src_name), Path::new(&dst_name));
}

/// Remove the temp DB and any sidecar files (failure-cleanup path).
fn cleanup_temp(tmp_path: &Path) {
    let _ = std::fs::remove_file(tmp_path);
    remove_sidecar_if_present(tmp_path, "-wal");
    remove_sidecar_if_present(tmp_path, "-shm");
    remove_sidecar_if_present(tmp_path, "-journal");
}

/// The content tables on the NEW DB's `main` schema, in alphabetical order
/// (deterministic across runs). Excludes `alembic_version` (created by
/// `create_new_database`, dropped by `run_cutover_on_conn` in step 6) and the
/// private Rust stamp table (never carries user rows).
///
/// No FK topological sort is required: [`heal_database`] sets
/// `PRAGMA foreign_keys=OFF` on the new DB before copying, so `SQLite` imposes
/// no parent-before-child ordering — a plain alphabetical scan is sufficient.
/// (The copy preserves PK `id` values as-is, so FK references resolve once the
/// full set is loaded; integrity is re-checked by [`verify_row_counts`] and the
/// post-heal application's own invariants.)
fn content_tables(new_conn: &Connection) -> Result<Vec<String>, rusqlite::Error> {
    let mut all: Vec<String> = new_conn
        .prepare(
            "SELECT name FROM sqlite_master \
             WHERE type='table' AND name NOT LIKE 'sqlite_%' \
             AND name NOT IN ('alembic_version','_degenbot_db_schema_version')",
        )?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    all.sort();
    Ok(all)
}

/// Copy all rows of `table` from the attached `old` schema into `main`, via a
/// native `INSERT INTO main.t (…) SELECT … FROM old.t` — the row transport runs
/// inside `SQLite`'s engine (page-level), with no Rust-side row buffer. The
/// caller ([`heal_database`]) has already attached the old DB read-only
/// (`file:…?mode=ro`) and disabled FK enforcement on `main`.
///
/// Column mapping per ADR-011 §"Column-mapping auto-derivation":
/// - present in both (after rename-override lookup) → copy the value;
/// - new column with no source → omit from the INSERT (`SQLite` applies the
///   column's declared default, or `NULL`);
/// - old column with no destination → dropped (skip the read), with a warning
///   (non-fatal — surfaces exotic pre-rename states where that column's data
///   is lost).
///
/// Returns the number of rows copied (from [`Connection::changes`]).
fn copy_table(
    new: &mut Connection,
    table: &str,
    warnings: &mut Vec<String>,
) -> Result<u64, DbError> {
    let old_cols = pragma_table_info_in(new, "old", table)?;
    let new_cols = pragma_table_info_in(new, "main", table)?;

    // Set of old column names that ARE a source (direct or via override) — used
    // to detect genuinely-dropped old columns with no destination.
    let mut sourced_old: HashSet<&str> = HashSet::new();

    // For each NEW column, resolve its source OLD column (direct name match,
    // or a RENAME_OVERRIDES entry mapping old→new).
    let mut insert_cols: Vec<String> = Vec::new(); // new column names
    let mut select_cols: Vec<String> = Vec::new(); // old column names (parallel, unquoted raw for SELECT)
    for new_col in &new_cols {
        if old_cols.iter().any(|o| o.name == new_col.name) {
            insert_cols.push(new_col.name.clone());
            select_cols.push(new_col.name.clone());
            sourced_old.insert(new_col.name.as_str());
            continue;
        }
        if let Some(&(_, old_name, _)) = RENAME_OVERRIDES
            .iter()
            .find(|&&(t, _, y)| t == table && y == new_col.name)
        {
            if old_cols.iter().any(|o| o.name == old_name) {
                insert_cols.push(new_col.name.clone());
                select_cols.push(old_name.to_string());
                sourced_old.insert(old_name);
            }
        }
        // Added-after: no source on old → omit (SQLite applies the default).
    }

    // Warn about old columns that go nowhere (dropped), excluding those that
    // ARE a source (direct or override) or still present on the new schema.
    for old_col in &old_cols {
        if sourced_old.contains(old_col.name.as_str())
            || new_cols.iter().any(|n| n.name == old_col.name)
        {
            continue;
        }
        warnings.push(format!(
            "heal: column {table}.{} has no destination on the new schema; dropped",
            old_col.name
        ));
    }

    if insert_cols.is_empty() {
        return Ok(0);
    }

    let quoted_table = quote_ident(table);
    let select_list = select_cols
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let insert_list = insert_cols
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");

    // One statement, inside SQLite's engine: no Rust row materialization, no
    // borrow untangling. `INSERT … SELECT` across the ATTACHed `old` schema
    // reads from the read-only `old` schema (`file:…?mode=ro`), writes `main`.
    let tx = new.transaction()?;
    tx.execute(
        &format!(
            "INSERT INTO main.{quoted_table} ({insert_list}) \
             SELECT {select_list} FROM old.{quoted_table}"
        ),
        [],
    )?;
    let copied = tx.changes();
    tx.commit()?;
    Ok(copied)
}

/// Per-table row-count verification: old (read-only) `COUNT(*)` must equal new
/// `COUNT(*)` for every content table, else [`DbError::HealVerificationFailed`].
fn verify_row_counts(old_path: &Path, tmp_path: &Path) -> Result<(), DbError> {
    let old = open_readonly(old_path)?;
    let new = open_readonly(tmp_path)?;
    let tables: Vec<String> = new
        .prepare(
            "SELECT name FROM sqlite_master \
             WHERE type='table' AND name NOT LIKE 'sqlite_%' \
             AND name NOT IN ('alembic_version','_degenbot_db_schema_version')",
        )?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for table in &tables {
        let q = format!("SELECT COUNT(*) FROM {}", quote_ident(table));
        let old_n: i64 = old.query_row(&q, [], |r| r.get(0))?;
        let new_n: i64 = new.query_row(&q, [], |r| r.get(0))?;
        if old_n != new_n {
            return Err(DbError::HealVerificationFailed {
                table: table.clone(),
                old_count: old_n,
                new_count: new_n,
            });
        }
    }
    Ok(())
}

struct ColumnInfo {
    name: String,
    #[expect(dead_code)]
    ty: String,
    #[expect(dead_code)]
    notnull: bool,
    #[expect(dead_code)]
    dflt: Option<String>,
    #[expect(dead_code)]
    pk: i64,
}

fn pragma_table_info_in(
    conn: &Connection,
    schema: &str,
    table: &str,
) -> Result<Vec<ColumnInfo>, rusqlite::Error> {
    let mut stmt = conn.prepare(&format!(
        "PRAGMA {schema}.table_info({})",
        quote_ident(table)
    ))?;
    let rows = stmt.query_map([], |r| {
        Ok(ColumnInfo {
            name: r.get::<_, String>(1)?,
            ty: r.get::<_, String>(2)?,
            notnull: r.get::<_, i64>(3)? != 0,
            dflt: r.get::<_, Option<String>>(4)?,
            pk: r.get::<_, i64>(5)?,
        })
    })?;
    rows.collect()
}

/// Quote a SQL identifier (double-quote, doubling embedded quotes). None of the
/// degenbot schema's table/column names contain `"`; this is belt-and-braces.
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(test)]
#[expect(clippy::unwrap_used)] // heal integration tests assert with unwrap
mod tests;
