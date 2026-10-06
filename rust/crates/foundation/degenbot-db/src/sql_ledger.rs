//! The statement ledger + canonical DB dump — the SQL half of a golden capture
//! (ADR-068 D3; GLOSSARY "statement ledger").
//!
//! **Test/bench-only surface.** Everything here is behind the crate's
//! `sql-ledger` feature (default OFF): production paths compile NONE of this
//! module, so `DegenbotDb::open*` installs no trace hooks and pays no capture
//! cost. The ONLY constructor that installs hooks is [`LedgerDb::open_for_writes`]
//! — structural zero-overhead proof is the feature gate itself plus that single
//! install site (grep `trace_v2` under `src/`: exactly this file).
//!
//! # What it records (one run over the wrapper's connection)
//!
//! Per statement, in execution order: the normalized SQL text (whitespace
//! collapsed, string/numeric literals → `?`, `?N` placeholder indexes → `?`),
//! the bind-placeholder count, the rows the statement changed, and the profile
//! duration in µs ([`StatementRecord`]). Hooks are `rusqlite` `trace_v2` with
//! `SQLITE_TRACE_STMT | SQLITE_TRACE_PROFILE`; rows-changed comes from
//! `sqlite3_changes64` at the PROFILE event (the statement has just finished).
//!
//! The rusqlite trace callback is a bare `fn(TraceEvent)` with no user context,
//! so the capture routes through ONE process-global session slot
//! ([`LedgerDb::open_for_writes`] arms it; `Drop` disarms). One capture per
//! process; a second arm refuses with [`DbError::LedgerArmed`].
//!
//! # Golden artifacts (machine-emitted, drift-gate + negative-probe idioms)
//!
//! [`ledger_golden_json`] / [`dump_tables_golden_json`] serialize the
//! DETERMINISTIC projection: statement order, normalized SQL, arg count, rows
//! changed; dump tables in caller order, columns in table order, rows sorted
//! by their canonical JSON form, hex values lowercased. The gate is
//! byte-identical regeneration through this exact code, so the volatile
//! `profile_us` wall-clock measurement is deliberately NOT in the committed
//! golden (it would break byte-identity); it stays available on
//! [`LedgerDb::records`] for the perf program's live measurements
//! (docs/updater-rpc-sql-survey.md). The artifact writers are the same
//! functions the gate re-runs.
//!
//! # Determinism scope
//!
//! Statement ORDER is deterministic for a golden corpus whose apply iterates
//! its write sets in a stable order. Multi-tick upserts chunk a hashbrown map
//! by iteration order; chunk membership can only shift the per-statement
//! `rows_changed` split once a single multi-row INSERT exceeds the bind cap
//! (> 249 rows — far above this corpus's per-chunk tick counts), and the
//! drift gate would flag any such drift honestly.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use rusqlite::trace::{TraceEvent, TraceEventCodes};
use rusqlite::Connection;

use crate::connection::DegenbotDb;
use crate::error::DbError;
use crate::migrate::SchemaState;

/// Schema tag of the statement-ledger golden JSON.
pub const LEDGER_SCHEMA: &str = "degenbot.statement-ledger/v1";

/// Schema tag of the canonical DB-dump golden JSON.
pub const DUMP_SCHEMA: &str = "degenbot.db-dump/v1";

/// Project a key-sorted `BTreeMap` into a `serde_json::Map` whose insertion
/// order IS the sorted order, so the map's own serialization emits the keys
/// sorted under either backend (`BTreeMap` storage by default;
/// insertion-ordered `IndexMap` storage under `serde_json/preserve_order`).
#[must_use]
fn object_from_sorted(sorted: BTreeMap<String, serde_json::Value>) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (key, value) in sorted {
        map.insert(key, value);
    }
    serde_json::Value::Object(map)
}

/// A JSON object that serializes with its keys sorted, under either `serde_json`
/// map backend. Deduped and ordered through a `BTreeMap` (whose `Serialize`
/// visits keys sorted), then projected in that order into the `Map` — so
/// feature unification turning on `serde_json`'s `preserve_order` cannot
/// change what the golden writers emit. Every object a golden artifact
/// carries must go through this.
#[must_use]
fn sorted_object<const N: usize>(entries: [(&str, serde_json::Value); N]) -> serde_json::Value {
    object_from_sorted(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    )
}

/// The backend-proof canonical JSON form of a value: the serialization with
/// every object's keys sorted. Used as the dump's row sort key — a raw
/// `to_string` would key on `Map` iteration order under `preserve_order`.
#[must_use]
fn canonical_form(value: &serde_json::Value) -> String {
    serde_json::to_string(&canonicalized(value)).unwrap_or_default()
}

/// Recursively rebuild a value, routing every object through
/// [`sorted_object`]'s sorted-`BTreeMap` projection.
#[must_use]
fn canonicalized(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => object_from_sorted(
            map.iter()
                .map(|(key, child)| (key.clone(), canonicalized(child)))
                .collect(),
        ),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonicalized).collect())
        }
        other => other.clone(),
    }
}

/// One captured statement, in execution order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementRecord {
    /// Whitespace-collapsed SQL with literals → `?` and `?N` → `?`.
    pub normalized_sql: String,
    /// Bind-placeholder count in the raw SQL text.
    pub arg_count: usize,
    /// Rows the statement changed (`sqlite3_changes64` at PROFILE).
    pub rows_changed: i64,
    /// Wall-clock duration in µs (volatile; NOT in the committed golden).
    pub profile_us: u64,
}

/// A STMT event awaiting its pairing PROFILE event.
struct PendingStmt {
    normalized_sql: String,
    arg_count: usize,
    mutates: bool,
}

/// The raw connection handle, wrapped for the session slot. The handle is
/// only ever DEREFERENCED inside the trace callback — i.e. on the thread
/// executing statements against that very connection — so the `Send` impl
/// carries a value between arm/read sites without ever crossing a thread
/// while a statement runs.
#[derive(Clone, Copy)]
struct DbHandle(*mut rusqlite::ffi::sqlite3);

// SAFETY: see the struct docs — the pointer is stored in the capture slot and
// used only by the trace hook of the connection it came from.
unsafe impl Send for DbHandle {}

/// The armed capture state. Holds the raw connection handle because the trace
/// callback is a context-free `fn` — `sqlite3_changes64` needs the `sqlite3*`
/// to read the just-finished statement's row count.
struct Session {
    db: DbHandle,
    pending: Vec<PendingStmt>,
    records: Vec<StatementRecord>,
}

/// The ONE process-global capture slot (see module docs for why it exists).
static SESSION: Mutex<Option<Session>> = Mutex::new(None);

/// The `trace_v2` callback: STMT opens a pending record, PROFILE finalizes it.
fn trace_hook(evt: TraceEvent<'_>) {
    let Ok(mut slot) = SESSION.lock() else {
        return; // a poisoned slot must not crash the profiled run
    };
    let Some(session) = slot.as_mut() else {
        return;
    };
    match evt {
        TraceEvent::Stmt(_stmt, sql) => {
            session.pending.push(PendingStmt {
                normalized_sql: normalize_sql(sql),
                arg_count: count_placeholders(sql),
                mutates: statement_mutates(sql),
            });
        }
        TraceEvent::Profile(_stmt, elapsed) => {
            if let Some(pending) = session.pending.pop() {
                // `sqlite3_changes64` is only meaningful after a mutating
                // statement — a SELECT inherits the previous statement's
                // count, so non-mutating statements record 0.
                let rows_changed = if pending.mutates {
                    // SAFETY: `session.db` is the handle of the connection
                    // running this statement (installed at arm time, valid
                    // until the `LedgerDb` drop disarms); at PROFILE the
                    // statement has just finished, so `sqlite3_changes64` is
                    // its row count.
                    unsafe { rusqlite::ffi::sqlite3_changes64(session.db.0) }
                } else {
                    0
                };
                session.records.push(StatementRecord {
                    normalized_sql: pending.normalized_sql,
                    arg_count: pending.arg_count,
                    rows_changed,
                    profile_us: u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX),
                });
            }
        }
        _ => {}
    }
}

/// A [`DegenbotDb`] write handle with the statement-ledger trace hooks
/// installed on its underlying connection. The ONLY hook install site in the
/// crate (module docs: structural zero-overhead when the `sql-ledger` feature
/// is off — production builds compile none of this).
///
/// Hooks arm AFTER the open sequence completes, so the schema-gate/heal DDL is
/// not captured: the ledger starts at the caller's first post-open statement
/// (the run seam's specs load, the chunk apply, the stamp advance).
pub struct LedgerDb {
    db: DegenbotDb,
}

impl LedgerDb {
    /// Open a write-capable handle with the statement-ledger hooks installed.
    /// Refuses with [`DbError::LedgerArmed`] if a capture session is already
    /// armed in this process.
    ///
    /// # Errors
    ///
    /// Same conditions as [`DegenbotDb::open_for_writes`], plus
    /// [`DbError::LedgerArmed`] for a double arm.
    pub fn open_for_writes(path: &Path) -> Result<(Self, SchemaState), DbError> {
        let (db, state) = DegenbotDb::open_for_writes(path)?;
        {
            let guard = db.lock();
            let handle = unsafe { guard.handle() };
            guard.trace_v2(
                TraceEventCodes::SQLITE_TRACE_STMT | TraceEventCodes::SQLITE_TRACE_PROFILE,
                Some(trace_hook),
            );
            let mut slot = SESSION
                .lock()
                .map_err(|_| DbError::Decode("sql-ledger session slot poisoned".to_string()))?;
            if slot.is_some() {
                return Err(DbError::LedgerArmed);
            }
            *slot = Some(Session {
                db: DbHandle(handle),
                pending: Vec::new(),
                records: Vec::new(),
            });
        }
        Ok((Self { db }, state))
    }

    /// The traced handle to hand the run seam (`run_pool_update_on_db`).
    pub fn db(&self) -> &DegenbotDb {
        &self.db
    }

    /// The captured statements in execution order. The live record carries the
    /// volatile `profile_us` — the committed golden is the deterministic
    /// projection via [`ledger_golden_json`].
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Decode`] only if the session slot was poisoned
    /// mid-capture.
    pub fn records(&self) -> Result<Vec<StatementRecord>, DbError> {
        let slot = SESSION
            .lock()
            .map_err(|_| DbError::Decode("sql-ledger session slot poisoned".to_string()))?;
        Ok(slot.as_ref().map(|s| s.records.clone()).unwrap_or_default())
    }
}

impl Drop for LedgerDb {
    fn drop(&mut self) {
        // Disarm BEFORE the connection drops: clear the hooks (so no further
        // statement routes into the session) and release the slot (which owns
        // the raw connection handle).
        self.db.lock().trace_v2(TraceEventCodes::empty(), None);
        if let Ok(mut slot) = SESSION.lock() {
            *slot = None;
        }
    }
}

/// Serialize the captured statements as the canonical statement-ledger golden
/// JSON (schema tag + ordered statements; the deterministic projection — see
/// module docs). The writer the drift gate re-runs.
#[must_use]
pub fn ledger_golden_json(records: &[StatementRecord]) -> String {
    let statements = records
        .iter()
        .map(|r| {
            // `BTreeMap` serialization visits keys sorted (`arg_count`,
            // `rows_changed`, `sql`) under both `serde_json` map backends.
            sorted_object([
                ("sql", serde_json::Value::from(r.normalized_sql.as_str())),
                ("arg_count", serde_json::Value::from(r.arg_count)),
                ("rows_changed", serde_json::Value::from(r.rows_changed)),
            ])
        })
        .collect::<Vec<_>>();
    let doc = sorted_object([
        ("schema", serde_json::Value::from(LEDGER_SCHEMA)),
        ("statements", serde_json::Value::from(statements)),
    ]);
    // Values are strings/ints only — serialization cannot fail.
    format!(
        "{}\n",
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    )
}

/// Serialize the touched tables as the canonical DB-dump golden JSON: tables
/// in caller order, columns in table order, rows sorted by their canonical
/// JSON form, `0x`-hex text lowercased, blobs as lowercase hex. The writer the
/// drift gate re-runs.
///
/// # Errors
///
/// [`DbError::Sqlite`] if a table cannot be read.
pub fn dump_tables_golden_json(conn: &Connection, tables: &[&str]) -> Result<String, DbError> {
    let mut tables_json = Vec::with_capacity(tables.len());
    for table in tables {
        let mut stmt = conn.prepare(&format!("SELECT * FROM {table}"))?;
        let columns: Vec<String> = stmt
            .column_names()
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let mut rows: Vec<serde_json::Value> = Vec::new();
        let mut cursor = stmt.query([])?;
        while let Some(row) = cursor.next()? {
            let mut cells = Vec::with_capacity(columns.len());
            for i in 0..columns.len() {
                cells.push(value_ref_to_json(row.get_ref(i)?));
            }
            rows.push(serde_json::Value::Array(cells));
        }
        // Canonical row order: sort by the row's backend-proof canonical
        // JSON form (object keys sorted) — never by a raw serialization
        // whose key order would follow `Map` iteration under
        // `preserve_order`.
        rows.sort_by_cached_key(canonical_form);
        tables_json.push(sorted_object([
            ("table", serde_json::Value::from(*table)),
            ("columns", serde_json::Value::from(columns)),
            ("rows", serde_json::Value::from(rows)),
        ]));
    }
    let doc = sorted_object([
        ("schema", serde_json::Value::from(DUMP_SCHEMA)),
        ("tables", serde_json::Value::from(tables_json)),
    ]);
    // Values are strings/ints only — serialization cannot fail.
    Ok(format!(
        "{}\n",
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    ))
}

/// One SQLite value → JSON: integers/reals as numbers, blobs as lowercase
/// `0x` hex, and `0x`-hex TEXT lowercased (hex-normalized identifiers:
/// addresses, pool hashes). Everything else is the text as stored.
fn value_ref_to_json(value: rusqlite::types::ValueRef<'_>) -> serde_json::Value {
    use rusqlite::types::ValueRef as V;
    match value {
        V::Null => serde_json::Value::Null,
        V::Integer(i) => serde_json::Value::from(i),
        V::Real(f) => serde_json::Value::from(f),
        V::Text(t) => {
            let s = String::from_utf8_lossy(t);
            serde_json::Value::from(hex_normalize(&s))
        }
        V::Blob(b) => {
            use std::fmt::Write as _;
            let mut hex = String::with_capacity(2 + b.len() * 2);
            hex.push_str("0x");
            for byte in b {
                let _ = write!(hex, "{byte:02x}");
            }
            serde_json::Value::from(hex)
        }
    }
}

/// Lowercase a `0x`-prefixed hex string; leave anything else untouched.
fn hex_normalize(s: &str) -> String {
    if s.len() > 2 && s.starts_with("0x") && s[2..].bytes().all(|b| b.is_ascii_hexdigit()) {
        s.to_ascii_lowercase()
    } else {
        s.to_string()
    }
}

/// Normalize SQL text for the ledger: collapse whitespace runs to one space,
/// strip `--` line comments, mask string/numeric literals to `?`, collapse
/// `?N` placeholder indexes to `?`.
#[must_use]
pub fn normalize_sql(sql: &str) -> String {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len());
    // Whether the previous emitted char makes a following digit an identifier
    // fragment (`v3_tick`, `"col1"`) rather than a numeric literal.
    let mut wordish_prev = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '-' if chars.get(i + 1) == Some(&'-') => {
                // Line comment: dropped (the newline whitespace-collapse covers it).
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '\'' => {
                // String literal (with '' escape): mask to `?`.
                let mut j = i + 1;
                while j < chars.len() {
                    if chars[j] == '\'' {
                        if chars.get(j + 1) == Some(&'\'') {
                            j += 2;
                            continue;
                        }
                        break;
                    }
                    j += 1;
                }
                i = (j + 1).min(chars.len());
                out.push('?');
                wordish_prev = false;
            }
            '?' => {
                out.push('?');
                // Collapse an explicit placeholder index (`?1`) into `?`.
                while chars.get(i + 1).is_some_and(char::is_ascii_digit) {
                    i += 1;
                }
                i += 1;
                wordish_prev = false;
            }
            c if c.is_ascii_digit() && !wordish_prev => {
                // Numeric literal (decimal or 0x hex): mask to `?`.
                let mut j = i;
                if c == '0' && matches!(chars.get(i + 1), Some('x' | 'X')) {
                    j += 2;
                }
                while j < chars.len() && (chars[j].is_ascii_hexdigit() || chars[j] == '.') {
                    j += 1;
                }
                out.push('?');
                i = j;
                wordish_prev = false;
            }
            c if c.is_whitespace() => {
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
                wordish_prev = false;
                i += 1;
            }
            c => {
                out.push(c);
                wordish_prev =
                    c.is_ascii_alphanumeric() || matches!(c, '_' | '"' | '.' | ']' | '`');
                i += 1;
            }
        }
    }
    out
}

/// Whether a statement can change rows (`sqlite3_changes64` is only defined
/// for these — a SELECT would otherwise inherit the previous write's count).
fn statement_mutates(sql: &str) -> bool {
    let trimmed = sql.trim_start();
    let head = trimmed
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or("");
    matches!(
        head.to_ascii_uppercase().as_str(),
        "INSERT" | "UPDATE" | "DELETE" | "REPLACE"
    )
}

/// Count bind placeholders in the RAW SQL text (`?` / `?N` outside quotes and
/// comments; occurrences == binds for the apply paths, which use sequential
/// placeholders).
#[must_use]
pub fn count_placeholders(sql: &str) -> usize {
    let chars: Vec<char> = sql.chars().collect();
    let mut count = 0usize;
    let (mut in_single, mut in_double) = (false, false);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_single {
            if c == '\'' {
                if chars.get(i + 1) == Some(&'\'') {
                    i += 2;
                    continue;
                }
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if c == '"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        match c {
            '\'' => in_single = true,
            '"' => in_double = true,
            '-' if chars.get(i + 1) == Some(&'-') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '?' => {
                count += 1;
                while chars.get(i + 1).is_some_and(char::is_ascii_digit) {
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    count
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn normalize_masks_literals_and_collapses_whitespace() {
        assert_eq!(
            normalize_sql("INSERT INTO pools (address, chain) VALUES ('0xAbC', 12345)"),
            "INSERT INTO pools (address, chain) VALUES (?, ?)"
        );
        assert_eq!(
            normalize_sql("SELECT id FROM t WHERE a = 'it''s' AND b = 1"),
            "SELECT id FROM t WHERE a = ? AND b = ?"
        );
        assert_eq!(
            normalize_sql("SELECT  a,\n  b\t FROM t WHERE x=5 LIMIT 100"),
            "SELECT a, b FROM t WHERE x=? LIMIT ?"
        );
    }

    #[test]
    fn normalize_keeps_identifiers_and_placeholder_indexes() {
        // Digits inside identifiers are not literals.
        assert_eq!(
            normalize_sql("SELECT v3_tick FROM t WHERE \"col1\" = 7"),
            "SELECT v3_tick FROM t WHERE \"col1\" = ?"
        );
        // Explicit placeholder indexes collapse to bare `?`.
        assert_eq!(
            normalize_sql("UPDATE t SET a = ?1, b = ?2 WHERE id = ?1"),
            "UPDATE t SET a = ?, b = ? WHERE id = ?"
        );
    }

    #[test]
    fn normalize_drops_line_comments() {
        assert_eq!(
            normalize_sql("SELECT 1 -- the count\nFROM t"),
            "SELECT ? FROM t"
        );
    }

    #[test]
    fn count_placeholders_counts_binds_only() {
        assert_eq!(count_placeholders("INSERT INTO t VALUES (?, ?, ?)"), 3);
        assert_eq!(count_placeholders("UPDATE t SET a = ?1 WHERE id = ?1"), 2);
        assert_eq!(count_placeholders("SELECT * FROM t WHERE s = 'a ? b'"), 0);
        assert_eq!(count_placeholders("SELECT 1 -- ? trailing\nFROM t"), 0);
    }

    #[test]
    fn ledger_golden_json_is_canonical_and_byte_stable() {
        let records = vec![
            StatementRecord {
                normalized_sql: "SELECT id FROM exchanges WHERE chain_id = ?".to_string(),
                arg_count: 1,
                rows_changed: 0,
                profile_us: 999,
            },
            StatementRecord {
                normalized_sql: "BEGIN".to_string(),
                arg_count: 0,
                rows_changed: 0,
                profile_us: 3,
            },
        ];
        let first = ledger_golden_json(&records);
        let second = ledger_golden_json(&records);
        assert_eq!(first, second, "same input must serialize byte-identical");
        // The volatile profile_us must NOT leak into the committed artifact.
        assert!(!first.contains("999"));
        // Order + shape are load-bearing.
        assert!(first.contains("degenbot.statement-ledger/v1"));
        assert!(first.starts_with('{'));

        // Key order must be the sorted (BTreeMap) order — not the call-site
        // insertion order — so a `preserve_order` unification cannot drift
        // the committed artifact's key layout.
        let arg_count = first.find("arg_count").expect("arg_count key present");
        let rows_changed = first
            .find("rows_changed")
            .expect("rows_changed key present");
        let sql = first.find("sql").expect("sql key present");
        assert!(
            arg_count < rows_changed && rows_changed < sql,
            "statement keys must serialize in sorted order"
        );
    }

    #[test]
    fn dump_is_hex_normalized_row_sorted_and_canonical() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE t (id INTEGER, address VARCHAR(42), note TEXT);\n\
             INSERT INTO t VALUES (2, '0xAbCd', 'x');\n\
             INSERT INTO t VALUES (1, '0xabcdef', 'y');",
        )
        .unwrap();
        let json = dump_tables_golden_json(&conn, &["t"]).unwrap();
        // Hex identifiers lowercased.
        assert!(json.contains("0xabcd"));
        assert!(!json.contains("0xAbCd"));
        // Rows sorted by canonical form: the id-1 row before the id-2 row.
        let one = json.find("\"0xabcdef\"").expect("id-1 row present");
        let two = json.find("\"0xabcd\",").expect("id-2 row present");
        assert!(one < two, "rows must be in canonical sorted order");
        assert!(json.contains("degenbot.db-dump/v1"));
        // Table-object key order must be the sorted (BTreeMap) order too —
        // `rfind` because the bare word also occurs in the top-level
        // `"tables"` key.
        let columns = json.find("columns").expect("columns key present");
        let rows = json.find("rows").expect("rows key present");
        let table = json.rfind("table").expect("table key present");
        assert!(
            columns < rows && rows < table,
            "dump table keys must serialize in sorted order"
        );
        // Regeneration through the same writer is byte-identical.
        assert_eq!(json, dump_tables_golden_json(&conn, &["t"]).unwrap());
    }

    #[test]
    fn dump_errors_on_missing_table() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(dump_tables_golden_json(&conn, &["nope"]).is_err());
    }
}
