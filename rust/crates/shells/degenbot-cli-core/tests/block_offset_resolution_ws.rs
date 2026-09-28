//! WebSocket-transport regression for the block-tag resolution path
//! (`block::resolve_to_block`).
//!
//! The HTTP-only stub in `block_offset_resolution.rs` cannot catch the defect
//! class fixed here: a WS transport spawns a PERSISTENT connection task bound
//! to the runtime that constructed its provider, so the
//! build-runtime/drive-runtime split the HTTP path tolerates fails over WS
//! with alloy's `TransportErrorKind::BackendGone`
//! ("backend connection task has stopped"). The contract pinned here: one
//! `resolve_to_block(TagOffset)` call over `ws://` succeeds — which requires
//! the provider build and the read to share one runtime.
//!
//! (lint allow) The stub harness and the assertions construct results with
//! `expect`: a local fixture failing to bind, spawn, or parse is an instant,
//! loud test failure.
#![expect(clippy::expect_used, clippy::unwrap_used)]

use degenbot_cli_core::block::{parse_to_block, resolve_to_block};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message;

/// The tag block the stub serves (`0x1337`).
const TAG_BLOCK: u64 = 0x1337;
/// The negative offset mirroring the `--to-block` default.
const OFFSET: i64 = -64;

/// A minimal WS JSON-RPC stub answering `eth_getBlockByNumber` with a block
/// whose number is [`TAG_BLOCK`], and everything else with a method error.
struct StubWsNode {
    url: String,
    shutdown: watch::Sender<bool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl StubWsNode {
    fn spawn() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("test runtime");
        let listener =
            runtime.block_on(async { TcpListener::bind("127.0.0.1:0").await.expect("bind") });
        let url = format!("ws://{}", listener.local_addr().unwrap());
        // The shutdown must be STATEFUL: `Drop` can land while `serve` is
        // inside a per-connection loop, where an edge-triggered notify has no
        // registered waiter and is lost — leaving `serve` parked on
        // `notified()` + `accept()` forever and `Drop`'s `thread.join()` hung.
        // A `watch` value survives until observed, so `serve` honors the
        // shutdown at its next await point wherever that is.
        let (shutdown, shutdown_rx) = watch::channel(false);
        let thread = Some(
            std::thread::Builder::new()
                .name("ws-stub".into())
                .spawn(move || runtime.block_on(serve(listener, shutdown_rx)))
                .expect("spawn"),
        );
        Self {
            url,
            shutdown,
            thread,
        }
    }
}

impl Drop for StubWsNode {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn serve(listener: TcpListener, mut shutdown: watch::Receiver<bool>) {
    'outer: loop {
        let socket = tokio::select! {
            _ = shutdown.changed() => break,
            streamed = listener.accept() => match streamed {
                Ok((socket, _addr)) => socket,
                Err(_) => break,
            },
        };
        let Ok(mut ws) = tokio_tungstenite::accept_async(socket).await else {
            break;
        };
        loop {
            // The shutdown select spans the connection loop too, not only the
            // accept path: the drop-under-load hang fired precisely while a
            // (possibly dead) connection was being drained here.
            let frame = tokio::select! {
                _ = shutdown.changed() => break 'outer,
                next = ws.next() => match next {
                    Some(Ok(frame)) => frame,
                    _ => break,
                },
            };
            let Message::Text(text) = frame else { continue };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            let id = value.get("id").cloned().unwrap_or(serde_json::json!(1));
            let reply =
                if value.get("method").and_then(|m| m.as_str()) == Some("eth_getBlockByNumber") {
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": {
                            "hash": format!("0x{:064x}", 1u128),
                            "parentHash": format!("0x{:064x}", 0),
                            "sha3Uncles": format!("0x{:064x}", 0),
                            "miner": format!("0x{:040x}", 0),
                            "stateRoot": format!("0x{:064x}", 0),
                            "transactionsRoot": format!("0x{:064x}", 0),
                            "receiptsRoot": format!("0x{:064x}", 0),
                            "logsBloom": format!("0x{}", "0".repeat(512)),
                            "difficulty": "0x0",
                            "number": format!("0x{TAG_BLOCK:x}"),
                            "gasLimit": "0x0",
                            "gasUsed": "0x0",
                            "timestamp": "0x0",
                            "extraData": "0x",
                            "mixHash": format!("0x{:064x}", 0),
                            "nonce": "0x0000000000000000",
                            "baseFeePerGas": "0x0",
                            "transactions": [],
                            "size": "0x0",
                        }
                    })
                } else {
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": {"code": -32601, "message": "method not found"}
                    })
                };
            if ws
                .send(Message::Text(reply.to_string().into()))
                .await
                .is_err()
            {
                break;
            }
        }
    }
}

/// Dropping the stub must shut `serve` down even while a client connection is
/// still open. (Red over the lost-shutdown defect: `Notify::notify_waiters`
/// only wakes waiters registered at that instant, so a drop that lands while
/// `serve` is inside a per-connection loop loses the signal and `Drop`'s
/// `thread.join()` blocks the test forever — the flaky 20s nextest
/// termination on `tag_with_offset_resolves_over_websocket`.)
#[test]
fn drop_shuts_down_serve_even_with_a_live_connection() {
    let node = StubWsNode::spawn();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("client runtime");
    let url = node.url.clone();
    // Complete a WS handshake and keep the connection OPEN: the stub is then
    // parked in its per-connection loop, exactly the state in which an
    // edge-triggered shutdown notify is lost.
    let _client = runtime.block_on(async move {
        let (ws, _resp) = tokio_tungstenite::connect_async(url).await.expect("dial");
        // Let the stub settle into its connection loop past the handshake.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        ws
    });
    // Drop the stub while the socket is still open; shutdown must reach
    // `serve` and let `Drop`'s join return.
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(node);
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("stub drop must complete while a connection is open");
}

/// The `latest:-64` default resolves over a WebSocket endpoint: the provider
/// build and the `eth_getBlockByNumber` read share one runtime, so the WS
/// transport's connection task is alive for the read. (Red over the defect:
/// a build-on-one-runtime/read-on-another pair fails this with backend-gone.)
#[test]
fn tag_with_offset_resolves_over_websocket() {
    let node = StubWsNode::spawn();
    let spec = parse_to_block("latest:-64").expect("parse");
    let resolved = resolve_to_block(spec, &node.url).expect("resolve");
    assert_eq!(
        resolved,
        Some(u64::try_from(i128::from(TAG_BLOCK) + i128::from(OFFSET)).expect("positive"))
    );
}
