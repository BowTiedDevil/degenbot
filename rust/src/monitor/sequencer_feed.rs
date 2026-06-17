//! Sequencer-feed analysis: turn raw `BroadcastFeedMessage`s into
//! `FrontrunCandidate` IPC events.
//!
//! Phase F2 of the frontrun epic — the free non-express path of the
//! Arbitrum sequencer reaction window. We consume the mpsc that
//! [`crate::monitor::sequencer_subscriber`] populates, peel the L2
//! envelope down to an EIP-2718 transaction, and pattern-match the
//! function selector against an allow-list of frontrun-target signatures.
//! Matching txs are surfaced as `Message::FrontrunCandidate` on the
//! engine→coordinator broadcast bus.
//!
//! # Layering
//!
//! - [`sequencer_subscriber`]: tokio-tungstenite transport + JSON
//!   envelope parse → `mpsc::Receiver<BroadcastFeedMessage>` (TRANSPORT).
//! - [`sequencer_feed`] (this module): consume that receiver, decode the
//!   nested L2 message, match selector, emit IPC (ANALYSIS).
//!
//! Keeping these in separate files preserves the transport file's
//! single responsibility and gives this module a clean seam for unit
//! tests that don't need a WebSocket fixture.
//!
//! # Wire reference
//!
//! `BroadcastFeedMessage.message` is opaque in
//! [`sequencer_broadcast`] (typed as `serde_json::Value`); the canonical
//! shape from Nitro `arbstate/inbox.go` and
//! `arbstate/dasreader.go` is:
//!
//! ```text
//! MessageWithMetadata {
//!   message: L1IncomingMessage {
//!     header:   L1IncomingMessageHeader { kind: u8, .. },
//!     l2Msg:    []byte   (base64-encoded in JSON)
//!   },
//!   delayedMessagesRead: u64
//! }
//! ```
//!
//! and inside `l2Msg`:
//!
//! ```text
//! l2Msg = [L2MessageKind, ...payload...]
//!   L2MessageKind_UnsignedUserTx   = 0
//!   L2MessageKind_ContractTx       = 1
//!   L2MessageKind_NonmutatingCall  = 2
//!   L2MessageKind_Batch            = 3
//!   L2MessageKind_SignedTx         = 4    ← user-flow signed tx
//!   L2MessageKind_Heartbeat         = 5    ← deprecated/internal
//!   L2MessageKind_SignedCompressedTx = 6   ← fail-closed unless explicitly decoded
//! ```
//!
//! For `L2MessageKind_SignedTx` the remaining bytes are an EIP-2718
//! `TxEnvelope` and `alloy_consensus::TxEnvelope::decode_2718` decodes
//! it directly. Internal/non-user L2 message kinds are ignored. A
//! compressed signed-tx kind is surfaced as a distinct decode error so
//! a future Brotli implementation cannot be mistaken for live coverage.
//!
//! # Latency posture
//!
//! The public WS endpoint `wss://arb1.arbitrum.io/feed` has a ~150–300 ms
//! publish lag from the sequencer's internal commit. The "sub-10 ms"
//! claim in the underlying substack source assumes a private direct
//! subscription (own Nitro node or Chainstack relay). We surface this
//! via the structured `observed_lag_ms` log/metric per opportunity so
//! that the coordinator can fold it into the gas-price economics; we
//! never assert a budget the public endpoint can't meet.

use std::time::{SystemTime, UNIX_EPOCH};

use alloy::consensus::transaction::SignerRecoverable;
use alloy::consensus::{Transaction, TxEnvelope};
use alloy::eips::eip2718::{Decodable2718, Encodable2718};
use alloy::primitives::{Address, Bytes, B256};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use eyre::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, trace};

use crate::monitor::sequencer_broadcast::BroadcastFeedMessage;
use crate::monitor::Message;

// =============================================================================
// L2 message kinds (Nitro `arbstate/parse_l2.go`)
// =============================================================================

/// Canonical L1 incoming message type for an L2 message.
const L1_MESSAGE_TYPE_L2_MESSAGE: u64 = 3;

/// Unsigned user transaction. Internal/non-user flow for this analyzer.
pub const L2_MSG_KIND_UNSIGNED_USER_TX: u8 = 0;

/// L1-originated contract transaction. Internal/non-user flow here.
pub const L2_MSG_KIND_CONTRACT_TX: u8 = 1;

/// Nonmutating call. Internal/non-user flow here.
pub const L2_MSG_KIND_NONMUTATING_CALL: u8 = 2;

/// Single signed L2 transaction. Selector-extractable.
pub const L2_MSG_KIND_SIGNED_TX: u8 = 4;

/// Length-prefixed concatenation of multiple L2 messages — produced by
/// the sequencer for compression. The decoder walks the inner messages
/// recursively (see [`decode_l2_signed_txs`]).
pub const L2_MSG_KIND_BATCH: u8 = 3;

/// Deprecated/internal heartbeat.
pub const L2_MSG_KIND_HEARTBEAT: u8 = 5;

/// Brotli-compressed signed transaction frame. Recognized but not decoded.
pub const L2_MSG_KIND_SIGNED_COMPRESSED_TX: u8 = 6;

/// Max batch nesting depth, matching Nitro's `parseL2Message` guard.
const MAX_BATCH_DEPTH: u8 = 16;

/// Per-segment upper bound, matching Nitro's `MaxL2MessageSize`
/// (256 KiB) — the bound `BytestringFromReader` enforces on each
/// length-prefixed batch segment.
const MAX_L2_MESSAGE_SIZE: u64 = 256 * 1024;

// =============================================================================
// Selector allow-list — frontrun-target signatures
// =============================================================================

// 4-byte function selectors of "interesting" transactions worth a
// frontrun-candidate emission. Canonical signatures listed alongside
// each selector mirror `coordinator/src/submission/selectors.ts`.
//
// Selectors here MUST match the TS authority — drift = silent
// false-negatives on real opportunities. Verify each entry via
// `cast sig "<canonical>"` against the upstream ABI.

/// `swapExactTokensForTokens(uint256,uint256,address[],address,uint256)`
/// — UniswapV2Router02. Bread-and-butter constant-product victim.
pub const UNIV2_SWAP_EXACT_TOKENS_FOR_TOKENS: [u8; 4] = [0x38, 0xed, 0x17, 0x39];

/// `swapTokensForExactTokens(uint256,uint256,address[],address,uint256)`
/// — UniswapV2Router02 exact-out variant. Same victim class as
/// `swapExactTokensForTokens` from a sandwich's perspective (caller still
/// has a slippage envelope on the implied amountIn), just inverted.
pub const UNIV2_SWAP_TOKENS_FOR_EXACT_TOKENS: [u8; 4] = [0x88, 0x03, 0xdb, 0xee];

/// `swapExactTokensForTokensSupportingFeeOnTransferTokens(uint256,uint256,address[],address,uint256)`
/// — UniswapV2Router02 FoT-friendly variant. Used by FoT tokens
/// (rebasing / tax / reflection). Mechanics identical to
/// `swapExactTokensForTokens` for sandwich sizing.
pub const UNIV2_SWAP_EXACT_TOKENS_FOR_TOKENS_SUPPORTING_FOT: [u8; 4] = [0x5c, 0x11, 0xd7, 0x95];

/// `exactInputSingle((address,address,uint24,address,uint256,uint256,uint256,uint160))`
/// — UniswapV3 SwapRouter02 single-hop swap.
pub const UNIV3_EXACT_INPUT_SINGLE: [u8; 4] = [0x41, 0x4b, 0xf3, 0x89];

/// `exactInput((bytes,address,uint256,uint256,uint256))`
/// — UniswapV3 SwapRouter02 multi-hop swap.
pub const UNIV3_EXACT_INPUT: [u8; 4] = [0xc0, 0x4b, 0x8d, 0x59];

/// `execute(bytes,bytes[],uint256)` — Universal Router v2 entry point
/// (with deadline).
pub const UNIVERSAL_ROUTER_EXECUTE: [u8; 4] = [0x35, 0x93, 0x56, 0x4c];

/// `execute(bytes,bytes[])` — Universal Router v2 entry point (no
/// deadline). Both forms route the same underlying commands; the
/// no-deadline overload is used by callers that prefer to enforce
/// timing off-chain (e.g., bundle inclusion logic).
pub const UNIVERSAL_ROUTER_EXECUTE_NO_DEADLINE: [u8; 4] = [0x24, 0x85, 0x6b, 0xc3];

/// `swapExactTokensForTokensSupportingFeeOnTransferTokens(uint256,uint256,address[],address,address,uint256)`
/// — Camelot Router (Arbitrum-native UniV2 fork). Note the 6-arg
/// signature includes a `referrer` address absent from the canonical
/// UniV2 router. Selector differs from
/// [`UNIV2_SWAP_EXACT_TOKENS_FOR_TOKENS_SUPPORTING_FOT`] for that
/// reason. Verified against the live Camelot router at
/// `0xc873fEcbd354f5A56E00E710B90EF4201db2448d` (see CLAUDE.md
/// addresses skill).
pub const CAMELOT_SWAP_EXACT_TOKENS_FOR_TOKENS_SUPPORTING_FOT_REFERRER: [u8; 4] =
    [0xac, 0x38, 0x93, 0xba];

/// `exchange(int128,int128,uint256,uint256)` — Curve V1 stable-pool
/// swap. `int128` indices select coins; common path for stablecoin
/// victims (USDC.e ↔ USDT ↔ DAI).
pub const CURVE_V1_EXCHANGE: [u8; 4] = [0x3d, 0xf0, 0x21, 0x24];

/// `exchange(uint256,uint256,uint256,uint256)` — Curve V2 / crypto-pool
/// swap. Same semantic as V1 but uses `uint256` indices.
pub const CURVE_V2_EXCHANGE: [u8; 4] = [0x5b, 0x41, 0xb9, 0x08];

/// `swap((bytes32,uint8,address,address,uint256,bytes),(address,bool,address,bool),uint256,uint256)`
/// — Balancer V2 Vault single-swap. `bytes32 poolId` selects the
/// pool, `uint8 kind` selects GIVEN_IN / GIVEN_OUT.
pub const BALANCER_V2_SWAP: [u8; 4] = [0x52, 0xbb, 0xbe, 0x29];

/// `batchSwap(uint8,(bytes32,uint256,uint256,uint256,bytes)[],address[],(address,bool,address,bool),int256[],uint256)`
/// — Balancer V2 Vault multi-step batch swap. Common for aggregator
/// routes that touch Balancer V2.
pub const BALANCER_V2_BATCH_SWAP: [u8; 4] = [0x94, 0x5b, 0xce, 0xc9];

/// `swapSingleTokenExactIn(address,address,address,uint256,uint256,uint256,bool,bytes)`
/// — Balancer V3 Router single-swap exact-in. V3 is live on Arbitrum
/// (FlashProtocol slot 5 per ADR-027); victim class for routes that
/// settle through the new Router.
pub const BALANCER_V3_SWAP_SINGLE_EXACT_IN: [u8; 4] = [0x75, 0x02, 0x83, 0xbc];

/// `liquidationCall(address,address,address,uint256,bool)` — Aave V3 Pool.
pub const AAVE_V3_LIQUIDATION_CALL: [u8; 4] = [0x00, 0xa7, 0x18, 0xa9];

/// Allow-list table. The `&'static str` is the protocol/function label
/// surfaced on the outbound IPC envelope so the coordinator's
/// strategy dispatcher can route without rebuilding the selector map.
///
/// Note: the Ostium oracle-write selector is intentionally absent. Per
/// CLAUDE.md (P-1' Ostium oracle-gap path) and the
/// `jaredbot-mev-ostium-oracle-gap` skill, the on-chain write is a
/// Stork-style pull update — we have not yet confirmed the
/// 4-byte selector against the deployed contract. The off-chain
/// `coordinator/src/feeds/ostium.ts` detector uses event topic hashes
/// (which are placeholders too) rather than tx selectors, so there's
/// no usable cross-reference yet. We match Ostium by destination
/// address instead — see [`is_ostium_destination`].
pub const FRONTRUN_SELECTORS: &[([u8; 4], &str)] = &[
    (
        UNIV2_SWAP_EXACT_TOKENS_FOR_TOKENS,
        "UniV2.swapExactTokensForTokens",
    ),
    (
        UNIV2_SWAP_TOKENS_FOR_EXACT_TOKENS,
        "UniV2.swapTokensForExactTokens",
    ),
    (
        UNIV2_SWAP_EXACT_TOKENS_FOR_TOKENS_SUPPORTING_FOT,
        "UniV2.swapExactTokensForTokensSupportingFeeOnTransferTokens",
    ),
    (UNIV3_EXACT_INPUT_SINGLE, "UniV3.exactInputSingle"),
    (UNIV3_EXACT_INPUT, "UniV3.exactInput"),
    (UNIVERSAL_ROUTER_EXECUTE, "UniversalRouter.execute"),
    (
        UNIVERSAL_ROUTER_EXECUTE_NO_DEADLINE,
        "UniversalRouter.execute(no-deadline)",
    ),
    (
        CAMELOT_SWAP_EXACT_TOKENS_FOR_TOKENS_SUPPORTING_FOT_REFERRER,
        "Camelot.swapExactTokensForTokensSupportingFeeOnTransferTokens",
    ),
    (CURVE_V1_EXCHANGE, "Curve.exchange(int128)"),
    (CURVE_V2_EXCHANGE, "Curve.exchange(uint256)"),
    (BALANCER_V2_SWAP, "BalancerV2.swap"),
    (BALANCER_V2_BATCH_SWAP, "BalancerV2.batchSwap"),
    (
        BALANCER_V3_SWAP_SINGLE_EXACT_IN,
        "BalancerV3.swapSingleTokenExactIn",
    ),
    (AAVE_V3_LIQUIDATION_CALL, "AaveV3.liquidationCall"),
];

/// Address-based fallback for protocols where we can't yet confirm the
/// 4-byte selector. Configurable so deploy-time addresses (e.g., the
/// Ostium oracle proxy at the as-yet-TBD address) can override the
/// build-time defaults without a recompile. None of these are set by
/// default — `with_address_targets` populates them.
#[derive(Debug, Clone, Default)]
pub struct AddressTargets {
    /// Destinations matched by `to` address regardless of selector.
    /// Pair: (address, label-on-the-wire).
    pub address_match: Vec<(Address, &'static str)>,
}

// =============================================================================
// IPC payload — kept here so all the wire-relevant context for the
// FrontrunCandidate variant lives next to the producer. The variant
// itself lives on `crate::Message` in lib.rs for symmetry with
// Opportunity / PoolUpdate / Heartbeat / Error.
// =============================================================================

/// Payload of `Message::FrontrunCandidate`.
///
/// All fields are wire-stable; if you rename one, also update
/// `coordinator/src/ipc/types.ts WireFrontrunCandidate`. The
/// `serde(rename_all)` is intentionally omitted — Rust's default
/// snake_case matches the TS wire mirror.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FrontrunCandidate {
    /// 4-byte function selector that matched the allow-list.
    pub selector: [u8; 4],

    /// Human-readable label for the selector (one of the labels in
    /// [`FRONTRUN_SELECTORS`] or `"address_match:<label>"` for fallback
    /// matches).
    pub target_label: String,

    /// Tx destination. `None` for contract-creation, which we never
    /// match (creation has no selector).
    pub victim: Option<Address>,

    /// EIP-2718 transaction hash — the coordinator's idempotency key
    /// for the sequencer-feed-tx signal.
    pub tx_hash: B256,

    /// Recovered signer (the `from` address). The envelope is decoded
    /// from a signed EIP-2718 frame, so recovery always succeeds for a
    /// candidate that reaches the wire.
    pub from: Address,

    /// Raw EIP-2718-encoded transaction bytes — lets the coordinator
    /// re-broadcast or re-decode without a round-trip to the engine.
    pub raw_tx: Bytes,

    /// Full calldata. The strategy layer can re-decode args without us
    /// understanding every protocol.
    pub calldata: Bytes,

    /// Wei value transferred by the tx.
    pub value_wei: u128,

    /// Gas limit the tx was signed with.
    pub gas_limit: u64,

    /// Gas price the tx was signed with — for legacy/EIP-2930 the
    /// pre-1559 `gasPrice`; for EIP-1559 the `maxFeePerGas`. Used by the
    /// coordinator's bid-economics module to set our own gas envelope.
    pub gas_price_wei: u128,

    /// EIP-1559 priority fee (`maxPriorityFeePerGas`). Equals
    /// `gas_price_wei` for legacy/EIP-2930 txs, which have no separate
    /// priority component.
    pub max_priority_fee_per_gas_wei: u128,

    /// Wall-clock unix-ms when this engine process first observed the
    /// envelope on the feed. Surfaces the public-feed lag explicitly so
    /// the coordinator can compare against `block.timestamp` once the
    /// tx lands and decide whether we still have a reaction window.
    pub observed_at_ms: u64,

    /// Sequencer-assigned L2 message index. Carried through so the
    /// coordinator can deduplicate when the same envelope is observed
    /// on multiple feed connections (failover, redundancy).
    pub sequence_number: u64,
}

// =============================================================================
// Decoder — `l2Msg` (base64 JSON) → `TxEnvelope`
// =============================================================================

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("inner `message` wrapper missing `l2Msg` field")]
    MissingL2Msg,
    #[error("`l2Msg` field is not a JSON string")]
    L2MsgNotString,
    #[error("`l2Msg` base64 decode failed: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("`l2Msg` is empty (no L2 kind byte)")]
    Empty,
    #[error("unsupported L2 message kind: {0}")]
    UnsupportedKind(u8),
    #[error("EIP-2718 envelope decode failed: {0}")]
    Eip2718(String),
    #[error("batch nesting exceeds max depth {MAX_BATCH_DEPTH}")]
    BatchTooDeep,
    #[error("batch segment length {0} exceeds MaxL2MessageSize")]
    BatchSegmentTooLarge(u64),
    #[error("incoming L1 message kind {0} is not L2Message kind {L1_MESSAGE_TYPE_L2_MESSAGE}")]
    NonL2MessageKind(u64),
    #[error("signed compressed L2 transaction frames are recognized but not decoded")]
    SignedCompressedTxUnsupported,
}

fn has_l2_msg(message: &serde_json::Value) -> bool {
    message
        .as_object()
        .map(|obj| obj.contains_key("l2Msg") || obj.contains_key("l2msg"))
        .unwrap_or(false)
}

/// Return the nested Nitro `L1IncomingMessage` object. Live
/// `BroadcastFeedMessage.message` values are `MessageWithMetadata`
/// wrappers (`{ message: { header, l2Msg }, delayedMessagesRead }`),
/// while older local fixtures pass the inner object directly.
fn l1_incoming_message(message: &serde_json::Value) -> Result<&serde_json::Value, DecodeError> {
    if has_l2_msg(message) {
        return Ok(message);
    }

    let obj = message.as_object().ok_or(DecodeError::MissingL2Msg)?;
    let nested = obj.get("message").ok_or(DecodeError::MissingL2Msg)?;
    if has_l2_msg(nested) {
        Ok(nested)
    } else {
        Err(DecodeError::MissingL2Msg)
    }
}

fn assert_l1_message_kind(message: &serde_json::Value) -> Result<(), DecodeError> {
    let Some(kind) = message
        .get("header")
        .and_then(|header| header.get("kind"))
        .and_then(serde_json::Value::as_u64)
    else {
        return Ok(());
    };

    if kind == L1_MESSAGE_TYPE_L2_MESSAGE {
        Ok(())
    } else {
        Err(DecodeError::NonL2MessageKind(kind))
    }
}

/// Pull the inner `l2Msg` field out of a `BroadcastFeedMessage.message`
/// JSON `Value`. We accept the live Nitro wrapper shape plus the direct
/// inner shape used by historical fixtures. We also accept either the
/// canonical Nitro tag (`l2Msg`) or lowercase form (`l2msg`) since Go's
/// `encoding/json` honours the struct tag when present and lowercases the
/// field name otherwise — both forms have been seen in fixtures from
/// different Nitro release branches.
fn extract_l2_msg_b64(message: &serde_json::Value) -> Result<&str, DecodeError> {
    let message = l1_incoming_message(message)?;
    assert_l1_message_kind(message)?;
    let obj = message.as_object().ok_or(DecodeError::MissingL2Msg)?;
    let val = obj
        .get("l2Msg")
        .or_else(|| obj.get("l2msg"))
        .ok_or(DecodeError::MissingL2Msg)?;
    val.as_str().ok_or(DecodeError::L2MsgNotString)
}

/// Recursively decode one L2 message (`[kind byte][payload]`) into zero
/// or more signed-tx envelopes, appending to `out`.
///
/// `SignedTx` contributes exactly one envelope. `Batch` is walked
/// depth-first: its payload is a sequence of segments, each a
/// big-endian `u64` length prefix followed by that many bytes of inner
/// L2 message. A truncated trailing segment (too few bytes for the
/// length prefix or the body) marks the end of the batch — matching
/// Nitro's `parseL2Message`, where a short read means "no further
/// messages" rather than an error. Any other kind is an error, as in
/// the pre-batch decoder.
fn decode_l2_message_bytes(
    bytes: &[u8],
    depth: u8,
    out: &mut Vec<TxEnvelope>,
) -> Result<(), DecodeError> {
    let Some((&kind, payload)) = bytes.split_first() else {
        return Err(DecodeError::Empty);
    };

    match kind {
        L2_MSG_KIND_SIGNED_TX => {
            let mut cursor: &[u8] = payload;
            let envelope = TxEnvelope::decode_2718(&mut cursor)
                .map_err(|e| DecodeError::Eip2718(format!("{e}")))?;
            out.push(envelope);
            Ok(())
        }
        L2_MSG_KIND_BATCH => {
            if depth >= MAX_BATCH_DEPTH {
                return Err(DecodeError::BatchTooDeep);
            }
            let mut rest = payload;
            loop {
                // A segment is `u64 BE length` ++ `length bytes`. Too
                // few bytes for either half means the batch is done.
                if rest.len() < 8 {
                    return Ok(());
                }
                let (len_bytes, after_len) = rest.split_at(8);
                let mut len_bytes_array = [0u8; 8];
                len_bytes_array.copy_from_slice(len_bytes);
                let seg_len = u64::from_be_bytes(len_bytes_array);
                if seg_len > MAX_L2_MESSAGE_SIZE {
                    return Err(DecodeError::BatchSegmentTooLarge(seg_len));
                }
                let seg_len = seg_len as usize;
                if after_len.len() < seg_len {
                    return Ok(());
                }
                let (segment, remaining) = after_len.split_at(seg_len);
                decode_l2_message_bytes(segment, depth + 1, out)?;
                rest = remaining;
            }
        }
        L2_MSG_KIND_UNSIGNED_USER_TX
        | L2_MSG_KIND_CONTRACT_TX
        | L2_MSG_KIND_NONMUTATING_CALL
        | L2_MSG_KIND_HEARTBEAT => Ok(()),
        L2_MSG_KIND_SIGNED_COMPRESSED_TX => Err(DecodeError::SignedCompressedTxUnsupported),
        other => Err(DecodeError::UnsupportedKind(other)),
    }
}

/// Decode a `BroadcastFeedMessage`'s inner `l2Msg` to zero or more
/// [`TxEnvelope`]s. A `SignedTx` frame yields one; a `Batch` frame
/// yields one per inner signed tx (recursively); a non-signed,
/// non-batch frame yields an empty vec via [`DecodeError::UnsupportedKind`]
/// surfaced to the caller.
///
/// `Err(_)` is reserved for structural failures: malformed JSON, base64
/// decode failure, EIP-2718 RLP errors, unsupported kinds, or a
/// malformed batch. The caller treats both an empty vec and `Err(_)` as
/// non-match — `Err` is logged for observability so we can spot a feed
/// regression early.
pub fn decode_l2_signed_txs(message: &serde_json::Value) -> Result<Vec<TxEnvelope>, DecodeError> {
    let b64 = extract_l2_msg_b64(message)?;
    let bytes = STANDARD.decode(b64)?;
    let mut out = Vec::new();
    decode_l2_message_bytes(&bytes, 0, &mut out)?;
    Ok(out)
}

// =============================================================================
// Selector matching
// =============================================================================

/// Match a calldata prefix against the [`FRONTRUN_SELECTORS`] table.
/// Returns the label of the matching entry, or `None` if the selector
/// isn't a frontrun target. Calldata shorter than 4 bytes never
/// matches (no selector to inspect).
pub fn match_selector(calldata: &[u8]) -> Option<&'static str> {
    let bytes: [u8; 4] = calldata.first_chunk::<4>().copied()?;
    FRONTRUN_SELECTORS
        .iter()
        .find_map(|(sel, label)| (*sel == bytes).then_some(*label))
}

/// Match `to` against the configured address allow-list (e.g., Ostium
/// oracle proxy). Returns the label of the matching entry.
pub fn match_address(addr: &Address, targets: &AddressTargets) -> Option<&'static str> {
    targets
        .address_match
        .iter()
        .find_map(|(a, label)| (a == addr).then_some(*label))
}

/// Convenience predicate — true if `to` is the configured Ostium oracle
/// destination. Retained for symmetry with the off-chain Ostium feed
/// detector at `coordinator/src/feeds/ostium.ts`.
pub fn is_ostium_destination(addr: &Address, targets: &AddressTargets) -> bool {
    targets
        .address_match
        .iter()
        .any(|(a, label)| a == addr && label.starts_with("Ostium"))
}

// =============================================================================
// Dispatch loop
// =============================================================================

/// Configuration knobs for the analysis loop. Held separate from
/// transport config so the two are independently testable.
#[derive(Debug, Clone, Default)]
pub struct AnalysisConfig {
    pub address_targets: AddressTargets,
}

/// Consume `BroadcastFeedMessage`s from the subscriber's mpsc, decode
/// the inner L2 signed tx, match against the allow-list, and emit
/// `Message::FrontrunCandidate` envelopes on the engine→coordinator
/// broadcast bus.
///
/// Loops until the inbound mpsc is closed (subscriber dropped). The
/// outbound `broadcast::Sender` is shared with the rest of the engine —
/// dropped subscribers are normal (no panic).
pub async fn run_analysis(
    mut rx: mpsc::Receiver<BroadcastFeedMessage>,
    outbound_tx: broadcast::Sender<Message>,
    cfg: AnalysisConfig,
) -> Result<()> {
    info!(
        target: "monitor::feed",
        selectors = FRONTRUN_SELECTORS.len(),
        address_targets = cfg.address_targets.address_match.len(),
        "sequencer-feed analyser starting"
    );

    while let Some(msg) = rx.recv().await {
        for candidate in analyse_one(&msg, &cfg) {
            // Structured log carries the same fields the coordinator
            // will see on the wire — keeps a local audit trail even if
            // no coordinator is connected.
            tracing::info!(
                target: "monitor::feed",
                selector = %hex_selector(&candidate.selector),
                target_label = %candidate.target_label,
                victim = ?candidate.victim,
                gas_price_wei = candidate.gas_price_wei,
                observed_at_ms = candidate.observed_at_ms,
                sequence_number = candidate.sequence_number,
                "frontrun_candidate"
            );
            crate::utils::metrics::inc_frontrun_candidates_emitted();

            if let Err(err) = outbound_tx.send(Message::FrontrunCandidate(candidate)) {
                // Common case during early boot: no IPC connections
                // yet. Trace, don't warn.
                trace!(
                    target: "monitor::feed",
                    err = ?err,
                    "no subscribers for frontrun candidate"
                );
            }
        }
    }

    info!(
        target: "monitor::feed",
        "sequencer-feed analyser exiting (mpsc closed)"
    );
    Ok(())
}

/// Stateless single-message analysis. Extracted so unit tests can feed
/// hand-built fixtures without the mpsc / broadcast plumbing.
///
/// Returns one [`FrontrunCandidate`] per matching signed tx. A plain
/// `SignedTx` frame yields at most one; a `Batch` frame may yield
/// several (or none). Order follows the batch's depth-first walk.
pub fn analyse_one(msg: &BroadcastFeedMessage, cfg: &AnalysisConfig) -> Vec<FrontrunCandidate> {
    let envelopes = match decode_l2_signed_txs(&msg.message) {
        Ok(envs) => envs,
        Err(err) => {
            debug!(
                target: "monitor::feed",
                err = ?err,
                seq = msg.sequence_number,
                "l2 decode failed; skipping"
            );
            return Vec::new();
        }
    };

    envelopes
        .iter()
        .filter_map(|env| candidate_from_envelope(env, msg, cfg))
        .collect()
}

/// Build a [`FrontrunCandidate`] from a single decoded envelope, or
/// `None` when the tx is not a frontrun target.
fn candidate_from_envelope(
    envelope: &TxEnvelope,
    msg: &BroadcastFeedMessage,
    cfg: &AnalysisConfig,
) -> Option<FrontrunCandidate> {
    let calldata: &Bytes = envelope.input();
    let victim = envelope.to();

    // Selector match takes priority; if absent, fall back to address.
    // A contract-creation tx (no `to`) or an unmatched address yields no
    // candidate — `?` propagates the `None`.
    let label_owned: String = match match_selector(calldata.as_ref()) {
        Some(label) => label.to_string(),
        None => {
            let addr = victim.as_ref()?;
            let label = match_address(addr, &cfg.address_targets)?;
            format!("address_match:{label}")
        }
    };

    // Selector bytes — `match_selector` already verified there are >=4,
    // but the address-fallback path may have a shorter calldata. Pad
    // missing prefix bytes with zero so the wire field is always 4
    // bytes wide. (This keeps the schema stable; the coordinator
    // distinguishes selector-match vs address-match via `target_label`,
    // not via the bytes.)
    let selector: [u8; 4] = match calldata.as_ref().first_chunk::<4>() {
        Some(arr) => *arr,
        None => [0u8; 4],
    };

    // Recover the signer. The envelope decoded from a signed EIP-2718
    // frame, so this only fails on a corrupt signature — treat that as
    // a non-match rather than emitting a candidate with no `from`.
    let from = match envelope.recover_signer() {
        Ok(addr) => addr,
        Err(err) => {
            debug!(
                target: "monitor::feed",
                err = ?err,
                seq = msg.sequence_number,
                "signer recovery failed; skipping"
            );
            return None;
        }
    };

    let gas_price_wei = envelope
        .gas_price()
        .unwrap_or_else(|| envelope.max_fee_per_gas());

    Some(FrontrunCandidate {
        selector,
        target_label: label_owned,
        victim,
        tx_hash: *envelope.tx_hash(),
        from,
        raw_tx: Bytes::from(envelope.encoded_2718()),
        calldata: calldata.clone(),
        value_wei: envelope.value().saturating_to::<u128>(),
        gas_limit: envelope.gas_limit(),
        gas_price_wei,
        // Legacy/EIP-2930 txs have no separate priority component —
        // the full gas price is effectively the priority fee.
        max_priority_fee_per_gas_wei: envelope.max_priority_fee_per_gas().unwrap_or(gas_price_wei),
        observed_at_ms: now_unix_ms(),
        sequence_number: msg.sequence_number,
    })
}

fn hex_selector(b: &[u8; 4]) -> String {
    // `format!("{:02x}{:02x}{:02x}{:02x}", b[0], b[1], b[2], b[3])` is
    // simpler than pulling in another hex crate.
    format!("0x{:02x}{:02x}{:02x}{:02x}", b[0], b[1], b[2], b[3])
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// =============================================================================
// Tests — synthetic L2 envelopes, selector dispatch, IPC variant
// round-trip.
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::consensus::{SignableTransaction, TxEip1559};
    use alloy::primitives::{address, b256, hex, Bytes, ChainId, Signature, U256};
    use alloy::signers::local::PrivateKeySigner;
    use alloy::signers::SignerSync;

    const TARGET: Address = address!("E592427A0AEce92De3Edee1F18E0157C05861564");
    // Some valid 32-byte signing key bytes; deterministic for repro.
    const SIGNER_HEX: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

    fn deterministic_signer() -> PrivateKeySigner {
        let bytes = hex::decode(SIGNER_HEX).expect("hex");
        PrivateKeySigner::from_slice(&bytes).expect("signer")
    }

    fn build_signed_eip1559(
        chain_id: ChainId,
        to: Address,
        input: Vec<u8>,
        max_fee_per_gas: u128,
    ) -> TxEnvelope {
        let tx = TxEip1559 {
            chain_id,
            nonce: 17,
            gas_limit: 100_000,
            max_fee_per_gas,
            max_priority_fee_per_gas: 1_000_000_000,
            to: alloy::primitives::TxKind::Call(to),
            value: U256::ZERO,
            access_list: Default::default(),
            input: input.into(),
        };
        let signer = deterministic_signer();
        let sig_hash = tx.signature_hash();
        let sig: Signature = signer.sign_hash_sync(&sig_hash).expect("sign");
        tx.into_signed(sig).into()
    }

    fn wrap_l2_signed_tx(envelope: &TxEnvelope) -> serde_json::Value {
        // Build the same JSON shape `BroadcastFeedMessage.message`
        // uses after the analyzer peels the live `MessageWithMetadata`
        // wrapper: the inner `message` (= L1IncomingMessage) has `header`
        // + `l2Msg`. `l2Msg` = [L2MessageKind || EIP-2718].
        let mut payload = Vec::with_capacity(256);
        payload.push(L2_MSG_KIND_SIGNED_TX);
        envelope.encode_2718(&mut payload);
        let b64 = STANDARD.encode(&payload);
        serde_json::json!({
            "header": { "kind": 3, "poster": "0x0000000000000000000000000000000000000000" },
            "l2Msg": b64,
        })
    }

    fn wrap_message_with_metadata_l2_bytes(
        bytes: &[u8],
        l1_block_number: u64,
    ) -> serde_json::Value {
        serde_json::json!({
            "message": {
                "header": {
                    "kind": L1_MESSAGE_TYPE_L2_MESSAGE,
                    "sender": "0xa4b000000000000000000073657175656e636572",
                    "blockNumber": l1_block_number,
                    "timestamp": 1_700_000_000_u64,
                    "requestId": null,
                    "baseFeeL1": null
                },
                "l2Msg": STANDARD.encode(bytes)
            },
            "delayedMessagesRead": 354560_u64
        })
    }

    fn make_broadcast_msg(envelope: &TxEnvelope, seq: u64) -> BroadcastFeedMessage {
        BroadcastFeedMessage {
            sequence_number: seq,
            message: wrap_l2_signed_tx(envelope),
            block_hash: None,
            signature: vec![0; 65],
            block_metadata: None,
        }
    }

    /// Inner L2-message bytes for a signed tx: `[kind][EIP-2718]`.
    fn signed_tx_msg_bytes(envelope: &TxEnvelope) -> Vec<u8> {
        let mut b = vec![L2_MSG_KIND_SIGNED_TX];
        envelope.encode_2718(&mut b);
        b
    }

    /// Wrap inner L2-message blobs in a batch frame: `[BATCH]` followed
    /// by `(u64 BE length ++ blob)` per segment.
    fn batch_msg_bytes(inner_msgs: &[Vec<u8>]) -> Vec<u8> {
        let mut payload = vec![L2_MSG_KIND_BATCH];
        for m in inner_msgs {
            payload.extend_from_slice(&(m.len() as u64).to_be_bytes());
            payload.extend_from_slice(m);
        }
        payload
    }

    /// Base64-wrap raw L2-message bytes into a `BroadcastFeedMessage.message`.
    fn wrap_l2_bytes(bytes: &[u8]) -> serde_json::Value {
        serde_json::json!({ "l2Msg": STANDARD.encode(bytes) })
    }

    fn make_broadcast_msg_raw(message: serde_json::Value, seq: u64) -> BroadcastFeedMessage {
        BroadcastFeedMessage {
            sequence_number: seq,
            message,
            block_hash: None,
            signature: vec![0; 65],
            block_metadata: None,
        }
    }

    // ----- selector allow-list integrity ------------------------------------

    #[test]
    fn selector_table_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for (sel, label) in FRONTRUN_SELECTORS {
            assert!(seen.insert(*sel), "duplicate selector in table: {label}");
        }
    }

    /// Drift insurance: every hardcoded selector in [`FRONTRUN_SELECTORS`]
    /// MUST equal `keccak256(canonical_signature)[0..4]`. If you change the
    /// canonical signature column without updating the byte literal (or
    /// vice-versa), this catches it before a silent false-negative ships.
    ///
    /// The canonical signatures here MUST exactly match the doc-comment on
    /// each `pub const SELECTOR: [u8; 4]` above — i.e. the canonical
    /// signature string is the source-of-truth for both the const and the
    /// runtime check.
    #[test]
    fn selector_bytes_match_keccak_of_canonical_signature() {
        use alloy::primitives::keccak256;

        fn sel(sig: &str) -> [u8; 4] {
            let hash = keccak256(sig.as_bytes());
            [hash[0], hash[1], hash[2], hash[3]]
        }

        let cases: &[(&str, [u8; 4])] = &[
            (
                "swapExactTokensForTokens(uint256,uint256,address[],address,uint256)",
                UNIV2_SWAP_EXACT_TOKENS_FOR_TOKENS,
            ),
            (
                "swapTokensForExactTokens(uint256,uint256,address[],address,uint256)",
                UNIV2_SWAP_TOKENS_FOR_EXACT_TOKENS,
            ),
            (
                "swapExactTokensForTokensSupportingFeeOnTransferTokens(uint256,uint256,address[],address,uint256)",
                UNIV2_SWAP_EXACT_TOKENS_FOR_TOKENS_SUPPORTING_FOT,
            ),
            (
                "exactInputSingle((address,address,uint24,address,uint256,uint256,uint256,uint160))",
                UNIV3_EXACT_INPUT_SINGLE,
            ),
            (
                "exactInput((bytes,address,uint256,uint256,uint256))",
                UNIV3_EXACT_INPUT,
            ),
            ("execute(bytes,bytes[],uint256)", UNIVERSAL_ROUTER_EXECUTE),
            (
                "execute(bytes,bytes[])",
                UNIVERSAL_ROUTER_EXECUTE_NO_DEADLINE,
            ),
            (
                "swapExactTokensForTokensSupportingFeeOnTransferTokens(uint256,uint256,address[],address,address,uint256)",
                CAMELOT_SWAP_EXACT_TOKENS_FOR_TOKENS_SUPPORTING_FOT_REFERRER,
            ),
            ("exchange(int128,int128,uint256,uint256)", CURVE_V1_EXCHANGE),
            (
                "exchange(uint256,uint256,uint256,uint256)",
                CURVE_V2_EXCHANGE,
            ),
            (
                "swap((bytes32,uint8,address,address,uint256,bytes),(address,bool,address,bool),uint256,uint256)",
                BALANCER_V2_SWAP,
            ),
            (
                "batchSwap(uint8,(bytes32,uint256,uint256,uint256,bytes)[],address[],(address,bool,address,bool),int256[],uint256)",
                BALANCER_V2_BATCH_SWAP,
            ),
            (
                "swapSingleTokenExactIn(address,address,address,uint256,uint256,uint256,bool,bytes)",
                BALANCER_V3_SWAP_SINGLE_EXACT_IN,
            ),
            (
                "liquidationCall(address,address,address,uint256,bool)",
                AAVE_V3_LIQUIDATION_CALL,
            ),
        ];

        for (sig, expected) in cases {
            assert_eq!(
                sel(sig),
                *expected,
                "selector drift for canonical sig `{sig}`: computed {:?}, constant {:?}",
                sel(sig),
                expected,
            );
        }
    }

    #[test]
    fn match_selector_returns_label_for_known_selector() {
        let calldata = [
            0x41, 0x4b, 0xf3, 0x89, 0xff, 0xff, // exactInputSingle + filler
        ];
        assert_eq!(match_selector(&calldata), Some("UniV3.exactInputSingle"));
    }

    #[test]
    fn match_selector_returns_none_for_unknown_selector() {
        let calldata = [0xde, 0xad, 0xbe, 0xef, 0x00];
        assert_eq!(match_selector(&calldata), None);
    }

    #[test]
    fn match_selector_returns_none_for_short_calldata() {
        assert_eq!(match_selector(&[]), None);
        assert_eq!(match_selector(&[0x41, 0x4b, 0xf3]), None);
    }

    // ----- decoder: positive path -------------------------------------------

    #[test]
    fn decode_l2_signed_txs_extracts_envelope() {
        let env = build_signed_eip1559(
            42161,
            TARGET,
            vec![0x41, 0x4b, 0xf3, 0x89, 0x00, 0x01, 0x02, 0x03],
            5_000_000_000,
        );
        let wrapper = wrap_l2_signed_tx(&env);
        let decoded = decode_l2_signed_txs(&wrapper).expect("decode ok");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].to(), Some(TARGET));
        assert_eq!(
            decoded[0].input().as_ref(),
            &[0x41, 0x4b, 0xf3, 0x89, 0x00, 0x01, 0x02, 0x03]
        );
    }

    #[test]
    fn decode_l2_signed_txs_accepts_lowercase_l2msg_field() {
        let env = build_signed_eip1559(42161, TARGET, vec![0x00, 0x00, 0x00, 0x00], 1_000_000_000);
        let mut payload = Vec::with_capacity(128);
        payload.push(L2_MSG_KIND_SIGNED_TX);
        env.encode_2718(&mut payload);
        let wrapper = serde_json::json!({ "l2msg": STANDARD.encode(&payload) });
        let decoded = decode_l2_signed_txs(&wrapper).expect("decode ok");
        assert_eq!(decoded.len(), 1);
    }

    #[test]
    fn decode_l2_signed_txs_accepts_live_message_with_metadata_wrapper() {
        let env = build_signed_eip1559(
            42161,
            TARGET,
            vec![0x41, 0x4b, 0xf3, 0x89, 0x00, 0x01, 0x02, 0x03],
            5_000_000_000,
        );
        let bytes = signed_tx_msg_bytes(&env);
        let wrapper = wrap_message_with_metadata_l2_bytes(&bytes, 16_238_523);
        let decoded = decode_l2_signed_txs(&wrapper).expect("decode ok");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].to(), Some(TARGET));
        assert_eq!(
            decoded[0].input().as_ref(),
            &[0x41, 0x4b, 0xf3, 0x89, 0x00, 0x01, 0x02, 0x03]
        );
    }

    // ----- decoder: batch walk ----------------------------------------------

    #[test]
    fn decode_l2_signed_txs_walks_batch_of_signed_txs() {
        let env_a = build_signed_eip1559(42161, TARGET, vec![0xaa; 8], 1_000_000_000);
        let env_b = build_signed_eip1559(42161, TARGET, vec![0xbb; 8], 2_000_000_000);
        let batch = batch_msg_bytes(&[signed_tx_msg_bytes(&env_a), signed_tx_msg_bytes(&env_b)]);
        let decoded = decode_l2_signed_txs(&wrap_l2_bytes(&batch)).expect("decode ok");
        assert_eq!(decoded.len(), 2, "both batched txs should decode");
        assert_eq!(decoded[0].input().as_ref(), &[0xaa; 8]);
        assert_eq!(decoded[1].input().as_ref(), &[0xbb; 8]);
    }

    #[test]
    fn decode_l2_signed_txs_walks_nested_batch() {
        let env = build_signed_eip1559(42161, TARGET, vec![0xcc; 8], 1_000_000_000);
        let inner_batch = batch_msg_bytes(&[signed_tx_msg_bytes(&env)]);
        let outer_batch = batch_msg_bytes(&[inner_batch]);
        let decoded = decode_l2_signed_txs(&wrap_l2_bytes(&outer_batch)).expect("decode ok");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].input().as_ref(), &[0xcc; 8]);
    }

    #[test]
    fn decode_l2_signed_txs_empty_for_truncated_batch() {
        // Payload `0x00 0x01` is fewer than the 8 bytes a segment length
        // prefix needs — the walk treats this as end-of-batch.
        let wrapper = wrap_l2_bytes(&[L2_MSG_KIND_BATCH, 0x00, 0x01]);
        let decoded = decode_l2_signed_txs(&wrapper).expect("decode ok");
        assert!(decoded.is_empty(), "truncated batch yields no envelopes");
    }

    #[test]
    fn decode_l2_signed_txs_errors_on_batch_too_deep() {
        let env = build_signed_eip1559(42161, TARGET, vec![0xdd; 8], 1_000_000_000);
        // 17 nested batch layers: the 17th is parsed at depth 16 and trips
        // the guard. 16 layers would still succeed.
        let mut msg = signed_tx_msg_bytes(&env);
        for _ in 0..17 {
            msg = batch_msg_bytes(&[msg]);
        }
        assert!(matches!(
            decode_l2_signed_txs(&wrap_l2_bytes(&msg)),
            Err(DecodeError::BatchTooDeep)
        ));
    }

    // ----- decoder: negative paths ------------------------------------------

    #[test]
    fn decode_l2_signed_txs_errors_on_missing_field() {
        let wrapper = serde_json::json!({ "wrong_field": "irrelevant" });
        assert!(matches!(
            decode_l2_signed_txs(&wrapper),
            Err(DecodeError::MissingL2Msg)
        ));
    }

    #[test]
    fn decode_l2_signed_txs_errors_on_invalid_base64() {
        let wrapper = serde_json::json!({ "l2Msg": "!!! not base64 !!!" });
        assert!(matches!(
            decode_l2_signed_txs(&wrapper),
            Err(DecodeError::Base64(_))
        ));
    }

    #[test]
    fn decode_l2_signed_txs_errors_on_empty_payload() {
        let wrapper = serde_json::json!({ "l2Msg": "" });
        assert!(matches!(
            decode_l2_signed_txs(&wrapper),
            Err(DecodeError::Empty)
        ));
    }

    #[test]
    fn decode_l2_signed_txs_errors_on_unknown_kind() {
        let wrapper = serde_json::json!({ "l2Msg": STANDARD.encode([99u8, 0x01, 0x02]) });
        match decode_l2_signed_txs(&wrapper) {
            Err(DecodeError::UnsupportedKind(99)) => {}
            other => panic!("expected UnsupportedKind(99), got {other:?}"),
        }
    }

    #[test]
    fn decode_l2_signed_txs_ignores_internal_l2_message_kinds() {
        for kind in [
            L2_MSG_KIND_UNSIGNED_USER_TX,
            L2_MSG_KIND_CONTRACT_TX,
            L2_MSG_KIND_NONMUTATING_CALL,
            L2_MSG_KIND_HEARTBEAT,
        ] {
            let wrapper = wrap_l2_bytes(&[kind, 0xde, 0xad, 0xbe, 0xef]);
            let decoded = decode_l2_signed_txs(&wrapper).expect("decode ok");
            assert!(
                decoded.is_empty(),
                "kind {kind} should not produce candidates"
            );
        }
    }

    #[test]
    fn decode_l2_signed_txs_errors_on_signed_compressed_tx_kind() {
        let wrapper = wrap_l2_bytes(&[L2_MSG_KIND_SIGNED_COMPRESSED_TX, 0xde, 0xad]);
        assert!(matches!(
            decode_l2_signed_txs(&wrapper),
            Err(DecodeError::SignedCompressedTxUnsupported)
        ));
    }

    #[test]
    fn decode_l2_signed_txs_errors_on_non_l2_incoming_message_kind() {
        let env = build_signed_eip1559(42161, TARGET, vec![0x41, 0x4b, 0xf3, 0x89], 1_000_000_000);
        let bytes = signed_tx_msg_bytes(&env);
        let wrapper = serde_json::json!({
            "message": {
                "header": { "kind": 12, "blockNumber": 16_238_523_u64 },
                "l2Msg": STANDARD.encode(bytes)
            },
            "delayedMessagesRead": 354560_u64
        });
        assert!(matches!(
            decode_l2_signed_txs(&wrapper),
            Err(DecodeError::NonL2MessageKind(12))
        ));
    }

    // ----- analyse_one: end-to-end synthetic --------------------------------

    #[test]
    fn analyse_one_emits_for_known_selector() {
        let mut calldata = Vec::with_capacity(4 + 32);
        calldata.extend_from_slice(&UNIV3_EXACT_INPUT_SINGLE);
        calldata.extend_from_slice(&[0u8; 32]);
        let env = build_signed_eip1559(42161, TARGET, calldata.clone(), 2_500_000_000);
        let msg = make_broadcast_msg(&env, 42);
        let cfg = AnalysisConfig::default();
        let candidates = analyse_one(&msg, &cfg);
        assert_eq!(candidates.len(), 1, "should match exactly one");
        let candidate = &candidates[0];
        assert_eq!(candidate.selector, UNIV3_EXACT_INPUT_SINGLE);
        assert_eq!(candidate.target_label, "UniV3.exactInputSingle");
        assert_eq!(candidate.victim, Some(TARGET));
        assert_eq!(candidate.calldata.as_ref(), calldata.as_slice());
        assert_eq!(candidate.gas_price_wei, 2_500_000_000);
        assert_eq!(candidate.sequence_number, 42);
        // Fields recovered from the decoded envelope.
        assert_eq!(candidate.from, deterministic_signer().address());
        assert_eq!(candidate.gas_limit, 100_000);
        assert_eq!(candidate.value_wei, 0);
        assert_eq!(candidate.max_priority_fee_per_gas_wei, 1_000_000_000);
        assert_ne!(candidate.tx_hash, B256::ZERO);
        assert!(!candidate.raw_tx.is_empty());
    }

    #[test]
    fn analyse_one_returns_empty_for_unknown_selector() {
        let calldata = vec![0xde, 0xad, 0xbe, 0xef, 0x00, 0x00, 0x00, 0x00];
        let env = build_signed_eip1559(42161, TARGET, calldata, 1_000_000_000);
        let msg = make_broadcast_msg(&env, 1);
        let cfg = AnalysisConfig::default();
        assert!(analyse_one(&msg, &cfg).is_empty());
    }

    #[test]
    fn analyse_one_matches_via_address_when_selector_unknown() {
        let ostium = address!("0123456789abcdef0123456789abcdef01234567");
        let mut cfg = AnalysisConfig::default();
        cfg.address_targets
            .address_match
            .push((ostium, "Ostium.oracleProxy"));
        let calldata = vec![0xaa, 0xbb, 0xcc, 0xdd, 0x00, 0x01];
        let env = build_signed_eip1559(42161, ostium, calldata, 1_500_000_000);
        let msg = make_broadcast_msg(&env, 7);
        let candidates = analyse_one(&msg, &cfg);
        assert_eq!(candidates.len(), 1, "address match");
        let candidate = &candidates[0];
        assert_eq!(candidate.target_label, "address_match:Ostium.oracleProxy");
        assert_eq!(candidate.victim, Some(ostium));
        assert_eq!(candidate.selector, [0xaa, 0xbb, 0xcc, 0xdd]);
    }

    #[test]
    fn analyse_one_emits_per_matching_tx_in_batch() {
        // A batch carrying two selector-matching txs yields two
        // candidates; a non-matching third tx is filtered out.
        let mut cd = Vec::with_capacity(36);
        cd.extend_from_slice(&UNIV3_EXACT_INPUT_SINGLE);
        cd.extend_from_slice(&[0u8; 32]);
        let env_a = build_signed_eip1559(42161, TARGET, cd.clone(), 1_000_000_000);
        let env_b = build_signed_eip1559(42161, TARGET, cd.clone(), 2_000_000_000);
        let env_miss = build_signed_eip1559(
            42161,
            TARGET,
            vec![0xde, 0xad, 0xbe, 0xef, 0x00, 0x00, 0x00, 0x00],
            3_000_000_000,
        );
        let batch = batch_msg_bytes(&[
            signed_tx_msg_bytes(&env_a),
            signed_tx_msg_bytes(&env_miss),
            signed_tx_msg_bytes(&env_b),
        ]);
        let msg = make_broadcast_msg_raw(wrap_l2_bytes(&batch), 99);
        let cfg = AnalysisConfig::default();
        let candidates = analyse_one(&msg, &cfg);
        assert_eq!(candidates.len(), 2, "two matching txs, miss filtered");
        assert_eq!(candidates[0].gas_price_wei, 1_000_000_000);
        assert_eq!(candidates[1].gas_price_wei, 2_000_000_000);
    }

    #[test]
    fn analyse_one_empty_for_truncated_batch() {
        let msg = make_broadcast_msg_raw(wrap_l2_bytes(&[L2_MSG_KIND_BATCH, 0x00, 0x01]), 99);
        let cfg = AnalysisConfig::default();
        assert!(analyse_one(&msg, &cfg).is_empty());
    }

    // ----- IPC envelope round-trip ------------------------------------------

    /// Wire-shape lock: the JSON the engine emits MUST match the
    /// `WireFrontrunCandidate` shape declared in
    /// `coordinator/src/ipc/types.ts`. If you change any field name or
    /// type representation, update both sides + this assertion.
    /// A fully-populated `FrontrunCandidate` for wire-shape / round-trip
    /// assertions. `calldata` is the only field callers vary.
    fn sample_candidate(calldata: &'static [u8]) -> FrontrunCandidate {
        FrontrunCandidate {
            selector: UNIV3_EXACT_INPUT_SINGLE,
            target_label: "UniV3.exactInputSingle".to_string(),
            victim: Some(TARGET),
            tx_hash: b256!("1111111111111111111111111111111111111111111111111111111111111111"),
            from: address!("00000000000000000000000000000000000000aa"),
            raw_tx: Bytes::from_static(&[0x02, 0xde, 0xad]),
            calldata: Bytes::from_static(calldata),
            value_wei: 1_000_000_000_000_000_000,
            gas_limit: 100_000,
            gas_price_wei: 2_500_000_000,
            max_priority_fee_per_gas_wei: 250_000_000,
            observed_at_ms: 1_700_000_000_000,
            sequence_number: 12345,
        }
    }

    #[test]
    fn frontrun_candidate_wire_shape_locks_snake_case_field_names() {
        let candidate = sample_candidate(&[0x41, 0x4b, 0xf3, 0x89]);
        let json = serde_json::to_string(&Message::FrontrunCandidate(candidate)).unwrap();
        // Tag: serde external-tag form for the outer enum.
        assert!(json.contains("\"FrontrunCandidate\""));
        // Snake_case keys for every field — the TS WireFrontrunCandidate
        // depends on this exact shape.
        assert!(json.contains("\"selector\":[65,75,243,137]"));
        assert!(json.contains("\"target_label\":\"UniV3.exactInputSingle\""));
        assert!(json.contains("\"victim\":\""));
        assert!(json.contains(
            "\"tx_hash\":\"0x1111111111111111111111111111111111111111111111111111111111111111\""
        ));
        assert!(json.contains("\"from\":\"0x00000000000000000000000000000000000000aa\""));
        assert!(json.contains("\"raw_tx\":\"0x02dead\""));
        // calldata serializes as 0x-prefixed hex (alloy::primitives::Bytes default).
        assert!(json.contains("\"calldata\":\"0x414bf389\""));
        assert!(json.contains("\"value_wei\":1000000000000000000"));
        assert!(json.contains("\"gas_limit\":100000"));
        assert!(json.contains("\"gas_price_wei\":2500000000"));
        assert!(json.contains("\"max_priority_fee_per_gas_wei\":250000000"));
        assert!(json.contains("\"observed_at_ms\":1700000000000"));
        assert!(json.contains("\"sequence_number\":12345"));
    }

    #[test]
    fn frontrun_candidate_round_trips_through_message_envelope() {
        let candidate = sample_candidate(&[0x41, 0x4b, 0xf3, 0x89, 0x00, 0xff]);
        let msg = Message::FrontrunCandidate(candidate.clone());
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.starts_with("{\"FrontrunCandidate\":"));
        let back: Message = serde_json::from_str(&json).unwrap();
        match back {
            Message::FrontrunCandidate(c) => assert_eq!(c, candidate),
            other => panic!("expected FrontrunCandidate, got {other:?}"),
        }
    }

    // ----- decoder boundary: legacy tx --------------------------------------

    #[test]
    fn decode_l2_signed_txs_handles_legacy_envelope() {
        // EIP-2718 envelope decode also covers the legacy type-0 form
        // (the first byte being a list-header RLP byte rather than a
        // type prefix). Build one and confirm round-trip via analyse.
        use alloy::consensus::TxLegacy;
        let tx = TxLegacy {
            chain_id: Some(42161),
            nonce: 1,
            gas_price: 3_000_000_000,
            gas_limit: 100_000,
            to: alloy::primitives::TxKind::Call(TARGET),
            value: U256::ZERO,
            input: Bytes::from_static(&AAVE_V3_LIQUIDATION_CALL),
        };
        let signer = deterministic_signer();
        let sig_hash = tx.signature_hash();
        let sig: Signature = signer.sign_hash_sync(&sig_hash).expect("sign");
        let envelope: TxEnvelope = tx.into_signed(sig).into();
        let bm = make_broadcast_msg(&envelope, 100);
        let cfg = AnalysisConfig::default();
        let candidates = analyse_one(&bm, &cfg);
        assert_eq!(candidates.len(), 1, "legacy tx should still match selector");
        let c = &candidates[0];
        assert_eq!(c.selector, AAVE_V3_LIQUIDATION_CALL);
        // For a legacy tx `gas_price()` returns Some(price); the helper
        // falls back to max_fee_per_gas only when None.
        assert_eq!(c.gas_price_wei, 3_000_000_000);
    }

    // ----- canary against the schema mirror -----------------------------------

    #[test]
    fn hex_selector_formats_as_lowercase_0x_prefixed() {
        let s = hex_selector(&[0x41, 0x4b, 0xf3, 0x89]);
        assert_eq!(s, "0x414bf389");
    }

    // ----- topic byte assertion: anchor against the b256! macro --------------

    #[test]
    fn known_constant_is_b256_safe() {
        // Spot-check that an Arbitrum-typical tx hash decodes via the
        // alloy `b256!` macro — guards us against alloy version drift
        // that would break the rest of the IPC schema tests too.
        let _ = b256!("0000000000000000000000000000000000000000000000000000000000000001");
    }
}
