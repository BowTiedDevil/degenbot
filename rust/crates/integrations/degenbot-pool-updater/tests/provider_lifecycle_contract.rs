//! Provider-lifecycle contract for the `pool update` chunk loop.
//!
//! The chunk loop builds the RUN's transport exactly once (the
//! `AlloyProvider::new` site in `run.rs`, instrumented via
//! [`run_provider_build_count`]) and every chunk — fetches + the pre-commit
//! verification gate (`VerifyCtx` / `FullVerifyCtx`) borrows that one
//! build. A per-chunk or per-verification transport build is the resource
//! churn that intermittently surfaces as alloy's
//! `TransportErrorKind::BackendGone` ("backend connection task has
//! stopped"): each throwaway transport dies together with the throwaway
//! runtime it was built inside. This contract pins the lifecycle so the
//! churn cannot return silently.
//!
//! The run is driven end-to-end against a minimal local JSON-RPC stub
//! (answers `eth_getLogs` with an empty list; every other method is a
//! `-32601` refusal): with empty logs every chunk commits and the cursor
//! advances, so a genuine multi-chunk run completes without state the stub
//! would have to serve.

#![expect(clippy::unwrap_used)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::net::TcpStream;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use alloy::primitives::Address;
use degenbot_db::DegenbotDb;
use degenbot_pool_updater::run::reset_run_provider_build_count;
use degenbot_pool_updater::run::run_provider_build_count;
use degenbot_pool_updater::run_pool_update;
use degenbot_pool_updater::NoProgress;
use tempfile::TempDir;

const CHAIN: i64 = 8453;

/// A minimal one-request-per-connection JSON-RPC stub for the chunk loop's
/// transport: `eth_getLogs` → empty list (an empty chunk); every other
/// method (none should arrive on this path) → `-32601`.
struct StubNode {
    url: String,
    shutdown: Arc<AtomicBool>,
}

impl StubNode {
    fn spawn(chain_id: u64) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_flag = Arc::clone(&shutdown);
        listener.set_nonblocking(true).unwrap();
        thread::spawn(move || {
            while !shutdown_flag.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _addr)) => {
                        let _ = answer(&mut stream, chain_id);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self { url, shutdown }
    }
}

impl Drop for StubNode {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
    }
}

fn answer(stream: &mut TcpStream, chain_id: u64) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    while buf.len() < header_end + content_length {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&buf[header_end..]);
    let request: serde_json::Value =
        serde_json::from_str(body.trim()).unwrap_or(serde_json::Value::Null);
    let id = request["id"].clone();
    let method = request["method"].as_str().unwrap_or("").to_string();
    let payload = match method.as_str() {
        "eth_getLogs" => serde_json::json!([]),
        "eth_chainId" => serde_json::json!(format!("0x{chain_id:x}")),
        _ => {
            let error = serde_json::json!({"code": -32601, "message": "method not found"});
            let response = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": error,
            });
            return write_response(stream, &response);
        }
    };
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": payload,
    });
    write_response(stream, &response)
}

fn write_response(stream: &mut TcpStream, response: &serde_json::Value) -> std::io::Result<()> {
    let body = response.to_string();
    let head = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len(),
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()
}

/// A migrated temp DB with one ACTIVE `uniswap_v2` exchange (a known spec
/// name; its fetches stay entirely within `eth_getLogs`).
fn seeded_db(dir: &Path, file_name: &str) -> PathBuf {
    let path = dir.join(file_name);
    let (db, _state) = DegenbotDb::open_for_writes(&path).unwrap();
    let factory = Address::repeat_byte(0xf1);
    let exchange = db
        .upsert_exchange(CHAIN, "uniswap_v2", factory, None)
        .unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE exchanges SET active = 1 WHERE id = ?1",
        [exchange.id],
    )
    .unwrap();
    path
}

/// ONE transport build covers a genuine 5-chunk run (verify gate ON); a run
/// with nothing to advance builds none. One test fn: the build counter is
/// process-global and this file's scenarios must not race on it.
#[test]
fn one_transport_per_run_and_none_without_work() {
    // ── a multi-chunk run: 5 chunks, one transport. ──
    let _ = reset_run_provider_build_count();
    let stub = StubNode::spawn(CHAIN as u64);
    let dir = TempDir::new().unwrap();
    let path = seeded_db(dir.path(), "multi_chunk.db");
    let report = run_pool_update(
        &path,
        CHAIN,
        // from 1 → 50 at 10 blocks per chunk = 5 committed chunks.
        Some(50),
        10,
        &stub.url,
        Arc::new(AtomicBool::new(false)),
        Arc::new(NoProgress),
        // The verification gate is ON — the lifecycle contract covers the
        // gated path (the gate borrows the run's one transport).
        true,
        None,
        false,
    )
    .unwrap();
    assert_eq!(
        report.chunks_committed, 5,
        "the run must genuinely span multiple chunks"
    );
    assert_eq!(
        run_provider_build_count(),
        1,
        "one transport build per multi-chunk run"
    );

    // ── a run with no active exchanges: zero builds, zero dials. ──
    let _ = reset_run_provider_build_count();
    let empty = TempDir::new().unwrap();
    let empty_path = empty.path().join("empty.db");
    DegenbotDb::open_for_writes(&empty_path).unwrap();
    let stub2 = StubNode::spawn(CHAIN as u64);
    let report = run_pool_update(
        &empty_path,
        CHAIN,
        Some(50),
        10,
        &stub2.url,
        Arc::new(AtomicBool::new(false)),
        Arc::new(NoProgress),
        true,
        None,
        false,
    )
    .unwrap();
    assert_eq!(report.chunks_committed, 0);
    assert_eq!(
        run_provider_build_count(),
        0,
        "a run with nothing to advance never builds a transport"
    );
}
