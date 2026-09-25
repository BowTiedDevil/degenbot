//! Thin `PyO3` seams for the remaining production database reads.

use std::collections::HashSet;
use std::path::PathBuf;

use alloy::primitives::Address;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::bot::py_bot_io::parse_address_for_call;
use crate::db::db_err_to_py;
use degenbot_db::DegenbotDb;

/// Resolve chain-scoped ERC-20 addresses to database row ids.
///
/// Addresses are normalized to EIP-55 checksums, duplicate inputs are
/// collapsed, and missing rows are omitted from the returned dictionary.
/// SQLite open/query work runs with the GIL detached.
#[pyfunction]
#[expect(clippy::needless_pass_by_value)]
pub(crate) fn db_resolve_token_ids(
    py: Python<'_>,
    database_path: &str,
    chain_id: i64,
    addresses: Vec<String>,
) -> PyResult<Py<PyDict>> {
    let unique: HashSet<Address> = addresses
        .iter()
        .map(|address| parse_address_for_call(address).map(Address::from))
        .collect::<PyResult<HashSet<_>>>()?;
    let mut rust_addresses: Vec<Address> = unique.into_iter().collect();
    rust_addresses.sort_by_key(|address| address.to_checksum(None));
    if rust_addresses.is_empty() {
        return Ok(PyDict::new(py).unbind());
    }

    let path = PathBuf::from(database_path);
    let resolved = py.detach(|| -> Result<_, PyErr> {
        let (db, _state) = DegenbotDb::open(&path).map_err(|error| db_err_to_py(&error))?;
        db.fetch_token_ids_by_address(chain_id, &rust_addresses)
            .map_err(|error| db_err_to_py(&error))
    })?;

    let result = PyDict::new(py);
    let mut rows: Vec<(String, u64)> = resolved
        .into_iter()
        .map(|(address, token_id)| (address.to_checksum(None), token_id))
        .collect();
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    for (address, token_id) in rows {
        result.set_item(address, token_id)?;
    }
    Ok(result.unbind())
}

/// Return `(v2v3_count, v2v3_max_id, v4_count, v4_max_id)` for a chain.
///
/// The managed-pool branch is scoped through its pool manager's chain. The
/// caller owns the fail-open policy; this seam reports DB/query failures as
/// `ValueError` after releasing the GIL.
#[pyfunction]
pub(crate) fn db_fetch_graph_edition(
    py: Python<'_>,
    database_path: &str,
    chain_id: i64,
) -> PyResult<(i64, i64, i64, i64)> {
    let path = PathBuf::from(database_path);
    let edition = py.detach(|| -> Result<_, PyErr> {
        let (db, _state) = DegenbotDb::open(&path).map_err(|error| db_err_to_py(&error))?;
        db.fetch_graph_edition(chain_id)
            .map_err(|error| db_err_to_py(&error))
    })?;

    Ok((
        edition.v2v3_count,
        edition.v2v3_max_id,
        edition.v4_count,
        edition.v4_max_id,
    ))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use pyo3::types::PyModule;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    static NEXT_TEMP_DB: AtomicU64 = AtomicU64::new(0);

    fn temp_db(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "degenbot_rs_{label}_{}_{}.db",
            std::process::id(),
            NEXT_TEMP_DB.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn seed(path: &std::path::Path) {
        let (db, _state) = DegenbotDb::open_for_writes(path).unwrap();
        let conn = db.lock();
        conn.execute_batch(
            "INSERT INTO erc20_tokens (id, chain, address) VALUES
               (1, 1, '0x1111111111111111111111111111111111111111'),
               (2, 1, '0x2222222222222222222222222222222222222222'),
               (3, 10, '0x1111111111111111111111111111111111111111');
             INSERT INTO exchanges (id, chain_id, name, active, factory) VALUES
               (1, 1, 'chain-one', 1, '0x1111111111111111111111111111111111111111'),
               (2, 10, 'chain-ten', 1, '0x1111111111111111111111111111111111111111');
             INSERT INTO pools
               (id, address, chain, kind, token0_id, token1_id, exchange_id) VALUES
               (2, '0x1111111111111111111111111111111111111111', 1, 'uniswap_v2', 1, 2, 1),
               (7, '0x2222222222222222222222222222222222222222', 1, 'uniswap_v3', 1, 2, 1),
               (100, '0x3333333333333333333333333333333333333333', 10, 'uniswap_v2', 3, 3, 2);
             INSERT INTO pool_managers (id, address, chain, kind, exchange_id) VALUES
               (20, '0x1111111111111111111111111111111111111111', 1, 'uniswap_v4', 1),
               (21, '0x1111111111111111111111111111111111111111', 10, 'uniswap_v4', 2);
             INSERT INTO managed_pools (id, kind, manager_id) VALUES
               (4, 'uniswap_v4', 20), (9, 'uniswap_v4', 20), (100, 'uniswap_v4', 21);",
        )
        .unwrap();
    }

    #[test]
    fn wrappers_return_documented_python_shapes() {
        let path = temp_db("read_shapes");
        seed(&path);
        let path_string = path.to_string_lossy().into_owned();

        Python::attach(|py| {
            let tokens = db_resolve_token_ids(
                py,
                &path_string,
                1,
                vec![
                    "0x1111111111111111111111111111111111111111".to_string(),
                    "0x3333333333333333333333333333333333333333".to_string(),
                    "0x1111111111111111111111111111111111111111".to_string(),
                    "0x2222222222222222222222222222222222222222".to_string(),
                ],
            )
            .unwrap();
            let tokens = tokens.bind(py);
            assert_eq!(tokens.len(), 2);
            assert_eq!(
                tokens
                    .get_item("0x1111111111111111111111111111111111111111")
                    .unwrap()
                    .unwrap()
                    .extract::<u64>()
                    .unwrap(),
                1
            );
            assert_eq!(
                db_fetch_graph_edition(py, &path_string, 1).unwrap(),
                (2, 7, 2, 9)
            );
            assert_eq!(
                db_fetch_graph_edition(py, &path_string, 999).unwrap(),
                (0, 0, 0, 0)
            );
        });

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn read_seams_are_registered_on_db_submodule() {
        Python::attach(|py| {
            let root = PyModule::new(py, "degenbot._ffi").unwrap();
            crate::db::add_db_module(&root).unwrap();
            let db = root.getattr("db").unwrap();
            assert!(db.hasattr("db_resolve_token_ids").unwrap());
            assert!(db.hasattr("db_fetch_graph_edition").unwrap());
        });
    }

    #[test]
    fn sqlite_wait_releases_gil() {
        let path = temp_db("gil_release");
        seed(&path);
        let path_string = path.to_string_lossy().into_owned();
        let (locking_db, _state) = DegenbotDb::open_for_writes(&path).unwrap();
        let lock_connection = locking_db.lock();
        lock_connection.execute_batch("BEGIN EXCLUSIVE").unwrap();

        let (probe_tx, probe_rx) = mpsc::channel();
        let probe = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            Python::attach(|py| {
                let _ = py.None();
            });
            probe_tx.send(()).unwrap();
        });
        let caller_path = path_string.clone();
        let caller = thread::spawn(move || {
            Python::attach(|py| {
                db_fetch_graph_edition(py, &caller_path, 1).unwrap();
            });
        });

        probe_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("another Python thread could not acquire the GIL while SQLite was blocked");
        drop(lock_connection);
        drop(locking_db);
        caller.join().unwrap();
        probe.join().unwrap();
        let _ = std::fs::remove_file(path);
    }
}
