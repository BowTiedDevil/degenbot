#![expect(
    clippy::expect_used,
    reason = "the offline transport answers known-valid JSON for the fixture's methods"
)]

//! The offline RPC fixture the crate's integration tests share.
//!
//! ONE transport, not two: the arbitration fixtures drive the REAL
//! execute → encode → EVM2 simulate path over it, so a second copy in another
//! test binary would be a second answer to "what does the node say at this
//! block". It answers the handful of read methods the sim's cold-miss fallback
//! needs (`eth_getBalance`, `eth_getTransactionCount`, `eth_getCode`,
//! `eth_getStorageAt`, `eth_getBlockByNumber`) and refuses every other method
//! loudly, so a fixture that silently grew a dependency on a live node fails
//! instead of passing on a made-up answer.
//!
//! No node, no socket, no broadcast: the transport is a `tower::Service` over a
//! `RootProvider`, which is what lets these suites run in CI with no network at
//! all.

use std::sync::Arc;

use alloy::providers::RootProvider;
use alloy::rpc::client::RpcClient;
use alloy::rpc::json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::transports::{TransportError, TransportFut};
use degenbot_rpc::provider::AlloyProvider;
use serde_json::value::{to_raw_value, RawValue};

#[derive(Clone, Copy)]
struct EmptyTransport;

fn success(value: &str) -> ResponsePayload {
    let raw: Box<RawValue> =
        to_raw_value(&serde_json::Value::String(value.into())).expect("valid JSON string");
    ResponsePayload::Success(raw)
}

impl tower::Service<RequestPacket> for EmptyTransport {
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: RequestPacket) -> Self::Future {
        Box::pin(async move {
            let respond = |req: &SerializedRequest| -> Response {
                let payload = match req.method() {
                    "eth_getBalance" | "eth_getTransactionCount" => success("0x0"),
                    "eth_getCode" => success("0x"),
                    "eth_getStorageAt" => success(&format!("0x{:064x}", 0)),
                    "eth_getBlockByNumber" => ResponsePayload::Success(
                        to_raw_value(
                            &serde_json::json!({ "hash": format!("0x{}", "11".repeat(32)) }),
                        )
                        .expect("valid JSON value"),
                    ),
                    other => {
                        return Response {
                            id: req.id().clone(),
                            payload: ResponsePayload::Failure(ErrorPayload {
                                code: -32601,
                                message: format!("method not found: {other}").into(),
                                data: None,
                            }),
                        }
                    }
                };
                Response {
                    id: req.id().clone(),
                    payload,
                }
            };
            Ok(match req {
                RequestPacket::Single(r) => ResponsePacket::Single(respond(&r)),
                RequestPacket::Batch(rs) => ResponsePacket::Batch(rs.iter().map(respond).collect()),
            })
        })
    }
}

/// The offline provider every fixture in this crate drives.
pub fn mock_provider() -> AlloyProvider {
    let client = RpcClient::new(EmptyTransport, true);
    let root: RootProvider = RootProvider::new(client);
    AlloyProvider::from_provider(
        Arc::new(root) as Arc<dyn alloy::providers::Provider<alloy::network::Ethereum>>
    )
}
