//! The wave-2 golden-capture generator seam (ADR-068 approach C):
//! a **node-free JSON-RPC chain** backed by the contract-agnostic fixture
//! EVM ([`crate::oracle`]).
//!
//! [`ScratchDriver`] executes REAL contract frames (deploy / call) on a
//! driver-thread-owned revm EVM and publishes each frame's committed-state
//! snapshot into [`ScratchChain`] — the served chain. The chain answers the
//! updater's JSON-RPC surface — `eth_chainId`, `eth_blockNumber`,
//! `eth_getLogs`, `eth_call` — from the frames' products: logs come from
//! real `LOG` opcodes of real deployed bytecode, and `eth_call` executes the
//! requested calldata on a throwaway EVM built over the published snapshot
//! (a Multicall3 `aggregate3` envelope is decoded and every sub-call runs as
//! a real EVM call — the pool's own `ticks`/`tickBitmap` getters execute as
//! real pool bytecode). Wrapped in `degenbot-rpc`'s
//! [`RecordingTransport`](::degenbot_rpc::cassette::RecordingTransport) the
//! chain therefore manufactures cassettes through the recorder's own
//! canonical writer — the SAME artifact pipeline a live recording uses, with
//! zero network.
//!
//! # The driver/served split (why two types)
//!
//! A revm [`FixtureEvm`](crate::oracle::FixtureEvm) is deliberately `!Send`
//! (its working buffers carry `Rc`/raw-pointer internals). The driver keeps
//! it thread-local; the served state carries only the Send-able committed
//! snapshot (plain maps + `Arc`-held bytecode) plus the log ledger, so the
//! transport can be driven from any runtime thread. The published snapshot
//! is always exactly the state the frames committed — there is no second
//! writer.
//!
//! # Determinism
//!
//! Every frame, log coordinate, block hash, and served byte is a pure
//! function of the frame sequence: block hashes are
//! `keccak256(block_number)`, tx hashes `keccak256(block || tx_index)`, and
//! the served head is the driver's explicitly advanced value. No wall clock,
//! no RNG, no map-iteration leakage — the same frame sequence yields
//! byte-identical cassettes (the generator's determinism gate).
//!
//! # Strictness
//!
//! Unknown RPC methods and malformed params are answered with JSON-RPC
//! FAILURE payloads, never defaults: there is no catch-all arm that silently
//! classifies an unrecognized surface as something it is not. `eth_call`
//! block tags beyond the served head are refused (a read of state the
//! generator has not driven must not masquerade as empty state).

use std::sync::Arc;

use alloy::primitives::{keccak256, Address, Bytes, B256, U256};
use alloy::providers::{Provider, RootProvider};
use alloy::rpc::client::RpcClient;
use alloy::rpc::json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::transports::TransportFut;
use degenbot_rpc::multicall3::{AGGREGATE3_SELECTOR, MULTICALL3_ADDRESS};
use degenbot_rpc::provider::AlloyProvider;
use parking_lot::Mutex;
use revm::context_interface::result::Output;
use revm::context_interface::ContextTr as _;
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use serde_json::Value;

use crate::oracle::{self, FixtureEvm, TxSpec, Verdict};

pub mod actor;

pub use actor::{
    scripted_actor_creation_code, scripted_coordinator_creation_code, ActorBranch, ScriptedLog,
};

/// One executed frame's log, pinned to its block with a deterministic
/// `(block, tx_index, log_index)` coordinate.
struct ScratchLog {
    block: u64,
    tx_index: u64,
    log_index: u64,
    address: Address,
    topics: Vec<B256>,
    data: Bytes,
}

/// The SERVED chain state (Send by construction): the frame products plus
/// the committed-state snapshot. Never holds the driver's EVM.
struct ScratchChainInner {
    chain_id: u64,
    /// The head `eth_blockNumber` serves (the driver advances it explicitly
    /// per frame batch; frames execute into the block it names).
    head: u64,
    /// Frame-assigned block timestamp base (deterministic, no wall clock):
    /// block `b` serves `timestamp_base + b`.
    timestamp_base: u64,
    logs: Vec<ScratchLog>,
    /// Per-block tx/log cursors, reset on every head advance.
    next_tx_index: u64,
    next_log_index: u64,
    /// The committed state the frames produced (the driver publishes a
    /// clone after each frame). `CacheDB<EmptyDB>` is plain maps + `Arc`
    /// bytecode — Send.
    db_snapshot: CacheDB<EmptyDB>,
}

/// The served, cloneable view of a scratch chain. Frame driving goes through
/// [`ScratchDriver`]; this handle answers JSON-RPC.
#[derive(Clone)]
pub struct ScratchChain(Arc<Mutex<ScratchChainInner>>);

impl ScratchChain {
    /// Wrap this chain as the transport behind a real [`AlloyProvider`] —
    /// updaters run unchanged over it (ADR-068 D5 injection).
    #[must_use]
    pub fn as_alloy_provider(&self) -> AlloyProvider {
        let client = RpcClient::new(self.clone(), false);
        let root: RootProvider = RootProvider::new(client);
        let arc: Arc<dyn Provider<_>> = Arc::new(root);
        AlloyProvider::from_provider(arc)
    }

    /// The served head.
    #[must_use]
    pub fn head(&self) -> u64 {
        self.0.lock().head
    }

    fn serve(&self, req: &SerializedRequest) -> Response {
        let payload = match req.method() {
            "eth_chainId" => {
                let inner = self.0.lock();
                success_payload_value(&Value::String(hex_quantity(inner.chain_id)))
            }
            "eth_blockNumber" => {
                let inner = self.0.lock();
                success_payload_value(&Value::String(hex_quantity(inner.head)))
            }
            "eth_getLogs" => self.serve_get_logs(req),
            "eth_call" => self.serve_eth_call(req),
            other => failure_payload(
                -32601,
                &format!(
                    "scratch chain serves eth_chainId/eth_blockNumber/eth_getLogs/eth_call only, got {other:?}"
                ),
            ),
        };
        Response {
            id: req.id().clone(),
            payload,
        }
    }

    fn serve_packet(&self, req: RequestPacket) -> ResponsePacket {
        match req {
            RequestPacket::Single(single) => ResponsePacket::Single(self.serve(&single)),
            RequestPacket::Batch(batch) => {
                let responses = batch.iter().map(|single| self.serve(single)).collect();
                ResponsePacket::Batch(responses)
            }
        }
    }

    fn serve_get_logs(&self, req: &SerializedRequest) -> ResponsePayload {
        let Some(items) = request_param_array(req) else {
            return failure_payload(-32602, "eth_getLogs params must be one [filter] array");
        };
        let Some(filter) = items.first() else {
            return failure_payload(-32602, "eth_getLogs requires one filter object");
        };
        let (from_block, to_block) = match parse_block_range(filter) {
            Ok(range) => range,
            Err(message) => return failure_payload(-32602, &message),
        };
        let address_filter = match parse_address_filter(filter) {
            Ok(filter) => filter,
            Err(message) => return failure_payload(-32602, &message),
        };
        let topic_filter = match parse_topic_filter(filter) {
            Ok(filter) => filter,
            Err(message) => return failure_payload(-32602, &message),
        };

        let inner = self.0.lock();
        let head = inner.head;
        if to_block == u64::MAX || to_block > head {
            // `latest` and any tag past the head clamp to the head — the
            // ledger never carries logs beyond it (a beyond-head answer would
            // have to invent state).
        }
        let effective_to = to_block.min(head);
        if from_block > effective_to {
            let logs: Vec<Value> = Vec::new();
            return success_payload_value(&Value::Array(logs));
        }
        let logs: Vec<Value> = inner
            .logs
            .iter()
            .filter(|log| log.block >= from_block && log.block <= effective_to)
            .filter(|log| {
                address_filter
                    .as_ref()
                    .is_none_or(|set| set.contains(&log.address))
            })
            .filter(|log| topics_match(topic_filter.as_ref(), &log.topics))
            .map(|log| inner.rpc_log_json(log))
            .collect();
        success_payload_value(&Value::Array(logs))
    }

    fn serve_eth_call(&self, req: &SerializedRequest) -> ResponsePayload {
        let Some(items) = request_param_array(req) else {
            return failure_payload(-32602, "eth_call params must be one [tx, block?] array");
        };
        let Some(tx) = items.first() else {
            return failure_payload(-32602, "eth_call requires one tx object");
        };
        let to = match parse_call_to(tx) {
            Ok(to) => to,
            Err(message) => return failure_payload(-32602, &message),
        };
        let data = match parse_call_input(tx) {
            Ok(data) => data,
            Err(message) => return failure_payload(-32602, &message),
        };
        let block = match items.get(1) {
            None | Some(Value::Null) => {
                let inner = self.0.lock();
                inner.head
            }
            Some(tag) => match parse_block_tag(tag) {
                Ok(block) => block,
                Err(message) => return failure_payload(-32602, &message),
            },
        };

        let inner = self.0.lock();
        if block > inner.head {
            return failure_payload(
                -32602,
                &format!(
                    "eth_call block {block} is beyond the served head {} — the driver must advance the frames first",
                    inner.head
                ),
            );
        }
        // A throwaway EVM over the published snapshot: the frame driver owns
        // the real EVM (thread-local, !Send); served reads rebuild one here
        // and discard it. View contract: served targets are read-only entry
        // points — a mutating target's writes die with the throwaway.
        let mut evm = throwaway_evm(&inner.db_snapshot);
        // Multicall3 `aggregate3` is answered by decoding the envelope and
        // executing every sub-call as a REAL EVM call against the published
        // state (the pool's own `ticks`/`tickBitmap` getters run as real pool
        // bytecode). Anything else executes directly.
        if to == MULTICALL3_ADDRESS {
            if data.get(0..4) != Some(&AGGREGATE3_SELECTOR) {
                return failure_payload(
                    -32602,
                    "eth_call to Multicall3 must carry the aggregate3 selector (no silent fallback)",
                );
            }
            let calls = match decode_aggregate3_calls(&data) {
                Ok(calls) => calls,
                Err(message) => return failure_payload(-32602, &message),
            };
            let mut results = Vec::with_capacity(calls.len());
            for (target, calldata) in calls {
                let outcome = oracle::call_bytes(&mut evm, target, calldata, SUBCALL_GAS);
                match outcome {
                    Ok(return_data) => results.push((true, return_data)),
                    // allowFailure semantics: a reverted/halted sub-call is
                    // success=false + empty data (the verifier's
                    // "revert ⇒ zero" reading).
                    Err(_) => results.push((false, Bytes::new())),
                }
            }
            return match encode_aggregate3_results(&results) {
                Ok(encoded) => success_payload_value(&Value::String(hex_data(encoded.as_ref()))),
                Err(message) => failure_payload(-32603, &message),
            };
        }
        match oracle::call_bytes(&mut evm, to, data, CALL_GAS) {
            Ok(out) => success_payload_value(&Value::String(hex_data(out.as_ref()))),
            // A revert surfaces as the JSON-RPC revert error a live node
            // answers (code 3) — no silent classification of a failed read.
            Err(revert) => failure_payload(3, &format!("execution reverted: {revert}")),
        }
    }
}

impl ScratchChainInner {
    /// The alloy wire shape of one scratch log (the exact JSON a live node
    /// serves and the recorder records): minimal hex quantities,
    /// deterministic block/tx hashes.
    fn rpc_log_json(&self, log: &ScratchLog) -> Value {
        let block_hash = block_hash(log.block);
        let tx_hash = tx_hash(log.block, log.tx_index);
        let topics: Vec<Value> = log
            .topics
            .iter()
            .map(|topic| Value::String(hex_data(topic.as_slice())))
            .collect();
        serde_json::json!({
            "address": hex_address(&log.address),
            "topics": topics,
            "data": hex_data(log.data.as_ref()),
            "blockNumber": hex_quantity(log.block),
            "transactionHash": hex_data(tx_hash.as_slice()),
            "transactionIndex": hex_quantity(log.tx_index),
            "blockHash": hex_data(block_hash.as_slice()),
            "logIndex": hex_quantity(log.log_index),
            "removed": false,
            "blockTimestamp": hex_quantity(self.timestamp_base.saturating_add(log.block)),
        })
    }
}

/// The frame driver: owns the revm EVM (thread-local — it is `!Send`) and
/// publishes each frame's committed state into the served [`ScratchChain`].
pub struct ScratchDriver {
    evm: FixtureEvm,
    chain: ScratchChain,
}

impl ScratchDriver {
    /// A fresh chain: the driver EVM (nonce check off — frames carry no
    /// signatures) and the served state at `chain_id`/`head`. Block `b`
    /// serves timestamp `timestamp_base + b` (deterministic).
    #[must_use]
    pub fn new(chain_id: u64, head: u64, timestamp_base: u64) -> Self {
        let mut evm = oracle::new_fixture_evm();
        oracle::set_disable_nonce_check(&mut evm, true);
        // The capture harness embeds the full canonical UniswapV3Pool plus
        // the mint/burn/announce entries — it exceeds EIP-170's deployed
        // size, so the fixture caps are set to usize::MAX. (revm 43's `None`
        // means the SPEC limit, not unlimited — the oracle helper's doc
        // notwithstanding.)
        oracle::set_code_size_limits(&mut evm, Some(usize::MAX));
        let chain = ScratchChain(Arc::new(Mutex::new(ScratchChainInner {
            chain_id,
            head,
            timestamp_base,
            logs: Vec::new(),
            next_tx_index: 0,
            next_log_index: 0,
            db_snapshot: CacheDB::new(EmptyDB::default()),
        })));
        Self { evm, chain }
    }

    /// The served handle (for [`ScratchChain::as_alloy_provider`]).
    #[must_use]
    pub fn chain(&self) -> &ScratchChain {
        &self.chain
    }

    /// Advance the served head to `block` (monotonic). Frames executed after
    /// this land in `block`; `eth_blockNumber` serves it.
    ///
    /// # Errors
    ///
    /// A non-monotonic advance is a generator bug: rewinding the head would
    /// serve `eth_getLogs` windows that never existed.
    pub fn advance_to(&mut self, block: u64) -> Result<(), String> {
        let mut inner = self.chain.0.lock();
        if block < inner.head {
            return Err(format!(
                "scratch chain head must advance monotonically: {block} < {}",
                inner.head
            ));
        }
        inner.head = block;
        inner.next_tx_index = 0;
        inner.next_log_index = 0;
        Ok(())
    }

    /// Deploy a contract (`init_code` = creation bytecode [+ constructor
    /// args]) as one frame in the current block. The frame's real logs join
    /// the block's ledger, and the committed state is published.
    ///
    /// # Errors
    ///
    /// Returns the fixture-driver verdict when the deploy reverts or halts —
    /// a scenario whose frame fails is a generator bug, not a served answer.
    pub fn deploy_frame(&mut self, init_code: Bytes, gas: u64) -> Result<Address, String> {
        let verdict = oracle::transact(&mut self.evm, TxSpec::Deploy { init_code, gas });
        let address = match verdict {
            Verdict::Accepted { output, logs } => {
                let addr = match output {
                    Output::Create(_, Some(addr)) => addr,
                    other @ (Output::Call(_) | Output::Create(_, None)) => {
                        return Err(format!("scratch deploy produced {other:?}"))
                    }
                };
                self.record_logs(logs);
                addr
            }
            Verdict::Reverted(r) => return Err(format!("scratch deploy reverted: {r:?}")),
            Verdict::Halted(h) => return Err(format!("scratch deploy halted: {h}")),
        };
        self.bump_tx_and_publish();
        Ok(address)
    }

    /// Execute one state-mutating call frame in the current block. The
    /// frame's real logs join the block's ledger, and the committed state is
    /// published.
    ///
    /// # Errors
    ///
    /// Returns the revert data / halt reason verbatim.
    pub fn call_frame(&mut self, to: Address, data: Bytes, gas: u64) -> Result<Bytes, String> {
        let verdict = oracle::transact(&mut self.evm, TxSpec::Call { to, data, gas });
        let out = match verdict {
            Verdict::Accepted { output, logs } => {
                let bytes = match output {
                    Output::Call(b) => b,
                    other @ Output::Create(..) => {
                        return Err(format!("scratch call produced {other:?}"))
                    }
                };
                self.record_logs(logs);
                bytes
            }
            Verdict::Reverted(r) => {
                let reason = oracle::decode_error_string(&r)
                    .map_or_else(|| format!("raw {r:?}"), |decoded| format!("{decoded:?}"));
                return Err(format!("scratch call reverted: {reason}"));
            }
            Verdict::Halted(h) => return Err(format!("scratch call halted: {h}")),
        };
        self.bump_tx_and_publish();
        Ok(out)
    }

    /// Execute a READ-ONLY call on the driver EVM (a getter like `pool()`).
    /// No log record, no tx bump, no publish — the fixture driver commits,
    /// but the contract keeps view targets read-only.
    ///
    /// # Errors
    ///
    /// Returns the revert data / halt reason verbatim.
    pub fn call_view(&mut self, to: Address, data: Bytes, gas: u64) -> Result<Bytes, String> {
        oracle::call_bytes(&mut self.evm, to, data, gas)
    }

    fn record_logs(&mut self, logs: Vec<alloy::primitives::Log>) {
        let mut inner = self.chain.0.lock();
        for log in logs {
            let entry = ScratchLog {
                block: inner.head,
                tx_index: inner.next_tx_index,
                log_index: inner.next_log_index,
                address: log.address,
                topics: log.topics().to_vec(),
                data: log.data.data.clone(),
            };
            inner.next_log_index += 1;
            inner.logs.push(entry);
        }
    }

    fn bump_tx_and_publish(&mut self) {
        let mut inner = self.chain.0.lock();
        inner.next_tx_index += 1;
        // Publish the committed state: a plain-map clone (Send) — the served
        // reads rebuild throwaway EVMs over exactly this state.
        inner.db_snapshot = self.evm.ctx.db_mut().clone();
    }
}

/// Gas the served `eth_call` grants a direct (non-multicall) read. Stays at
/// or under the oracle transact cap (2^24): a served call is a real
/// `transact` on the fixture EVM, and a gas limit past that cap is refused
/// (`TxGasLimitGreaterThanCap`) — the first scratch-chain `eth_call` (the
/// aave discount pre-pass) drove this in.
const CALL_GAS: u64 = 16_777_216;
/// Gas each Multicall3 sub-call grants its real EVM execution.
const SUBCALL_GAS: u64 = 2_000_000;

/// The alloy transport view of a [`ScratchChain`] — the same seam shape the
/// `RecordingTransport` implements: a `tower::Service` over JSON-RPC packets
/// that `RpcClient` boxes. Every answer is computed synchronously inside
/// [`ScratchChain::serve_packet`] (deterministic, one answer per request);
/// the future merely hands it back, so the future stays `Send`.
impl tower::Service<RequestPacket> for ScratchChain {
    type Response = ResponsePacket;
    type Error = alloy::transports::TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: RequestPacket) -> Self::Future {
        let resp = self.serve_packet(req);
        Box::pin(async move { Ok(resp) })
    }
}

// ── payload builders (infallible JSON → JSON-RPC; a serialization failure
// of a served answer is a loud error, never a default) ────────────

fn success_payload_value(value: &Value) -> ResponsePayload {
    match serde_json::to_string(&value) {
        Ok(json) => payload_from_raw_json(json),
        Err(e) => failure_payload(
            -32700,
            &format!("scratch chain failed to serialize its own answer: {e}"),
        ),
    }
}

fn payload_from_raw_json(json: String) -> ResponsePayload {
    match serde_json::value::RawValue::from_string(json) {
        Ok(raw) => ResponsePayload::Success(raw),
        Err(e) => failure_payload(
            -32700,
            &format!("scratch chain produced non-JSON answer bytes: {e}"),
        ),
    }
}

fn failure_payload(code: i64, message: &str) -> ResponsePayload {
    ResponsePayload::Failure(ErrorPayload {
        code,
        message: std::borrow::Cow::Owned(message.to_string()),
        data: None,
    })
}

/// The decoded `params` of one request (None when absent/unparseable).
fn request_params(req: &SerializedRequest) -> Option<Value> {
    let raw = req.params()?;
    serde_json::from_str::<Value>(raw.get()).ok()
}

fn request_param_array(req: &SerializedRequest) -> Option<Vec<Value>> {
    match request_params(req)? {
        Value::Array(items) => Some(items),
        _ => None,
    }
}

// ── wire-form helpers (minimal hex — the same canonical wire form a live
// node emits, which the recorder's precision rule decimalizes exactly) ──

fn hex_quantity(value: u64) -> String {
    format!("0x{value:x}")
}

fn hex_data(bytes: &[u8]) -> String {
    format!("0x{}", alloy::primitives::hex::encode(bytes))
}

fn hex_address(address: &Address) -> String {
    format!("0x{}", alloy::primitives::hex::encode(address.as_slice()))
}

/// Deterministic block hash: `keccak256(block_number)` — a pure function of
/// the coordinate, no chain semantics implied.
fn block_hash(block: u64) -> B256 {
    keccak256(U256::from(block).to_be_bytes::<32>())
}

/// Deterministic tx hash: `keccak256(block || tx_index)`.
fn tx_hash(block: u64, tx_index: u64) -> B256 {
    let mut preimage = [0u8; 16];
    preimage[..8].copy_from_slice(&block.to_be_bytes());
    preimage[8..].copy_from_slice(&tx_index.to_be_bytes());
    keccak256(preimage)
}

/// A throwaway read EVM over the published snapshot (nonce check off — the
/// served call carries no signature; base fee is the fixture default 0).
fn throwaway_evm(snapshot: &CacheDB<EmptyDB>) -> FixtureEvm {
    let mut evm = oracle::new_fixture_evm();
    oracle::set_disable_nonce_check(&mut evm, true);
    *evm.ctx.db_mut() = snapshot.clone();
    evm
}

// ── request parsing (strict: every malformed shape is a JSON-RPC error,
// never a default) ────────────────────────────────────────────────

/// Parse a block tag: minimal-hex quantity, decimal string, `latest`, or
/// `earliest` (0). Anything else is refused.
fn parse_block_tag(tag: &Value) -> Result<u64, String> {
    let Value::String(s) = tag else {
        return Err(format!("block tag {tag} must be a string"));
    };
    if s == "latest" {
        return Ok(u64::MAX);
    }
    if s == "earliest" {
        return Ok(0);
    }
    if let Some(hex) = s.strip_prefix("0x") {
        let digits = if hex.is_empty() { "0" } else { hex };
        return u64::from_str_radix(digits, 16).map_err(|e| format!("block tag {s:?}: {e}"));
    }
    s.parse::<u64>()
        .map_err(|e| format!("block tag {s:?}: {e}"))
}

fn parse_block_range(filter: &Value) -> Result<(u64, u64), String> {
    let from = match filter.get("fromBlock") {
        None | Some(Value::Null) => 0,
        Some(tag) => parse_block_tag(tag)?,
    };
    let to = match filter.get("toBlock") {
        None | Some(Value::Null) => u64::MAX,
        Some(tag) => parse_block_tag(tag)?,
    };
    if from > to {
        return Err(format!("eth_getLogs range inverted: {from} > {to}"));
    }
    Ok((from, to))
}

fn parse_address_filter(filter: &Value) -> Result<Option<Vec<Address>>, String> {
    let Some(value) = filter.get("address") else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(vec![parse_strict_address(s)?])),
        Value::Array(items) => {
            let mut set = Vec::with_capacity(items.len());
            for item in items {
                let Value::String(s) = item else {
                    return Err("eth_getLogs address array entries must be strings".to_string());
                };
                set.push(parse_strict_address(s)?);
            }
            Ok(Some(set))
        }
        other => Err(format!(
            "eth_getLogs address filter {other:?} not supported"
        )),
    }
}

fn parse_strict_address(s: &str) -> Result<Address, String> {
    s.parse::<Address>()
        .map_err(|e| format!("eth_getLogs address {s:?}: {e}"))
}

/// One topics-filter position: `None` (absent/null = wildcard) or an
/// explicit OR-set of topic hashes.
type TopicFilter = Vec<Option<Vec<B256>>>;

fn parse_topic_filter(filter: &Value) -> Result<Option<TopicFilter>, String> {
    let Some(value) = filter.get("topics") else {
        return Ok(None);
    };
    let Value::Array(positions) = value else {
        return Err("eth_getLogs topics must be an array".to_string());
    };
    let mut parsed: TopicFilter = Vec::with_capacity(positions.len());
    for position in positions {
        match position {
            Value::Null => parsed.push(None),
            Value::String(s) => parsed.push(Some(vec![parse_strict_topic(s)?])),
            Value::Array(set) => {
                let mut alternatives = Vec::with_capacity(set.len());
                for item in set {
                    let Value::String(s) = item else {
                        return Err("eth_getLogs topic alternatives must be strings".to_string());
                    };
                    alternatives.push(parse_strict_topic(s)?);
                }
                parsed.push(Some(alternatives));
            }
            other => {
                return Err(format!(
                    "eth_getLogs topics position {other:?} not supported"
                ));
            }
        }
    }
    Ok(Some(parsed))
}

fn parse_strict_topic(s: &str) -> Result<B256, String> {
    s.parse::<B256>()
        .map_err(|e| format!("eth_getLogs topic {s:?}: {e}"))
}

/// Positional topic matching: wildcard positions skip; explicit positions
/// require the log's topic at that index to be in the OR-set.
fn topics_match(filter: Option<&TopicFilter>, log_topics: &[B256]) -> bool {
    let Some(positions) = filter else {
        return true;
    };
    for (index, position) in positions.iter().enumerate() {
        let Some(alternatives) = position else {
            continue;
        };
        match log_topics.get(index) {
            Some(topic) if alternatives.contains(topic) => {}
            _ => return false,
        }
    }
    true
}

fn parse_call_to(tx: &Value) -> Result<Address, String> {
    let Some(Value::String(s)) = tx.get("to") else {
        return Err("eth_call requires a string `to` (creates are not served)".to_string());
    };
    s.parse::<Address>()
        .map_err(|e| format!("eth_call to {s:?}: {e}"))
}

fn parse_call_input(tx: &Value) -> Result<Bytes, String> {
    // alloy serializes the calldata under `input`; `data` is the legacy
    // alias some callers use. Both decode strictly.
    let field = tx.get("input").or_else(|| tx.get("data"));
    match field {
        None | Some(Value::Null) => Ok(Bytes::new()),
        Some(Value::String(s)) => {
            let hex = s.strip_prefix("0x").unwrap_or(s);
            alloy::primitives::hex::decode(hex)
                .map(Bytes::from)
                .map_err(|e| format!("eth_call input {s:?}: {e}"))
        }
        other => Err(format!("eth_call input {other:?} not supported")),
    }
}

// ── Multicall3 aggregate3 (strict envelope decode + encode) ──────

type AggregateCall = (Address, Bytes);

/// Decode `aggregate3((bool,address,bytes)[])` calldata into its sub-calls.
/// Each element is the `Call3` TUPLE `(bool allowFailure, address target,
/// bytes callData)`: two static head words + the dynamic `bytes` offset, the
/// offset target carrying `[len][data]`. Every offset/length is
/// bounds-checked — a malformed envelope is an Err, never a guess.
fn decode_aggregate3_calls(data: &Bytes) -> Result<Vec<AggregateCall>, String> {
    const WORD: usize = 32;
    let bytes = data.as_ref();
    if bytes.len() < 4 + WORD {
        return Err(format!(
            "aggregate3 calldata {} bytes too short for a head word",
            bytes.len()
        ));
    }
    let read_word = |offset: usize| -> Result<[u8; 32], String> {
        if offset + WORD > bytes.len() {
            return Err(format!(
                "aggregate3 word at {offset} out of bounds (len {})",
                bytes.len()
            ));
        }
        let mut word = [0u8; 32];
        word.copy_from_slice(&bytes[offset..offset + WORD]);
        Ok(word)
    };
    let word_usize = |word: &[u8; 32]| -> Result<usize, String> {
        if word[..16].iter().any(|&b| b != 0) {
            return Err("aggregate3 offset/length exceeds u128".to_string());
        }
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&word[24..32]);
        usize::try_from(u64::from_be_bytes(buf))
            .map_err(|_| "aggregate3 offset/length exceeds usize".to_string())
    };

    let array_offset = word_usize(&read_word(4)?)?;
    if array_offset != WORD {
        return Err(format!(
            "aggregate3 array offset {array_offset} != head width 32"
        ));
    }
    let count = word_usize(&read_word(4 + array_offset)?)?;
    if count > 10_000 {
        return Err(format!(
            "aggregate3 count {count} exceeds the sane batch bound 10000"
        ));
    }
    let element_offsets_base = 4 + array_offset + WORD;
    let mut calls = Vec::with_capacity(count);
    for index in 0..count {
        let element_offset = word_usize(&read_word(element_offsets_base + index * WORD)?)?;
        let element_base = element_offsets_base + element_offset;
        // Tuple head: [allowFailure][target][bytes offset]. The offset target
        // carries [len][data] (relative to the tuple start).
        let _allow_failure = read_word(element_base)?;
        let target_word = read_word(element_base + WORD)?;
        if target_word[..12].iter().any(|&b| b != 0) {
            return Err("aggregate3 target word must be a padded address".to_string());
        }
        let target = Address::from_slice(&target_word[12..32]);
        let bytes_offset = word_usize(&read_word(element_base + 2 * WORD)?)?;
        let len_pos = element_base
            .checked_add(bytes_offset)
            .ok_or_else(|| "aggregate3 bytes offset overflow".to_string())?;
        let data_len = word_usize(&read_word(len_pos)?)?;
        let data_base = len_pos + WORD;
        if data_base + data_len > bytes.len() {
            return Err(format!(
                "aggregate3 sub-call {index} data [{data_base}..{}] out of bounds (len {})",
                data_base + data_len,
                bytes.len()
            ));
        }
        calls.push((
            target,
            Bytes::copy_from_slice(&bytes[data_base..data_base + data_len]),
        ));
    }
    Ok(calls)
}

/// Encode `(bool,bytes)[]` — the `aggregate3` return shape — for the decoded
/// results. Deterministic canonical ABI layout (verified byte-for-byte
/// against alloy's `DynSolValue::abi_encode` in the module tests): one
/// offset word per element (offsets relative to the offset table, which
/// starts right after `count`), each element body `[success, bytes_offset,
/// bytes_len, zero-padded data]`.
fn encode_aggregate3_results(results: &[(bool, Bytes)]) -> Result<Bytes, String> {
    const WORD: usize = 32;
    if results.len() > 10_000 {
        return Err(format!(
            "aggregate3 result count {} exceeds the sane batch bound",
            results.len()
        ));
    }
    // Element body: [success][bytes_offset][bytes_len][padded data].
    let body_sizes: Vec<usize> = results
        .iter()
        .map(|(_, data)| 3 * WORD + data.len().next_multiple_of(WORD))
        .collect();
    let table_size = results.len() * WORD;
    let mut out: Vec<u8> = Vec::with_capacity(WORD + table_size + body_sizes.iter().sum::<usize>());
    push_word(&mut out, WORD as u64); // the outer offset of the dynamic return
    push_word(&mut out, results.len() as u64); // count
                                               // Offset table: element i starts after the table plus all earlier bodies.
    let mut running = table_size;
    for size in &body_sizes {
        push_word(&mut out, running as u64);
        running += *size;
    }
    // Element bodies.
    for (success, return_data) in results {
        push_word(&mut out, u64::from(*success));
        push_word(&mut out, (2 * WORD) as u64); // bytes offset within the tuple
        push_word(&mut out, return_data.len() as u64);
        out.extend_from_slice(return_data.as_ref());
        while !out.len().is_multiple_of(WORD) {
            out.push(0);
        }
    }
    Ok(Bytes::from(out))
}

fn push_word(buf: &mut Vec<u8>, value: u64) {
    buf.extend_from_slice(&[0u8; 24]);
    buf.extend_from_slice(&value.to_be_bytes());
}

// ── V3CaptureHarness frame vocabulary (the wave-2 generator's calls) ──

/// Load the committed `V3CaptureHarness` creation bytecode (the real
/// UniswapV3Pool deployer/callback harness, artifact + manifest drift-gated
/// by `tier3_harness_artifacts.rs`).
///
/// # Errors
///
/// Passthrough of the artifact loader's error.
pub fn load_capture_harness_creation_bytecode(
    artifact_dir: &std::path::Path,
) -> Result<Vec<u8>, String> {
    oracle::load_foundry_creation_bytecode(artifact_dir, "V3CaptureHarness.sol", "V3CaptureHarness")
}

/// Sign-extended 32-byte big-endian word for a signed Solidity int
/// (int24/int256 alike — the encoder law the ABI encoder pins).
fn word_i32(value: i32) -> [u8; 32] {
    alloy::primitives::aliases::I256::try_from(value)
        .unwrap_or(alloy::primitives::aliases::I256::ZERO)
        .into_raw()
        .to_be_bytes::<32>()
}

fn word_u128(value: u128) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[16..].copy_from_slice(&value.to_be_bytes());
    word
}

fn selector_call(sig: &str, args: Vec<[u8; 32]>) -> Bytes {
    let mut data = Vec::with_capacity(4 + args.len() * 32);
    data.extend_from_slice(&oracle::selector(sig));
    for word in args {
        data.extend_from_slice(&word);
    }
    Bytes::from(data)
}

/// `initialize(uint160 sqrtPriceX96)` on the capture harness.
#[must_use]
pub fn encode_capture_initialize(sqrt_price_x96: U256) -> Bytes {
    let mut word = [0u8; 32];
    word.copy_from_slice(&sqrt_price_x96.to_be_bytes::<32>());
    selector_call("initialize(uint160)", vec![word])
}

/// `setupPool()` — deploys the real `UniswapV3Pool` (the deferred CREATE so
/// the code deposit gets full gas).
#[must_use]
pub fn encode_capture_setup_pool() -> Bytes {
    selector_call("setupPool()", vec![])
}

/// `announcePool()` — the REAL `PoolCreated` event the creation fetch filters
/// on (the harness is the factory role).
#[must_use]
pub fn encode_capture_announce_pool() -> Bytes {
    selector_call("announcePool()", vec![])
}

/// `pool()` — the deployed pool's address getter.
#[must_use]
pub fn encode_capture_pool_getter() -> Bytes {
    selector_call("pool()", vec![])
}

/// `mint(int24 tickLower, int24 tickUpper, uint128 amount)` — the harness
/// pays the real pool through the mint callback.
#[must_use]
pub fn encode_capture_mint(tick_lower: i32, tick_upper: i32, amount: u128) -> Bytes {
    selector_call(
        "mint(int24,int24,uint128)",
        vec![
            word_i32(tick_lower),
            word_i32(tick_upper),
            word_u128(amount),
        ],
    )
}

/// `burn(int24 tickLower, int24 tickUpper, uint128 amount)` — the harness is
/// the position owner.
#[must_use]
pub fn encode_capture_burn(tick_lower: i32, tick_upper: i32, amount: u128) -> Bytes {
    selector_call(
        "burn(int24,int24,uint128)",
        vec![
            word_i32(tick_lower),
            word_i32(tick_upper),
            word_u128(amount),
        ],
    )
}

/// `swap(bool zeroForOne, int256 amountSpecified, uint160
/// sqrtPriceLimitX96)` — the harness pays the real pool through the swap
/// callback.
#[must_use]
pub fn encode_capture_swap(
    zero_for_one: bool,
    amount_specified: alloy::primitives::aliases::I256,
    sqrt_price_limit: U256,
) -> Bytes {
    let mut bool_word = [0u8; 32];
    bool_word[31] = u8::from(zero_for_one);
    let amount_word = amount_specified.into_raw().to_be_bytes::<32>();
    let mut limit_word = [0u8; 32];
    limit_word.copy_from_slice(&sqrt_price_limit.to_be_bytes::<32>());
    selector_call(
        "swap(bool,int256,uint160)",
        vec![bool_word, amount_word, limit_word],
    )
}

/// Hand-assembled initcode that emits ONE real `LOGn` (topics + data) during
/// its own deployment frame, then stops — the contract-agnostic way frames
/// carry short / non-minimal hex DATA fields (the cassette precision-rule
/// surface) without any Solidity toolchain.
///
/// Bytecode: `PUSH1 len; PUSH1 off; PUSH1 0; CODECOPY;` (payload → memory)
/// then the topic words, `PUSH1 len; PUSH1 0; LOGn; STOP; <data>` — the LOGn
/// stack convention (μs[0] = offset on top, topics deepest).
///
/// Topic order is as-produced: the emitter serves its topics in the order
/// the wave-2 corpus was recorded with, and the committed pin test
/// `log_emitter_initcode_is_exact_stack_order` locks that order. Consumers
/// must treat recorded emitter topics as as-produced — a future order
/// change is a corpus regen decision, not a silent fix.
///
/// # Errors
///
/// Errors when there are no topics, more than 4, or the data exceeds a
/// `PUSH1` length bound (255 bytes) — the generator must know its emitter
/// did not silently degrade.
pub fn log_emitter_initcode(topics: &[B256], data: &[u8]) -> Result<Vec<u8>, String> {
    if topics.is_empty() || topics.len() > 4 {
        return Err(format!(
            "log emitter needs 1..=4 topics, got {}",
            topics.len()
        ));
    }
    if data.is_empty() || data.len() > 255 {
        return Err(format!(
            "log emitter data {} bytes outside the 1..=255 PUSH1 bound",
            data.len()
        ));
    }
    let header_len = 7 + topics.len() * 33 + 2 + 2 + 1 + 1;
    let off = u8::try_from(header_len).map_err(|_| "emitter header overflow".to_string())?;
    let len = u8::try_from(data.len()).map_err(|_| "emitter data overflow".to_string())?;
    let mut code = Vec::with_capacity(header_len + data.len());
    // CODECOPY(dest=0, src=off, size=len): stack (bottom→top) size, src, dest.
    code.push(0x60);
    code.push(len); // size (deepest)
    code.push(0x60);
    code.push(off); // src
    code.push(0x60);
    code.push(0x00); // dest (top)
    code.push(0x39); // CODECOPY
                     // LOGn: stack (bottom→top) topics t1..tN, size, offset.
    for topic in topics {
        code.push(0x7f); // `PUSH32` topic
        code.extend_from_slice(topic.as_slice());
    }
    code.push(0x60);
    code.push(len); // size
    code.push(0x60);
    code.push(0x00); // offset (top)
    code.push(0xa0 + u8::try_from(topics.len()).map_err(|_| "topic count overflow".to_string())?); // LOGn
    code.push(0x00); // STOP
    debug_assert_eq!(code.len(), header_len);
    code.extend_from_slice(data);
    Ok(code)
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn word_u64(value: u64) -> [u8; 32] {
        let mut word = [0u8; 32];
        word[24..].copy_from_slice(&value.to_be_bytes());
        word
    }

    // ── wire-form helpers: independent literals ──

    #[test]
    fn hex_quantity_is_minimal_wire_form() {
        assert_eq!(hex_quantity(0), "0x0");
        assert_eq!(hex_quantity(1), "0x1"); // the odd-digit quantity
        assert_eq!(hex_quantity(4_660), "0x1234");
        assert_eq!(hex_quantity(117_300_739), "0x6fdde03");
    }

    #[test]
    fn hex_data_is_lowercase_and_empty_is_bare_0x() {
        assert_eq!(hex_data(&[]), "0x");
        assert_eq!(hex_data(&[0x00]), "0x00");
        assert_eq!(hex_data(&[0x06, 0xfd, 0xde, 0x03]), "0x06fdde03");
    }

    #[test]
    fn block_tag_parsing_is_strict() {
        assert_eq!(parse_block_tag(&Value::String("0x10".into())).unwrap(), 16);
        assert_eq!(parse_block_tag(&Value::String("16".into())).unwrap(), 16);
        assert_eq!(
            parse_block_tag(&Value::String("latest".into())).unwrap(),
            u64::MAX
        );
        assert_eq!(
            parse_block_tag(&Value::String("earliest".into())).unwrap(),
            0
        );
        assert!(parse_block_tag(&Value::String("0xzz".into())).is_err());
        assert!(parse_block_tag(&Value::String("pending".into())).is_err());
        assert!(parse_block_tag(&Value::Number(16.into())).is_err());
    }

    #[test]
    fn topics_match_is_positional_with_or_sets() {
        let t = B256::from([1u8; 32]);
        let u = B256::from([2u8; 32]);
        let filter: Option<TopicFilter> = Some(vec![Some(vec![t, u]), None]);
        assert!(topics_match(filter.as_ref(), &[t, u]));
        assert!(topics_match(filter.as_ref(), &[u, t]));
        assert!(!topics_match(filter.as_ref(), &[B256::from([3u8; 32]), t]));
        assert!(topics_match(None, &[t]));
        // A filter position past the log's topic count never matches.
        let deep: Option<TopicFilter> = Some(vec![None, None, None, None, Some(vec![t])]);
        assert!(!topics_match(deep.as_ref(), &[t, u, t, u]));
    }

    #[test]
    fn served_log_json_deserializes_as_an_alloy_rpc_log() {
        // The exact served shape must be consumable by the same alloy Log
        // type the replay transport and the decoders use.
        let inner = ScratchChainInner {
            chain_id: 1,
            head: 4,
            timestamp_base: 1_700_000_000,
            logs: Vec::new(),
            next_tx_index: 0,
            next_log_index: 0,
            db_snapshot: CacheDB::new(EmptyDB::default()),
        };
        let log = ScratchLog {
            block: 1001,
            tx_index: 2,
            log_index: 7,
            address: Address::from([9u8; 20]),
            topics: vec![B256::from([1u8; 32]), B256::from([2u8; 32])],
            data: Bytes::from(vec![0x00, 0x01, 0x02]),
        };
        let json = inner.rpc_log_json(&log);
        let parsed: alloy::rpc::types::Log =
            serde_json::from_value(json.clone()).expect("served log must be an alloy Log");
        assert_eq!(parsed.inner.address, log.address);
        assert_eq!(parsed.inner.topics(), log.topics.as_slice());
        assert_eq!(parsed.inner.data.data, log.data);
        assert_eq!(json["data"], "0x000102");
        assert_eq!(json["blockNumber"], "0x3e9");
        assert_eq!(json["removed"], false);
    }

    #[test]
    fn aggregate3_round_trips_through_the_real_consumer_decoder() {
        // Encode the results the scratch chain serves and decode them with
        // degenbot-rpc's aggregate3 consumer (the exact decoder the
        // verification gate runs) — the two sides must agree byte-for-byte.
        let results = vec![
            (true, Bytes::from(vec![1u8; 32])),
            (false, Bytes::new()),
            (true, Bytes::from(vec![0x00, 0xff])),
        ];
        let encoded = encode_aggregate3_results(&results).expect("encode");
        let decoded = degenbot_rpc::multicall3::decode_aggregate3_results(&encoded, results.len())
            .expect("the consumer decoder must accept the served encoding");
        assert_eq!(decoded.len(), results.len());
        for (served, consumed) in results.iter().zip(&decoded) {
            assert_eq!(consumed.success, served.0);
            assert_eq!(consumed.return_data, served.1);
        }
    }

    #[test]
    fn aggregate3_decode_rejects_malformed_envelopes() {
        // Truncated head.
        assert!(decode_aggregate3_calls(&Bytes::from(vec![0x13, 0x4d])).is_err());
        // Wrong array offset.
        let mut data = vec![0x13, 0x4d, 0xd3, 0x43];
        data.extend_from_slice(&word_u64(64)); // not 32
        assert!(decode_aggregate3_calls(&Bytes::from(data)).is_err());
        // Non-padded target word.
        let mut data = vec![0x13, 0x4d, 0xd3, 0x43];
        data.extend_from_slice(&word_u64(32));
        data.extend_from_slice(&word_u64(1)); // count
        data.extend_from_slice(&word_u64(0x20)); // element offset
        data.extend_from_slice(&word_u64(1)); // allowFailure
        let mut target = word_u64(0);
        target[0] = 0xff; // padding bytes must be zero
        data.extend_from_slice(&target);
        data.extend_from_slice(&word_u64(0x60)); // bytes offset
        data.extend_from_slice(&word_u64(0)); // bytes len
        assert!(decode_aggregate3_calls(&Bytes::from(data)).is_err());
    }

    #[test]
    fn aggregate3_decode_recovers_exact_subcalls() {
        // A single sub-call to a known address with known calldata, in the
        // REAL Call3 tuple shape the verifier's encoder emits: element head
        // [allowFailure][target][bytes offset], offset target [len][data].
        let target = Address::from([7u8; 20]);
        let calldata = [vec![0x53, 0x39, 0xc2, 0x96], vec![0xff; 32]].concat();
        let mut data = vec![0x13, 0x4d, 0xd3, 0x43];
        data.extend_from_slice(&word_u64(32)); // array offset
        data.extend_from_slice(&word_u64(1)); // count
        data.extend_from_slice(&word_u64(0x20)); // element offset (from table)
        data.extend_from_slice(&word_u64(1)); // allowFailure = true
        let mut target_word = [0u8; 32];
        target_word[12..].copy_from_slice(target.as_slice());
        data.extend_from_slice(&target_word);
        data.extend_from_slice(&word_u64(0x60)); // bytes offset in tuple
        data.extend_from_slice(&word_u64(calldata.len() as u64));
        data.extend_from_slice(&calldata);
        while data.len() % 32 != 0 {
            data.push(0);
        }
        let decoded = decode_aggregate3_calls(&Bytes::from(data)).expect("decode");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].0, target);
        assert_eq!(decoded[0].1.as_ref(), calldata.as_slice());
    }

    #[test]
    fn log_emitter_initcode_is_exact_stack_order() {
        let topic = B256::from([1u8; 32]);
        let code = log_emitter_initcode(&[topic], &[0x00, 0x01]).expect("build");
        // PUSH1 len; PUSH1 off; PUSH1 0; CODECOPY; topic word; PUSH1 len;
        // PUSH1 0; LOG1; STOP; 00 01
        let header = 7 + 33 + 2 + 2 + 1 + 1;
        assert_eq!(code[0], 0x60);
        assert_eq!(code[1], 2); // CODECOPY size
        assert_eq!(code[2], 0x60);
        assert_eq!(code[3], u8::try_from(header).unwrap()); // src
        assert_eq!(code[4], 0x60);
        assert_eq!(code[5], 0x00); // dest
        assert_eq!(code[6], 0x39); // CODECOPY
        assert_eq!(code[7], 0x7f); // `PUSH32` topic
        assert_eq!(&code[8..40], topic.as_slice());
        assert_eq!(code[40], 0x60);
        assert_eq!(code[41], 2); // size
        assert_eq!(code[42], 0x60);
        assert_eq!(code[43], 0x00); // offset (top)
        assert_eq!(code[44], 0xa1); // LOG1
        assert_eq!(code[45], 0x00); // STOP
        assert_eq!(&code[46..], &[0x00, 0x01]);
    }

    #[test]
    fn log_emitter_initcode_rejects_out_of_grammar_shapes() {
        let topic = B256::from([1u8; 32]);
        assert!(log_emitter_initcode(&[], &[0x00]).is_err());
        assert!(log_emitter_initcode(&[topic; 5], &[0x00]).is_err());
        assert!(log_emitter_initcode(&[topic], &[]).is_err());
        assert!(log_emitter_initcode(&[topic], &[0u8; 256]).is_err());
    }
}
