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

use std::sync::Arc;

use degenbot_cli_core::block::{parse_to_block, resolve_to_block};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;

/// The tag block the stub serves (`0x1337`).
const TAG_BLOCK: u64 = 0x1337;
/// The negative offset mirroring the `--to-block` default.
const OFFSET: i64 = -64;

/// A minimal WS JSON-RPC stub answering `eth_getBlockByNumber` with a block
/// whose number is [`TAG_BLOCK`], and everything else with a method error.
struct StubWsNode {
    url: String,
    shutdown: Arc<Notify>,
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
        let shutdown = Arc::new(Notify::new());
        let notify = Arc::clone(&shutdown);
        let thread = Some(
            std::thread::Builder::new()
                .name("ws-stub".into())
                .spawn(move || runtime.block_on(serve(listener, notify)))
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
        self.shutdown.notify_waiters();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn serve(listener: TcpListener, shutdown: Arc<Notify>) {
    loop {
        let socket = tokio::select! {
            () = shutdown.notified() => break,
            streamed = listener.accept() => match streamed {
                Ok((socket, _addr)) => socket,
                Err(_) => break,
            },
        };
        let Ok(mut ws) = tokio_tungstenite::accept_async(socket).await else {
            break;
        };
        while let Some(Ok(frame)) = ws.next().await {
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
