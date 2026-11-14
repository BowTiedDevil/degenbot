//! The statement-ledger pattern every `sql-ledger` program in this package shares.
//!
//! A generator's SQL golden is two artifacts produced from ONE replay: the ordered
//! statement ledger the recorder captured ([`ledger_golden_json`]) and the canonical
//! dump of the tables the chunk apply touched ([`dump_tables_golden_json`]). Both
//! live in `degenbot-db`'s test/bench-only `sql-ledger` feature, so this module is
//! behind the package's `sql-ledger` forward: a default build of the support library
//! compiles none of it.
//!
//! The pair is read through one function on purpose. Two generators that each
//! reached for the ledger's records and the table dump on their own would drift in
//! *which* tables they dump or in *when* they take the lock, and the drift would show
//! up as a fixture diff nobody can attribute. The replay-side consumers
//! (`wave2_capture_replay`, `wave3_capture_replay`) call the same two writers, which
//! is what makes the committed goldens comparable in both directions.

use degenbot::db::sql_ledger::{dump_tables_golden_json, ledger_golden_json, LedgerDb};

/// Read the two SQL goldens from an armed [`LedgerDb`]: the ordered statement ledger
/// and the canonical dump of `tables`.
///
/// `tables` is the caller's list — the dump is table-selected, so the list IS part of
/// the golden's contract and every change to it is a fixture change.
///
/// # Errors
///
/// Returns the writer's error, stringified, when the ledger's records cannot be read
/// (an unarmed capture session) or when a dumper fails on the open connection.
pub fn goldens(ledger: &LedgerDb, tables: &[&str]) -> Result<(String, String), String> {
    let records = ledger
        .records()
        .map_err(|err| format!("ledger records: {err}"))?;
    let ledger_json = ledger_golden_json(&records);
    let connection = ledger.db().lock();
    let dump_json =
        dump_tables_golden_json(&connection, tables).map_err(|err| format!("db dump: {err}"))?;
    drop(connection);
    Ok((ledger_json, dump_json))
}
