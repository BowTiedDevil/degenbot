//! Integration tests for the `pool verify` provider lifecycle.
//!
//! The verify arm builds ONE chain-verified provider per run, on the
//! process-wide shared runtime (the pattern `run_pool_update` uses for its
//! chunk loop) — never a throwaway runtime + transport per target family,
//! whose connection tasks die with the ad-hoc runtime (the alloy
//! `BackendGone` churn). These tests pin the lifecycle through the public
//! `run()` seam against a minimal local JSON-RPC stub:
//!
//! - GREEN run (matching chain, an empty tracked map verifies without any
//!   further RPC): exactly one build.
//! - wrong-chain endpoint: REFUSED before any comparison (the chain-binding
//!   contract), for BOTH families.
//! - pre-RPC refusals (pool unknown): zero builds, zero dials.

#![expect(clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::net::TcpStream;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use alloy::primitives::{Address, B256};
use degenbot_cli_core::pool::{reset_verify_provider_build_count, verify_provider_build_count};
use degenbot_cli_core::{run, CliContext, Command, ExitCode, PoolCommand, PoolFamily, Prompter};
use degenbot_config::MapEnv;
use degenbot_db::{DegenbotDb, V3PoolRowInput, V4PoolRowInput};
use tempfile::TempDir;

const CHAIN: i64 = 8453;

/// The build counter these tests assert on is process-global (one
/// `AtomicU64` in `pool.rs`), so the tests serialize their reset → run →
/// assert sections on this guard instead of racing on it.
static BUILD_COUNTER: Mutex<()> = Mutex::new(());

/// A never-confirming prompter (neither pool arm prompts).
struct NoPrompt;

impl Prompter for NoPrompt {
    fn confirm(&self, _message: &str, _default: bool) -> bool {
        false
    }
}

/// A minimal one-request-per-connection JSON-RPC stub that binds to
/// `chain_id` (`eth_chainId` → the served id) and answers nothing else:
/// every other method is a `-32601` refusal. Counts the `eth_chainId` reads
/// (the chain-bind round trip).
struct StubNode {
    url: String,
    chain_id_reads: Arc<AtomicUsize>,
    shutdown: Arc<AtomicBool>,
}

impl StubNode {
    fn spawn(chain_id: u64) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let chain_id_reads = Arc::new(AtomicUsize::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let reads = Arc::clone(&chain_id_reads);
        let shutdown_flag = Arc::clone(&shutdown);
        listener.set_nonblocking(true).unwrap();
        thread::spawn(move || {
            while !shutdown_flag.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _addr)) => {
                        let _ = answer(&mut stream, chain_id, &reads);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            url,
            chain_id_reads,
            shutdown,
        }
    }

    fn chain_id_reads(&self) -> usize {
        self.chain_id_reads.load(Ordering::Acquire)
    }
}

impl Drop for StubNode {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
    }
}

fn answer(
    stream: &mut TcpStream,
    chain_id: u64,
    chain_id_reads: &AtomicUsize,
) -> std::io::Result<()> {
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
    let payload = if method == "eth_chainId" {
        chain_id_reads.fetch_add(1, Ordering::AcqRel);
        serde_json::json!(format!("0x{chain_id:x}"))
    } else {
        // The GREEN empty-map verify performs NO further reads; anything
        // that does arrive is a refusal the run reports.
        let error = serde_json::json!({"code": -32601, "message": "method not found"});
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": error,
        });
        return write_response(stream, &response);
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

fn env(rpc_url: &str) -> MapEnv {
    let mut map = BTreeMap::new();
    map.insert("DEGENBOT_DEFAULT_CHAIN_ID".to_string(), CHAIN.to_string());
    map.insert(
        "DEGENBOT_RPC_HTTP_CHAINID_8453".to_string(),
        rpc_url.to_string(),
    );
    MapEnv::new(map)
}

fn ctx_for(rpc_url: &str, db: &Path) -> CliContext<'static> {
    let e = Box::leak(Box::new(env(rpc_url)));
    CliContext::new(e).with_database(db.display().to_string())
}

/// A migrated temp DB with one V3 pool row (no stored ticks — an empty
/// tracked map, which the shared verifier checks GREEN without RPC) and its
/// exchange. Returns the pool address the verify arm should name.
fn seed_v3_pool(db_path: &Path) -> Address {
    let (db, _state) = DegenbotDb::open_for_writes(db_path).unwrap();
    let factory = Address::repeat_byte(0xf1);
    let exchange = db
        .upsert_exchange(CHAIN, "uniswap_v3", factory, None)
        .unwrap();
    let pool = Address::repeat_byte(0x11);
    db.upsert_v3_pools(
        CHAIN,
        "uniswap_v3",
        exchange.id,
        1_000_000,
        &[V3PoolRowInput {
            address: pool,
            token0_address: Address::repeat_byte(0x22),
            token1_address: Address::repeat_byte(0x33),
            fee: 500,
            tick_spacing: 10,
        }],
    )
    .unwrap();
    pool
}

/// A migrated temp DB with one V4 pool row + its `PoolManager`. Returns the
/// `(pool_hash, manager_checksum)` the verify arm should name.
fn seed_v4_pool(db_path: &Path) -> (String, String) {
    let (db, _state) = DegenbotDb::open_for_writes(db_path).unwrap();
    let factory = Address::repeat_byte(0xf1);
    let exchange = db
        .upsert_exchange(CHAIN, "uniswap_v4", factory, None)
        .unwrap();
    let manager = Address::repeat_byte(0x44);
    db.upsert_pool_manager(manager, CHAIN, "uniswap_v4", None, exchange.id)
        .unwrap();
    let pool_hash = format!("0x{}", B256::from([0xAB; 32]));
    db.upsert_v4_pools(
        CHAIN,
        &manager.to_checksum(None),
        1_000_000,
        &[V4PoolRowInput {
            pool_hash: pool_hash.clone(),
            hooks: Address::ZERO,
            currency0_address: Address::repeat_byte(0x22),
            currency1_address: Address::repeat_byte(0x33),
            fee: 0,
            tick_spacing: 60,
        }],
    )
    .unwrap();
    (pool_hash, manager.to_checksum(None))
}

fn verify_command(
    rpc_url: &str,
    pool: String,
    family: PoolFamily,
    pool_manager: Option<String>,
) -> Command {
    Command::Pool(PoolCommand::Verify {
        rpc_url: rpc_url.to_string(),
        chain_id: CHAIN,
        block_number: 42,
        pool,
        family,
        pool_manager,
    })
}

/// A GREEN verify run builds its ONE chain-verified provider and never
/// re-dials: exactly one build, exactly one `eth_chainId` bind read.
#[test]
fn pool_verify_builds_one_provider_and_runs_the_gate_green() {
    let _guard = BUILD_COUNTER.lock().unwrap();
    let _ = reset_verify_provider_build_count();
    let stub = StubNode::spawn(CHAIN as u64);
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("degenbot.db");
    let pool = seed_v3_pool(&db);
    let ctx = ctx_for(&stub.url, &db);
    let outcome = run(
        &verify_command(&stub.url, pool.to_checksum(None), PoolFamily::V3, None),
        &ctx,
        &NoPrompt,
    );
    assert_eq!(outcome.exit_code, ExitCode::Success, "{outcome:?}");
    let Some(degenbot_cli_core::CommandReport::Pool(degenbot_cli_core::PoolReport::Verified {
        divergences,
        ..
    })) = outcome.report()
    else {
        panic!("expected Verified, got {:?}", outcome.report());
    };
    assert!(divergences.is_empty());
    assert_eq!(
        verify_provider_build_count(),
        1,
        "one provider build per verify run"
    );
    assert_eq!(
        stub.chain_id_reads(),
        1,
        "the chain bind reads the endpoint chain exactly once"
    );
}

/// A provider bound to a DIFFERENT chain is refused before any on-chain
/// comparison — the chain-identity contract the hoisted construction must
/// keep (V3 family).
#[test]
fn pool_verify_refuses_an_endpoint_serving_a_different_chain_v3() {
    let _guard = BUILD_COUNTER.lock().unwrap();
    let _ = reset_verify_provider_build_count();
    let stub = StubNode::spawn(999);
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("degenbot.db");
    let pool = seed_v3_pool(&db);
    let ctx = ctx_for(&stub.url, &db);
    let outcome = run(
        &verify_command(&stub.url, pool.to_checksum(None), PoolFamily::V3, None),
        &ctx,
        &NoPrompt,
    );
    assert_eq!(outcome.exit_code, ExitCode::Failure);
    let message = outcome.error().unwrap().message();
    assert!(
        message.contains("reports chain id 999, not the expected 8453"),
        "chain-binding refusal missing: {message}"
    );
    assert_eq!(
        verify_provider_build_count(),
        1,
        "the refusal still rides the arm's single provider build"
    );
    assert_eq!(stub.chain_id_reads(), 1);
}

/// Same refusal for the V4 family: the hoisted construction serves BOTH
/// families, and neither compares on-chain state against a foreign-chain
/// endpoint.
#[test]
fn pool_verify_refuses_an_endpoint_serving_a_different_chain_v4() {
    let _guard = BUILD_COUNTER.lock().unwrap();
    let _ = reset_verify_provider_build_count();
    let stub = StubNode::spawn(999);
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("degenbot.db");
    let (pool_hash, manager) = seed_v4_pool(&db);
    let ctx = ctx_for(&stub.url, &db);
    let outcome = run(
        &verify_command(&stub.url, pool_hash, PoolFamily::V4, Some(manager)),
        &ctx,
        &NoPrompt,
    );
    assert_eq!(outcome.exit_code, ExitCode::Failure);
    let message = outcome.error().unwrap().message();
    assert!(
        message.contains("reports chain id 999, not the expected 8453"),
        "chain-binding refusal missing: {message}"
    );
    assert_eq!(verify_provider_build_count(), 1);
}

/// The lazy build: an arm that fails BEFORE the node is needed (the pool is
/// unknown) builds no provider and dials nothing.
#[test]
fn pool_verify_without_a_verified_pool_never_builds_a_provider() {
    let _guard = BUILD_COUNTER.lock().unwrap();
    let _ = reset_verify_provider_build_count();
    let stub = StubNode::spawn(CHAIN as u64);
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("degenbot.db");
    degenbot_db::ops::create_new_database(&db).unwrap();
    let ctx = ctx_for(&stub.url, &db);
    let outcome = run(
        &verify_command(
            &stub.url,
            Address::repeat_byte(0x77).to_checksum(None),
            PoolFamily::V3,
            None,
        ),
        &ctx,
        &NoPrompt,
    );
    assert_eq!(outcome.exit_code, ExitCode::Failure);
    assert!(
        matches!(
            outcome.error(),
            Some(degenbot_cli_core::CliError::InvalidArgument(_))
        ),
        "expected the unknown-pool refusal, got {:?}",
        outcome.error()
    );
    assert_eq!(
        verify_provider_build_count(),
        0,
        "a run that verifies nothing never builds a provider"
    );
    assert_eq!(stub.chain_id_reads(), 0, "no dial without work");
}
