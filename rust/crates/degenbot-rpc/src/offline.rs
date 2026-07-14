//! Offline provider: an in-memory alloy `Transport` over recorded chain data
//! (O1).
//!
//! The pure-Python `OfflineProvider` served pre-recorded RPC responses from a
//! JSON file so tests run without a live node. This module is the Rust
//! successor: an [`OfflineProvider`] holds the parsed recorded data, and its
//! [`OfflineTransport`] implements `tower::Service<RequestPacket>` (the alloy
//! `Transport` trait) so the recorded data is reachable as a *real*
//! [`AlloyProvider`] — wrapped via [`AlloyProvider::from_provider`].
//! `PyBotIo` (and any standalone Rust consumer) can then hold an offline
//! provider indistinguishably from a live one.
//!
//! ## Supported RPCs
//!
//! Mirrors the Python interface: `eth_call`, `eth_getCode`, `eth_blockNumber`,
//! `eth_getBlockByNumber`. The Python impl raised `NotImplementedError` for
//! balance / storage / nonce / logs — those surface here as
//! [`TransportErrorKind`] "method not found" (non-revert RPC error), preserving
//! the "unsupported" contract without panicking.
//!
//! ## Revert / unrecorded semantics
//!
//! - A recorded `null` result (the contract reverted / was not deployed at the
//!   block) is returned as a JSON-RPC error with code `-32000` and message
//!   `"execution reverted"`, which [`AlloyProvider::eth_call`] classifies as
//!   [`ProviderError::ExecutionReverted`] (mirroring a live node revert).
//! - An unrecorded `(to, data)` pair (fixture gap) is returned as a JSON-RPC
//!   error whose message does **not** contain "revert", so it surfaces as a
//!   `ProviderError::RpcError` — distinguishable from an on-chain revert.
//! - A `get_block` for an unrecorded block returns `null` (→ `None`); an
//!   `eth_call` at an unrecorded block is an error.

use alloy::consensus::Header as ConsensusHeader;
use alloy::primitives::B256;
use alloy::providers::{Provider, RootProvider};
use alloy::rpc::client::RpcClient;
use alloy::rpc::json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::rpc::types::eth::Header as RpcHeader;
use alloy::transports::{TransportError, TransportFut};
use degenbot_core::address_utils;
use serde::Deserialize;
use serde_json::value::{to_raw_value, RawValue};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::provider::{AlloyProvider, EthBlock};

/// Recorded chain data for a single block (the per-block JSON object shape the
/// Python `OfflineProvider` records and consumes).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RecordedBlock {
    /// The block timestamp (Unix seconds), recorded as a plain integer.
    #[serde(default)]
    pub timestamp: u64,
    /// `"to_lower:0x<data_hex>"` → result hex (**without** `0x` prefix) or
    /// `None` for a recorded revert.
    #[serde(default)]
    pub calls: HashMap<String, Option<String>>,
    /// `to_lower` → bytecode hex (**without** `0x` prefix).
    #[serde(default)]
    pub code: HashMap<String, String>,
}

/// Loose wire format for recorded chain data — accepts both the single-block
/// form (`{ chain_id, block_number, timestamp, calls, code }`) and the
/// multi-block form (`{ chain_id, blocks: { "n": {…} } }`).
#[derive(Debug, Deserialize)]
struct RecordedData {
    chain_id: u64,
    #[serde(default)]
    block_number: Option<u64>,
    #[serde(default)]
    timestamp: Option<u64>,
    #[serde(default)]
    calls: Option<HashMap<String, Option<String>>>,
    #[serde(default)]
    code: Option<HashMap<String, String>>,
    #[serde(default)]
    blocks: Option<HashMap<String, RecordedBlock>>,
}

/// An in-memory provider over recorded chain data.
///
/// Construct from a recorded-JSON file/string, then convert to an
/// [`AlloyProvider`] via [`OfflineProvider::as_alloy_provider`].
#[derive(Debug, Clone)]
pub struct OfflineProvider {
    chain_id: u64,
    /// Block number → recorded block, sorted ascending.
    blocks: BTreeMap<u64, RecordedBlock>,
}

impl OfflineProvider {
    /// Parse recorded chain data from a JSON byte slice (single- or
    /// multi-block format).
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] if the bytes are not valid recorded data.
    pub fn from_json_bytes(bytes: &[u8]) -> serde_json::Result<Self> {
        let data: RecordedData = serde_json::from_slice(bytes)?;
        Ok(Self::from_recorded_data(data))
    }

    /// Parse recorded chain data from a JSON string.
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] if the string is not valid recorded data.
    pub fn from_json_str(s: &str) -> serde_json::Result<Self> {
        let data: RecordedData = serde_json::from_str(s)?;
        Ok(Self::from_recorded_data(data))
    }

    fn from_recorded_data(data: RecordedData) -> Self {
        let mut blocks: BTreeMap<u64, RecordedBlock> = BTreeMap::new();
        if let Some(b) = data.blocks {
            for (k, v) in b {
                if let Ok(n) = k.parse::<u64>() {
                    blocks.insert(n, v);
                }
            }
        } else if let Some(n) = data.block_number {
            // Single-block format: inline fields.
            blocks.insert(
                n,
                RecordedBlock {
                    timestamp: data.timestamp.unwrap_or(0),
                    calls: data.calls.unwrap_or_default(),
                    code: data.code.unwrap_or_default(),
                },
            );
        }
        Self {
            chain_id: data.chain_id,
            blocks,
        }
    }

    /// The chain ID this provider serves.
    #[must_use]
    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// The latest recorded block number (the "latest" block).
    ///
    /// # Panics
    ///
    /// Panics if no blocks are recorded (mirrors the Python `ValueError` on
    /// empty data — construction with zero blocks is a usage error).
    #[must_use]
    pub fn latest_block(&self) -> u64 {
        *self
            .blocks
            .last_key_value()
            .expect("OfflineProvider must have at least one recorded block")
            .0
    }

    /// All recorded block numbers, ascending.
    #[must_use]
    pub fn block_numbers(&self) -> Vec<u64> {
        self.blocks.keys().copied().collect()
    }

    /// Resolve a block tag (`"latest"`/`"pending"`/absent or a hex/decimal
    /// block id) to a recorded block number. `None` if the requested block is
    /// not recorded.
    fn resolve_block(&self, tag: Option<&serde_json::Value>) -> Option<u64> {
        match tag {
            None | Some(serde_json::Value::Null) => Some(self.latest_block()),
            Some(serde_json::Value::String(s)) => {
                if s == "latest"
                    || s == "pending"
                    || s == "finalized"
                    || s == "safe"
                    || s == "earliest"
                {
                    Some(self.latest_block())
                } else if let Some(hex) = s.strip_prefix("0x") {
                    u64::from_str_radix(hex, 16)
                        .ok()
                        .filter(|n| self.blocks.contains_key(n))
                } else {
                    s.parse::<u64>()
                        .ok()
                        .filter(|n| self.blocks.contains_key(n))
                }
            }
            Some(serde_json::Value::Number(n)) => {
                n.as_u64().filter(|n| self.blocks.contains_key(n))
            }
            _ => None,
        }
    }

    /// Wrap this offline provider as a live [`AlloyProvider`] (no network).
    #[must_use]
    pub fn as_alloy_provider(self) -> AlloyProvider {
        let transport = OfflineTransport {
            data: Arc::new(self),
        };
        let client = RpcClient::new(transport, true);
        let root: RootProvider = RootProvider::new(client);
        let arc: Arc<dyn Provider<_>> = Arc::new(root);
        AlloyProvider::from_provider(arc)
    }
}

/// The alloy `Transport` backing an [`OfflineProvider`], answering JSON-RPC
/// requests from the recorded data.
#[derive(Clone, Debug)]
struct OfflineTransport {
    data: Arc<OfflineProvider>,
}

impl OfflineTransport {
    fn handle(self, packet: RequestPacket) -> ResponsePacket {
        match packet {
            RequestPacket::Single(req) => ResponsePacket::Single(self.respond(&req)),
            RequestPacket::Batch(reqs) => {
                let mut out = Vec::with_capacity(reqs.len());
                for req in reqs {
                    out.push(self.clone().respond(&req));
                }
                ResponsePacket::Batch(out)
            }
        }
    }

    fn respond(self, req: &SerializedRequest) -> Response {
        let id = req.id().clone();
        let params = req
            .params()
            .and_then(|rv| serde_json::from_str::<serde_json::Value>(rv.get()).ok())
            .unwrap_or(serde_json::Value::Null);
        let payload = match req.method() {
            "eth_blockNumber" => self.block_number(),
            "eth_getCode" => self.get_code(&params),
            "eth_call" => self.eth_call(&params),
            "eth_getBlockByNumber" => self.get_block_by_number(&params),
            _ => Err(method_not_found(req.method())),
        };
        Response {
            id,
            payload: payload.unwrap_or_else(ResponsePayload::Failure),
        }
    }

    fn block_number(&self) -> Result<ResponsePayload, ErrorPayload> {
        let n = self.data.latest_block();
        success(&format!("0x{n:x}"))
    }

    fn get_code(&self, params: &serde_json::Value) -> Result<ResponsePayload, ErrorPayload> {
        let arr = params
            .as_array()
            .ok_or_else(|| invalid_params("eth_getCode params"))?;
        let addr = arr
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| invalid_params("eth_getCode address"))?;
        let block = self
            .data
            .resolve_block(arr.get(1))
            .ok_or_else(|| block_not_recorded_msg(arr.get(1)));
        let block = block?;
        let key = match address_utils::parse_address(addr) {
            Ok(a) => a.to_checksum(None).to_lowercase(),
            Err(_) => addr.to_lowercase(),
        };
        let Some(block_data) = self.data.blocks.get(&block) else {
            return Err(block_not_recorded(block));
        };
        // GetCode returns an empty `0x` when the address has no code.
        let code = block_data.code.get(&key).cloned().unwrap_or_default();
        success(&format!("0x{code}"))
    }

    fn eth_call(&self, params: &serde_json::Value) -> Result<ResponsePayload, ErrorPayload> {
        let arr = params
            .as_array()
            .ok_or_else(|| invalid_params("eth_call params"))?;
        let tx = arr
            .first()
            .ok_or_else(|| invalid_params("eth_call tx object"))?;
        let to = tx
            .get("to")
            .or_else(|| tx.get("to"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| invalid_params("eth_call `to`"))?;
        let data = tx
            .get("input")
            .or_else(|| tx.get("data"))
            .and_then(|v| v.as_str())
            .unwrap_or("0x");
        let block = self
            .data
            .resolve_block(arr.get(1))
            .ok_or_else(|| block_not_recorded_block_id(arr.get(1)))?;
        let block_data = self
            .data
            .blocks
            .get(&block)
            .expect("resolve_block only returns recorded block numbers");
        let key = call_key(to, data);
        match block_data.calls.get(&key) {
            Some(Some(result)) => success(&format!("0x{result}")),
            Some(None) => Err(revert(to, data)),
            None => Err(unrecorded_call(to, data)),
        }
    }

    fn get_block_by_number(
        &self,
        params: &serde_json::Value,
    ) -> Result<ResponsePayload, ErrorPayload> {
        let arr = params
            .as_array()
            .ok_or_else(|| invalid_params("eth_getBlockByNumber params"))?;
        let Some(block) = self.data.resolve_block(arr.first()) else {
            return Ok(success_null());
        };
        let block_data = self
            .data
            .blocks
            .get(&block)
            .expect("resolve_block only returns recorded block numbers");
        success_block(block, block_data.timestamp)
    }
}

impl tower::Service<RequestPacket> for OfflineTransport {
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
        let this = self.clone();
        Box::pin(std::future::ready(Ok(this.handle(req))))
    }
}

// ---------------------------------------------------------------------------
// Response-payload builders
// ---------------------------------------------------------------------------

/// Build a `Success` payload from a serializable value.
fn success<T: serde::Serialize>(value: &T) -> Result<ResponsePayload, ErrorPayload> {
    let raw = to_raw_value(value).map_err(|e| internal_err(format!("offline encode: {e}")))?;
    Ok(ResponsePayload::Success(raw))
}

/// Build a `Success(null)` payload (for an unrecorded `get_block`).
fn success_null() -> ResponsePayload {
    ResponsePayload::Success(to_raw_value(&serde_json::Value::Null).expect("null encodes"))
}

/// Build the JSON-RPC block object for an `eth_getBlockByNumber` response.
fn success_block(number: u64, timestamp: u64) -> Result<ResponsePayload, ErrorPayload> {
    let consensus = ConsensusHeader {
        number,
        timestamp,
        parent_hash: B256::ZERO,
        ommers_hash: B256::ZERO,
        state_root: B256::ZERO,
        transactions_root: B256::ZERO,
        receipts_root: B256::ZERO,
        ..Default::default()
    };
    let header = RpcHeader {
        hash: B256::ZERO,
        inner: consensus,
        total_difficulty: None,
        size: None,
    };
    let block: EthBlock = EthBlock {
        header,
        ..Default::default()
    };
    success(&block)
}

/// Build the call-map key `"to_lower:0x<data_hex>"` matching the recorded
/// fixture format (lowercase address and data hex, data prefixed with `0x`).
fn call_key(to: &str, data: &str) -> String {
    let to_lower = match address_utils::parse_address(to) {
        Ok(a) => a.to_checksum(None).to_lowercase(),
        Err(_) => to.to_ascii_lowercase(),
    };
    let data_hex = data.strip_prefix("0x").unwrap_or(data).to_ascii_lowercase();
    format!("{to_lower}:0x{data_hex}")
}

// ---------------------------------------------------------------------------
// JSON-RPC error builders
// ---------------------------------------------------------------------------

fn revert(to: &str, data: &str) -> ErrorPayload {
    ErrorPayload {
        code: -32000,
        message: Cow::Borrowed("execution reverted"),
        data: revert_data(to, data),
    }
}

fn unrecorded_call(to: &str, data: &str) -> ErrorPayload {
    // Message deliberately omits "revert" so it is NOT misclassified as an
    // on-chain revert by AlloyProvider's revert marker check.
    internal_err(format!("offline: unrecorded call to {to} with data {data}"))
}

fn block_not_recorded(block: u64) -> ErrorPayload {
    internal_err(format!("offline: block {block} not recorded"))
}

fn block_not_recorded_block_id(tag: Option<&serde_json::Value>) -> ErrorPayload {
    let s = match tag {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => "latest".to_string(),
    };
    internal_err(format!("offline: block {s} not recorded"))
}

fn block_not_recorded_msg(tag: Option<&serde_json::Value>) -> ErrorPayload {
    block_not_recorded_block_id(tag)
}

fn method_not_found(method: &str) -> ErrorPayload {
    ErrorPayload {
        code: -32601,
        message: Cow::Owned(format!("offline: method not found: {method}")),
        data: None,
    }
}

fn invalid_params(msg: &str) -> ErrorPayload {
    ErrorPayload {
        code: -32602,
        message: Cow::Owned(format!("offline: invalid params: {msg}")),
        data: None,
    }
}

fn internal_err(msg: String) -> ErrorPayload {
    ErrorPayload {
        code: -32603,
        message: Cow::Owned(msg),
        data: None,
    }
}

/// Build the optional revert `data` field (a `0x`-prefixed hex string).
fn revert_data(_to: &str, _data: &str) -> Option<Box<RawValue>> {
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use alloy::primitives::{address, U256};

    /// A recorded single-block fixture in the same shape the Python
    /// `OfflineProvider` records/consumes.
    fn sample_json() -> String {
        let mut calls = std::collections::HashMap::new();
        // name() on WBTC → returns a uint256-like word.
        calls.insert(
            "0x2260fac5e5542a773aa44fbcfedf7c193bc2c599:0x06fdde03".to_string(),
            Some("000000000000000000000000000000000000000000000000000000000000002a".to_string()),
        );
        // A reverted call (null result).
        calls.insert(
            "0x2260fac5e5542a773aa44fbcfedf7c193bc2c599:0xffffffff".to_string(),
            None,
        );
        let mut code = std::collections::HashMap::new();
        code.insert(
            "0x2260fac5e5542a773aa44fbcfedf7c193bc2c599".to_string(),
            "60806040".to_string(),
        );
        let block = serde_json::json!({
            "timestamp": 1_776_986_723,
            "calls": calls,
            "code": code,
        });
        serde_json::json!({
            "chain_id": 1,
            "block_number": 24_945_920,
            "timestamp": 1_776_986_723,
            "calls": block["calls"],
            "code": block["code"],
        })
        .to_string()
    }

    #[test]
    fn parses_single_block_format() {
        let p = OfflineProvider::from_json_str(&sample_json()).unwrap();
        assert_eq!(p.chain_id(), 1);
        assert_eq!(p.block_numbers(), vec![24_945_920]);
        assert_eq!(p.latest_block(), 24_945_920);
    }

    #[test]
    fn parses_multi_block_format() {
        let inner = serde_json::json!({
            "timestamp": 100,
            "calls": {},
            "code": {},
        });
        let json = serde_json::json!({
            "chain_id": 137,
            "blocks": { "100": inner, "200": inner },
        })
        .to_string();
        let p = OfflineProvider::from_json_str(&json).unwrap();
        assert_eq!(p.chain_id(), 137);
        assert_eq!(p.block_numbers(), vec![100, 200]);
        assert_eq!(p.latest_block(), 200);
    }

    #[tokio::test]
    async fn eth_block_number_returns_latest() {
        let provider = OfflineProvider::from_json_str(&sample_json())
            .unwrap()
            .as_alloy_provider();
        let n = provider.get_block_number().await.unwrap();
        assert_eq!(n, 24_945_920);
    }

    #[tokio::test]
    async fn eth_get_code_returns_recorded_bytecode() {
        let provider = OfflineProvider::from_json_str(&sample_json())
            .unwrap()
            .as_alloy_provider();
        let addr = address!("2260fac5e5542a773aa44fbcfedf7c193bc2c599");
        let code = provider.get_code(&addr, None).await.unwrap();
        assert!(code.to_string().ends_with("60806040"));
    }

    #[tokio::test]
    async fn eth_call_returns_recorded_result() {
        let provider = OfflineProvider::from_json_str(&sample_json())
            .unwrap()
            .as_alloy_provider();
        // balanceOf-shaped call: data 0x06fdde03 happens to be name() selector
        // here, but the offline provider only matches by bytes — it returns the
        // recorded word.
        let addr = address!("2260fac5e5542a773aa44fbcfedf7c193bc2c599");
        let result = provider
            .eth_call(
                &addr,
                alloy::primitives::Bytes::from(vec![0x06, 0xfd, 0xde, 0x03]),
                None,
            )
            .await
            .unwrap();
        assert_eq!(U256::from_be_slice(&result), U256::from(0x2au64));
    }

    #[tokio::test]
    async fn eth_call_reverted_surfaces_as_execution_reverted() {
        let provider = OfflineProvider::from_json_str(&sample_json())
            .unwrap()
            .as_alloy_provider();
        let addr = address!("2260fac5e5542a773aa44fbcfedf7c193bc2c599");
        let err = provider
            .eth_call(
                &addr,
                alloy::primitives::Bytes::from(vec![0xff, 0xff, 0xff, 0xff]),
                None,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                degenbot_core::errors::ProviderError::ExecutionReverted { .. }
            ),
            "expected ExecutionReverted, got {err:?}"
        );
    }

    #[tokio::test]
    async fn eth_call_unrecorded_surfaces_as_rpc_error_not_revert() {
        let provider = OfflineProvider::from_json_str(&sample_json())
            .unwrap()
            .as_alloy_provider();
        let addr = address!("2260fac5e5542a773aa44fbcfedf7c193bc2c599");
        let err = provider
            .eth_call(
                &addr,
                alloy::primitives::Bytes::from(vec![0x12, 0x34]),
                None,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, degenbot_core::errors::ProviderError::RpcError { .. }),
            "expected RpcError (not revert), got {err:?}"
        );
    }

    #[tokio::test]
    async fn eth_get_block_by_number_returns_timestamp() {
        let provider = OfflineProvider::from_json_str(&sample_json())
            .unwrap()
            .as_alloy_provider();
        let block = provider
            .get_block(24_945_920)
            .await
            .unwrap()
            .expect("recorded block should return Some");
        assert_eq!(block.header.timestamp, 1_776_986_723u64);
    }

    #[tokio::test]
    async fn eth_get_block_by_number_unrecorded_is_none() {
        let provider = OfflineProvider::from_json_str(&sample_json())
            .unwrap()
            .as_alloy_provider();
        let block = provider.get_block(9_999).await.unwrap();
        assert!(block.is_none(), "unrecorded block should be None");
    }

    #[tokio::test]
    async fn unsupported_method_is_rpc_error() {
        // get_balance isn't recorded by the offline provider; the Python impl
        // raised NotImplementedError. The Rust transport returns "method not
        // found" → non-revert RPC error.
        let provider = OfflineProvider::from_json_str(&sample_json())
            .unwrap()
            .as_alloy_provider();
        let err = provider
            .get_balance(&address!("2260fac5e5542a773aa44fbcfedf7c193bc2c599"), None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, degenbot_core::errors::ProviderError::RpcError { .. }),
            "expected RpcError for unsupported method, got {err:?}"
        );
    }
}
