//! Integration tests for the block-tagoffset resolution path
//! (`block::resolve_to_block` / `fetch_tag_block_number`).
//!
//! The defect class pinned here: a transport-backed provider (an alloy
//! `AlloyProvider` and the background connection tasks it spawns) constructed
//! inside ONE `block_on` ad-hoc runtime and then USED in ANOTHER. The tag
//! WITH an offset (`latest:-64` — also the `--to-block` default) reaches the
//! RPC read; a provider built on runtime A, driven on runtime B, fails its
//! first request with alloy's `TransportErrorKind::BackendGone`
//! ("backend connection task has stopped").
//!
//! - `latest:-64`-shaped specs MUST resolve against a live local JSON-RPC stub.
//! - A bare tag (`latest`) resolves to `None` without touching the network at
//!   all (the core fetches the chain tip itself).

#![expect(clippy::unwrap_used)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use degenbot_cli_core::block::{parse_to_block, resolve_to_block, ToBlockSpec};

/// The tag block the stub serves (`0x1337`).
const TAG_BLOCK: u64 = 0x1337;
/// A negative offset the tests add like `--to-block latest:-64` does.
const OFFSET: i64 = -64;

/// A minimal one-request-per-connection JSON-RPC stub answering
/// `eth_getBlockByNumber` with a single block, and refusing everything else.
/// Counts accepted HTTP connections so the bare-tag test can assert the stub
/// was never dialed.
struct StubNode {
    url: String,
    connections: Arc<AtomicUsize>,
    shutdown: Arc<AtomicBool>,
}

impl StubNode {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let connections = Arc::new(AtomicUsize::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let accepted = Arc::clone(&connections);
        let shutdown_flag = Arc::clone(&shutdown);
        listener.set_nonblocking(true).unwrap();
        thread::spawn(move || {
            while !shutdown_flag.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _addr)) => {
                        accepted.fetch_add(1, Ordering::AcqRel);
                        let _ = answer(&mut stream);
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
            connections,
            shutdown,
        }
    }

    fn connections(&self) -> usize {
        self.connections.load(Ordering::Acquire)
    }
}

impl Drop for StubNode {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
    }
}

fn answer(stream: &mut TcpStream) -> std::io::Result<()> {
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
    if method != "eth_getBlockByNumber" {
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32601, "message": "method not found"},
        });
        return write_response(stream, &response);
    }
    // A full serpable block body: alloy's RPC header deserialization requires
    // these fields present (verified by the first red run's deser refusal).
    let block = serde_json::json!({
        "number": format!("0x{TAG_BLOCK:x}"),
        "hash": format!("0x{}", "ab".repeat(32)),
        "parentHash": format!("0x{}", "cd".repeat(32)),
        "sha3Uncles": format!("0x{}", "11".repeat(32)),
        "miner": format!("0x{}", "ee".repeat(20)),
        "stateRoot": format!("0x{}", "22".repeat(32)),
        "transactionsRoot": format!("0x{}", "33".repeat(32)),
        "receiptsRoot": format!("0x{}", "44".repeat(32)),
        "logsBloom": format!("0x{}", "00".repeat(256)),
        "difficulty": "0x0",
        "gasLimit": "0x1c9c380",
        "gasUsed": "0x0",
        "timestamp": "0x64",
        "extraData": "0x",
        "mixHash": format!("0x{}", "55".repeat(32)),
        "nonce": "0x0000000000000000",
        "totalDifficulty": "0x1",
        "size": "0x20a",
    });
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": block,
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

/// RED for the cross-runtime defect: a tag WITH an offset (`latest:-64`, the
/// `--to-block` default) builds a provider, then issues `eth_getBlockByNumber`
/// against it. On the broken code the provider's transport backend task dies
/// with the runtime that constructed it, and the request fails
/// "backend connection task has stopped" instead of resolving.
#[test]
fn tag_with_offset_resolves_against_live_stub() {
    let node = StubNode::spawn();
    let spec = parse_to_block("latest:-64").unwrap();
    assert!(
        matches!(spec, ToBlockSpec::TagOffset { .. }),
        "sanity: the default `--to-block` value must take the TagOffset path, got {spec:?}"
    );
    let resolved = resolve_to_block(spec, &node.url).unwrap();
    assert_eq!(
        resolved,
        Some(u64::try_from(i128::from(TAG_BLOCK) + i128::from(OFFSET)).unwrap())
    );
}

/// A bare tag (`latest`) stays pure: `resolve_to_block` must return `None`
/// ("advance to the chain tip") without dialing the stub at all.
#[test]
fn bare_tag_resolves_without_dialing_the_node() {
    let node = StubNode::spawn();
    let resolved = resolve_to_block(parse_to_block("latest").unwrap(), &node.url).unwrap();
    assert_eq!(resolved, None);
    assert_eq!(node.connections(), 0, "a bare tag must not reach the node");
}

/// A negative offset that drives the tag block below zero is refused with the
/// same `BlockResolution` mapping (range check, not an RPC failure).
#[test]
fn out_of_range_offset_is_refused() {
    let node = StubNode::spawn();
    let parsed = parse_to_block("latest:-999999999").unwrap();
    let err = resolve_to_block(parsed, &node.url).unwrap_err();
    let rendered = err.to_string();
    assert!(
        rendered.contains("out of range"),
        "expected the range refusal, got: {rendered}"
    );
}
