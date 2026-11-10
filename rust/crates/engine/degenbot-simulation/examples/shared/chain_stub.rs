//! The scripted chain stub the delta and retention suites drive: one storage
//! word per `(height, account, slot)` request and one raw list per exact block
//! hash, recording every dispatched request so a test can count exactly what
//! took the dynamic path. Hand-scripted scenarios set the chain truth directly
//! (values may change from block to block); the modeled chain
//! ([`modeled_chain::StubChain`]) derives its answers instead — pick this stub
//! when the test names the values.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use alloy::primitives::{Address, B256, U256};
use alloy::providers::RootProvider;
use alloy::rpc::client::ClientBuilder;
use alloy::rpc::json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::transports::{TransportError, TransportFut};
use serde_json::value::to_raw_value;
use serde_json::Value;

use alloy_eip7928::bal::{Bal, DecodedBal};

use degenbot_rpc::provider::AlloyProvider;

/// The stub's per-`(height, account, slot)` answer table — one block's chain
/// truth.
pub type ChainValues = HashMap<(u64, Address, U256), u64>;

/// The value every unlisted storage word answers with.
pub const SERVED: u64 = 42;

/// A chain stub serving the serial storage read and the raw block access list
/// at exact hashes, recording every dispatched request.
#[derive(Debug, Clone, Default)]
pub struct ChainStub {
    requests: Arc<Mutex<Vec<String>>>,
    bal_by_hash: Arc<Mutex<HashMap<B256, Vec<u8>>>>,
    bal_unsupported: Arc<AtomicBool>,
    bal_null: Arc<AtomicBool>,
    values: Arc<Mutex<ChainValues>>,
    refused: Arc<Mutex<HashSet<(u64, Address, U256)>>>,
    hold: Arc<AtomicBool>,
    slow: Arc<AtomicBool>,
    storage_singles: Arc<AtomicUsize>,
    storage_batched: Arc<AtomicUsize>,
}

impl ChainStub {
    pub fn serving() -> Self {
        Self::default()
    }

    /// Serve one storage word of one block's post state — the chain truth a
    /// dynamic read at `height` returns.
    pub fn serve_at(&self, height: u64, address: Address, slot: U256, value: u64) {
        self.values
            .lock()
            .unwrap()
            .insert((height, address, slot), value);
    }

    /// Serve `debug_getRawBlockAccessList` with these raw bytes at ONE exact
    /// block hash.
    pub fn serve_bal_at(&self, hash: B256, bytes: Vec<u8>) {
        self.bal_by_hash.lock().unwrap().insert(hash, bytes);
    }

    /// Refuse the BAL method the way a node without it answers.
    pub fn set_bal_unsupported(&self, unsupported: bool) {
        self.bal_unsupported.store(unsupported, Ordering::Relaxed);
    }

    /// Answer the BAL request with JSON `null` — no list at that hash.
    pub fn set_bal_null(&self, null: bool) {
        self.bal_null.store(null, Ordering::Relaxed);
    }

    /// Refuse one storage word at one height the way a node that cannot serve
    /// the slot answers — the member's own failure, not the round's.
    pub fn refuse_at(&self, height: u64, address: Address, slot: U256) {
        self.refused.lock().unwrap().insert((height, address, slot));
    }

    /// Serve a previously refused storage word again — the healed node a
    /// retried identity reads from.
    pub fn allow_at(&self, height: u64, address: Address, slot: U256) {
        self.refused
            .lock()
            .unwrap()
            .remove(&(height, address, slot));
    }

    /// Hold every request without answering: the round is genuinely in flight
    /// and only a dropped caller ends it (the abandoned-round path).
    pub fn set_hold(&self, hold: bool) {
        self.hold.store(hold, Ordering::Relaxed);
    }

    /// Answer every request one cooperative yield later, so a round is
    /// observably in flight across a poll (the in-flight coalescing path).
    pub fn set_slow(&self, slow: bool) {
        self.slow.store(slow, Ordering::Relaxed);
    }

    pub fn clear(&self) {
        self.requests.lock().unwrap().clear();
        self.storage_singles.store(0, Ordering::Relaxed);
        self.storage_batched.store(0, Ordering::Relaxed);
    }

    /// Storage reads dispatched as their own single request — the shape a
    /// serial fallback would produce.
    pub fn storage_singles(&self) -> usize {
        self.storage_singles.load(Ordering::Relaxed)
    }

    /// Storage reads dispatched as batch members — the shape the read hub's
    /// rounds produce.
    pub fn storage_batched(&self) -> usize {
        self.storage_batched.load(Ordering::Relaxed)
    }

    /// BAL fetches dispatched, through EITHER official representation.
    pub fn bal_requests(&self) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| {
                request.starts_with("debug_getRawBlockAccessList")
                    || request.starts_with("eth_getBlockAccessList")
            })
            .count()
    }

    /// Storage reads dispatched — one `eth_getStorageAt` per dynamic miss.
    pub fn storage_members(&self) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.starts_with("eth_getStorageAt"))
            .count()
    }

    pub fn provider(&self) -> AlloyProvider {
        let client = ClientBuilder::default().transport(self.clone(), true);
        let root: RootProvider = RootProvider::new(client);
        AlloyProvider::from_provider(Arc::new(root))
    }

    /// Record one dispatched request's identity.
    fn record(&self, one: &SerializedRequest) {
        self.requests.lock().unwrap().push(format!(
            "{} {}",
            one.method(),
            one.params()
                .map_or_else(String::new, |p| p.get().to_string())
        ));
    }

    /// One dispatched request's identity in wire form.
    fn answer(&self, req: &SerializedRequest) -> Response {
        let payload = if req.method() == "debug_getRawBlockAccessList" {
            self.answer_bal(req)
        } else if req.method() == "eth_getBlockAccessList" {
            self.answer_bal_json(req)
        } else if req.method() == "eth_getStorageAt" {
            self.answer_storage(req)
        } else {
            panic!("a scripted read must not issue {}", req.method())
        };
        Response {
            id: req.id().clone(),
            payload,
        }
    }

    fn answer_bal(&self, req: &SerializedRequest) -> ResponsePayload {
        if self.bal_unsupported.load(Ordering::Relaxed) {
            return ResponsePayload::Failure(ErrorPayload {
                code: -32_601,
                message: Cow::Borrowed("the method debug_getRawBlockAccessList does not exist"),
                data: None,
            });
        }
        if self.bal_null.load(Ordering::Relaxed) {
            return ResponsePayload::Success(to_raw_value(&Value::Null).unwrap());
        }
        let hash: B256 = first_param(req)
            .parse()
            .expect("the block hash is the first param");
        match self.bal_by_hash.lock().unwrap().get(&hash) {
            Some(raw) => ResponsePayload::Success(
                to_raw_value(&alloy::primitives::Bytes::copy_from_slice(raw)).unwrap(),
            ),
            None => ResponsePayload::Failure(ErrorPayload {
                code: -32_000,
                message: Cow::Borrowed("no block access list at that hash"),
                data: None,
            }),
        }
    }

    /// The same list through the OFFICIAL JSON method: one fixture, both
    /// representations, so a scenario reads the same either way.
    fn answer_bal_json(&self, req: &SerializedRequest) -> ResponsePayload {
        if self.bal_unsupported.load(Ordering::Relaxed) {
            return ResponsePayload::Failure(ErrorPayload {
                code: -32_601,
                message: Cow::Borrowed("the method eth_getBlockAccessList does not exist"),
                data: None,
            });
        }
        if self.bal_null.load(Ordering::Relaxed) {
            return ResponsePayload::Success(to_raw_value(&Value::Null).unwrap());
        }
        let hash: B256 = first_param(req)
            .parse()
            .expect("the block hash is the first param");
        match self.bal_by_hash.lock().unwrap().get(&hash) {
            Some(raw) => ResponsePayload::Success(to_raw_value(&bal_json(raw)).unwrap()),
            None => ResponsePayload::Failure(ErrorPayload {
                code: -32_000,
                message: Cow::Borrowed("no block access list at that hash"),
                data: None,
            }),
        }
    }

    fn answer_storage(&self, req: &SerializedRequest) -> ResponsePayload {
        let address = param(req, 0)
            .parse()
            .expect("the address is the first param");
        let slot: U256 = param(req, 1).parse().expect("the slot is the second param");
        let height = param(req, 2);
        let height =
            u64::from_str_radix(height.trim_start_matches("0x"), 16).expect("the block tag is hex");
        if self
            .refused
            .lock()
            .unwrap()
            .contains(&(height, address, slot))
        {
            return ResponsePayload::Failure(ErrorPayload {
                code: -32_000,
                message: Cow::Borrowed("the node refuses this storage slot"),
                data: None,
            });
        }
        let value = self
            .values
            .lock()
            .unwrap()
            .get(&(height, address, slot))
            .copied()
            .unwrap_or(SERVED);
        ResponsePayload::Success(to_raw_value(&Value::String(format!("{value:#x}"))).unwrap())
    }
}

/// The official `eth_getBlockAccessList` JSON equivalent of these raw list bytes
/// — the SAME fixture in the node's other representation. Bytes that are not one
/// complete list answer with a JSON list whose account carries an unparseable
/// address, so a malformed fixture stays malformed in both representations.
fn bal_json(raw: &[u8]) -> Value {
    match DecodedBal::from_rlp_bytes_as::<Bal>(alloy::primitives::Bytes::copy_from_slice(raw)) {
        Ok(decoded) => serde_json::to_value(decoded.as_bal()).expect("the list serializes"),
        Err(_) => serde_json::json!([{"address": "0xnothex"}]),
    }
}

/// The `n`th request param as raw text.
fn param(req: &SerializedRequest, n: usize) -> String {
    let params: Value = serde_json::from_str(req.params().expect("a member carries params").get())
        .expect("member params are JSON");
    params
        .get(n)
        .and_then(Value::as_str)
        .expect("the member carries the param")
        .to_string()
}

fn first_param(req: &SerializedRequest) -> String {
    param(req, 0)
}

impl tower::Service<RequestPacket> for ChainStub {
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: RequestPacket) -> Self::Future {
        let stub = self.clone();
        if stub.hold.load(Ordering::Relaxed) {
            // A request the node never answers: the caller's round stays in
            // flight until it is abandoned.
            return Box::pin(std::future::pending());
        }
        let slow = stub.slow.load(Ordering::Relaxed);
        Box::pin(async move {
            if slow {
                tokio::task::yield_now().await;
            }
            match req {
                RequestPacket::Single(one) => {
                    stub.record(&one);
                    if one.method() == "eth_getStorageAt" {
                        stub.storage_singles.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok(ResponsePacket::Single(stub.answer(&one)))
                }
                RequestPacket::Batch(many) => {
                    for one in &many {
                        if one.method() == "eth_getStorageAt" {
                            stub.storage_batched.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Ok(ResponsePacket::Batch(
                        many.iter()
                            .map(|one| {
                                stub.record(one);
                                stub.answer(one)
                            })
                            .collect(),
                    ))
                }
            }
        })
    }
}
