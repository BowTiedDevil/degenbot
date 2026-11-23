//! The scripted chain the activation and verification seams read against.
//!
//! One exact head block, one answer per `(target, selector)`, and the code each
//! address holds there. Every hash-pinned read must name that head's hash, so a
//! seam that reads at an ambient position (or at a second block) fails here
//! instead of passing on a tolerant double.

#![expect(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use alloy::primitives::{Address, Bytes, B256};
use alloy::providers::RootProvider;
use alloy::rpc::client::ClientBuilder;
use alloy::rpc::json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::transports::{TransportError, TransportFut};
use degenbot_rpc::provider::AlloyProvider;
use serde_json::{json, value::to_raw_value, Value};

/// The code every address holds unless the fixture says otherwise.
const DEFAULT_CODE: [u8; 2] = [0x60, 0x00];

/// The chain id every fixture serves.
pub(crate) const CHAIN_ID: u64 = 1;

/// The height (and identity) every fixture serves.
pub(crate) const HEAD_NUMBER: u64 = 100;
pub(crate) const HEAD_HASH: B256 = B256::new([0xaa; 32]);
pub(crate) const HEAD_PARENT: B256 = B256::new([0xbb; 32]);

/// What one scripted `eth_call` answers.
#[derive(Debug, Clone)]
pub(crate) enum Answer {
    /// The call returns these bytes.
    Return(Bytes),
    /// The call reverts at the anchor.
    Revert,
    /// The node refuses the call (a transport-class failure).
    Refuse { code: i64, message: String },
}

/// One request the seam issued, as the wire carried it.
#[derive(Debug, Clone)]
pub(crate) struct Issued {
    pub(crate) method: String,
    pub(crate) params: Value,
}

impl Issued {
    /// The calldata's 4-byte selector, lowercase, when this is an `eth_call`.
    pub(crate) fn selector(&self) -> Option<String> {
        let data = self.params.get(0)?.get("data")?.as_str()?;
        Some(data.get(..10)?.to_lowercase())
    }

    /// The block identifier this request named.
    pub(crate) fn block_id(&self) -> Option<&Value> {
        self.params.get(1)
    }

    /// The target address this request named.
    pub(crate) fn to(&self) -> Option<Address> {
        self.params.get(0)?.get("to")?.as_str()?.parse().ok()
    }
}

#[derive(Clone)]
struct ChainState {
    chain_id: u64,
    code: BTreeMap<Address, Bytes>,
    answers: BTreeMap<(Address, [u8; 4]), Answer>,
}

/// A chain that answers one exact head block and refuses everything else.
#[derive(Clone)]
pub(crate) struct ScriptedChain {
    state: Arc<Mutex<ChainState>>,
    issued: Arc<Mutex<Vec<Issued>>>,
}

impl ScriptedChain {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(ChainState {
                chain_id: CHAIN_ID,
                code: BTreeMap::new(),
                answers: BTreeMap::new(),
            })),
            issued: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Script the answer for `selector` on `to`.
    pub(crate) fn answer(self, to: Address, selector: [u8; 4], answer: Answer) -> Self {
        self.state
            .lock()
            .unwrap()
            .answers
            .insert((to, selector), answer);
        self
    }

    /// Serve this chain id from `eth_chainId` — an endpoint bound to another
    /// chain than the caller's market.
    pub(crate) fn chain(self, chain_id: u64) -> Self {
        self.state.lock().unwrap().chain_id = chain_id;
        self
    }

    /// Script `to` as an address that holds no code at the head block.
    pub(crate) fn no_code(self, to: Address) -> Self {
        self.state.lock().unwrap().code.insert(to, Bytes::new());
        self
    }

    pub(crate) fn issued(&self) -> Vec<Issued> {
        self.issued.lock().unwrap().clone()
    }

    /// The requests that named `selector` on `to`.
    pub(crate) fn calls_to(&self, to: Address, selector: [u8; 4]) -> Vec<Issued> {
        let wanted = format!("0x{}", alloy::hex::encode(selector));
        self.issued()
            .into_iter()
            .filter(|issued| issued.method == "eth_call")
            .filter(|issued| issued.to() == Some(to))
            .filter(|issued| issued.selector() == Some(wanted.clone()))
            .collect()
    }

    /// The read requests (`eth_call` / `eth_getCode`) the seam issued.
    pub(crate) fn reads(&self) -> Vec<Issued> {
        self.issued()
            .into_iter()
            .filter(|issued| issued.method == "eth_call" || issued.method == "eth_getCode")
            .collect()
    }

    /// A provider bound to this chain with the binding's own traffic forgotten,
    /// so every request the test inspects belongs to the seam under test.
    pub(crate) async fn provider(&self) -> AlloyProvider {
        let client = ClientBuilder::default().transport(self.clone(), true);
        let root: RootProvider = RootProvider::new(client);
        let provider = AlloyProvider::from_provider(Arc::new(root));
        let chain_id = self.state.lock().unwrap().chain_id;
        provider
            .bind_to_chain(chain_id)
            .await
            .expect("the scripted chain serves its own chain id");
        self.issued.lock().unwrap().clear();
        provider
    }

    fn code_of(&self, address: Address) -> Bytes {
        let state = self.state.lock().unwrap();
        match state.code.get(&address) {
            Some(code) => code.clone(),
            None => Bytes::copy_from_slice(&DEFAULT_CODE),
        }
    }

    fn answer_of(&self, to: Address, data: &str) -> Answer {
        let selector: [u8; 4] = {
            let raw = data.trim_start_matches("0x");
            let mut selector = [0u8; 4];
            for (slot, byte) in selector.iter_mut().zip(raw.as_bytes().chunks(2)) {
                *slot = u8::from_str_radix(std::str::from_utf8(byte).unwrap(), 16).unwrap();
            }
            selector
        };
        let state = self.state.lock().unwrap();
        state
            .answers
            .get(&(to, selector))
            .cloned()
            .unwrap_or_else(|| Answer::Refuse {
                code: -32_603,
                message: format!("scripted chain: no answer for {to} {data}"),
            })
    }
}

/// Whether `id` names the fixture's exact head block, as EIP-1898 states it.
fn names_head(id: Option<&Value>) -> bool {
    let Some(id) = id else { return false };
    let hash = id.get("blockHash").and_then(Value::as_str);
    let canonical = id.get("requireCanonical").and_then(Value::as_bool);
    hash == Some(format!("{HEAD_HASH}").as_str()) && canonical == Some(true)
}

fn success(value: &Value) -> ResponsePayload {
    ResponsePayload::Success(to_raw_value(value).unwrap())
}

fn refusal(code: i64, message: String) -> ResponsePayload {
    ResponsePayload::Failure(ErrorPayload {
        code,
        message: message.into(),
        data: None,
    })
}

fn reply(chain: &ScriptedChain, req: &SerializedRequest) -> ResponsePayload {
    let params: Value = req
        .params()
        .and_then(|raw| serde_json::from_str(raw.get()).ok())
        .unwrap_or(Value::Null);
    match req.method() {
        "eth_chainId" => {
            let chain_id = chain.state.lock().unwrap().chain_id;
            success(&json!(format!("0x{chain_id:x}")))
        }
        "eth_blockNumber" => success(&json!(format!("0x{HEAD_NUMBER:x}"))),
        "eth_getBlockByNumber" => {
            let asked = params
                .get(0)
                .and_then(Value::as_str)
                .and_then(|raw| u64::from_str_radix(raw.trim_start_matches("0x"), 16).ok());
            if asked == Some(HEAD_NUMBER) {
                success(&json!({
                    "number": format!("0x{HEAD_NUMBER:x}"),
                    "hash": format!("{HEAD_HASH}"),
                    "parentHash": format!("{HEAD_PARENT}"),
                }))
            } else {
                success(&Value::Null)
            }
        }
        "eth_getCode" | "eth_call" => {
            if !names_head(params.get(1)) {
                return refusal(
                    -32_000,
                    format!(
                        "scripted chain holds no canonical block {}",
                        params
                            .get(1)
                            .map_or_else(|| "<none>".to_string(), Value::to_string)
                    ),
                );
            }
            let to: Address = match params.get(0) {
                Some(Value::String(raw)) => raw.parse().ok(),
                Some(call) => call
                    .get("to")
                    .and_then(Value::as_str)
                    .and_then(|raw| raw.parse().ok()),
                None => None,
            }
            .unwrap_or(Address::ZERO);
            if req.method() == "eth_getCode" {
                return success(&json!(format!(
                    "0x{}",
                    alloy::hex::encode(chain.code_of(to))
                )));
            }
            let data = params
                .get(0)
                .and_then(|call| call.get("data"))
                .and_then(Value::as_str)
                .unwrap_or("0x");
            match chain.answer_of(to, data) {
                Answer::Return(bytes) => {
                    success(&json!(format!("0x{}", alloy::hex::encode(bytes))))
                }
                Answer::Revert => refusal(-32_000, String::from("execution reverted")),
                Answer::Refuse { code, message } => refusal(code, message),
            }
        }
        other => refusal(-32_601, format!("scripted chain serves no {other}")),
    }
}

impl tower::Service<RequestPacket> for ScriptedChain {
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: RequestPacket) -> Self::Future {
        let chain = self.clone();
        Box::pin(async move {
            let respond = |req: &SerializedRequest| -> Response {
                let params = req
                    .params()
                    .and_then(|raw| serde_json::from_str::<Value>(raw.get()).ok())
                    .unwrap_or(Value::Null);
                chain.issued.lock().unwrap().push(Issued {
                    method: req.method().to_string(),
                    params,
                });
                Response {
                    id: req.id().clone(),
                    payload: reply(&chain, req),
                }
            };
            match req {
                RequestPacket::Single(single) => Ok(ResponsePacket::Single(respond(&single))),
                RequestPacket::Batch(batch) => {
                    let answers: Vec<Response> = batch.iter().map(respond).collect();
                    Ok(ResponsePacket::Batch(answers))
                }
            }
        })
    }
}
