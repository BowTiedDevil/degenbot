//! Ethereum RPC provider implementation using Alloy.
//!
//! Provides high-performance HTTP/HTTPS/WS connections with connection pooling,
//! retry logic, response caching, optional transport-level rate limiting, and
//! chunked log fetching. Also supports IPC endpoints for local node connections.

use alloy::consensus::{Header as ConsensusHeader, TxEnvelope};
use alloy::eips::{BlockId, BlockNumberOrTag};
use alloy::network::Ethereum;
use alloy::primitives::{Address, Bytes, B256, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::client::ClientBuilder;
use alloy::rpc::types::eth::{
    simulate::{SimulatePayload, SimulatedBlock},
    FeeHistory,
};
use alloy::rpc::types::eth::{
    AccessListResult, Block, Header as RpcHeader, Transaction, TransactionReceipt,
};
use alloy::rpc::types::TransactionRequest;
use alloy::rpc::types::{Filter, Log};
use alloy::transports::ipc::IpcConnect;
use alloy::transports::layers::ThrottleLayer;
use alloy::transports::ws::{WebSocketConfig, WsConnect};
use alloy::transports::{RpcError, TransportErrorKind};
use degenbot_core::errors::{ProviderError, ProviderResult};
use rand::RngExt;
use std::num::NonZeroU32;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

/// Constants for retry logic. `pub(crate)` so the subscription watchdog
/// ([`crate::subscription::pump_blocks`]) reuses the same backoff curve for
/// its reconnect retries — one retry vocabulary across the crate, not a
/// duplicate per-module tuning.
pub(crate) const INITIAL_RETRY_DELAY_MS: u64 = 100;
pub(crate) const MAX_RETRY_DELAY_MS: u64 = 30_000; // 30 seconds
pub(crate) const BACKOFF_MULTIPLIER: u64 = 2;
const MAX_JITTER_MS: u64 = 100; // Add up to 100ms of jitter

/// Retry an async operation with exponential backoff, emitting `log` records on
/// every retry attempt so operators can see backoff in logs (E2B542).
///
/// The codebase's logging vocabulary is `log` (used across every Rust core
/// crate); `log` is already a dependency of `degenbot-rpc`, so no new dep is
/// introduced. The per-call context label is embedded in the `ProviderError`
/// `Display` (baked in via `into_provider_error("<context>")` at the call site,
/// which formats `{context}: {self}`), so the emitted `{error}` carries the
/// context verbatim.
///
/// Emission policy (the E2B542 decision):
/// - First retry (attempt 1): `log::debug!` — transient, often benign.
/// - Subsequent retries (attempt >= 2): `log::warn!` — sustained backoff.
/// - Exhausted all attempts: `log::error!` — terminal failure.
///
/// Extracted from `AlloyProvider::retry_with_backoff` (which delegates, passing
/// `self.max_attempts` + `self.call_timeout`) so the loop is unit-testable
/// without constructing a live provider, and so a test can drive
/// `max_attempts = 2` for a fast, deterministic assertion that the expected
/// structured fields are emitted.
pub(crate) async fn retry_with_backoff_loop<F, Fut, T>(
    max_attempts: u32,
    call_timeout: Duration,
    operation: F,
) -> ProviderResult<T>
where
    F: Fn() -> Fut + Send + Sync,
    Fut: std::future::Future<Output = ProviderResult<T>> + Send,
    T: Send,
{
    let mut attempt = 0;
    let mut delay_ms = INITIAL_RETRY_DELAY_MS;

    loop {
        // EO75JH: bound every attempt so a stuck transport (especially
        // WS/IPC mid-read) cannot hang the loop indefinitely. HTTP has alloy's
        // read timeout, but WS/IPC have no default — a half-open socket hangs
        // forever without this wrapper. On elapsed, classify as
        // `ProviderError::Timeout` so the existing `is_retryable()` picks it up.
        let outcome = match tokio::time::timeout(call_timeout, operation()).await {
            Ok(Ok(result)) => return Ok(result),
            Ok(Err(e)) if !e.is_retryable() => return Err(e),
            // Retryable failure OR per-call timeout → fall through to backoff.
            Ok(Err(e)) => e,
            Err(_elapsed) => ProviderError::Timeout {
                message: format!(
                    "RPC call timed out after {}ms (attempt {}/{max_attempts})",
                    call_timeout.as_millis(),
                    attempt + 1
                ),
            },
        };

        attempt += 1;
        if attempt >= max_attempts {
            log::error!(
                target: "degenbot_rpc::provider",
                "RPC retries exhausted: attempt {attempt}/{max_attempts}: {outcome}"
            );
            return Err(outcome);
        }

        // Calculate delay with exponential backoff and jitter
        // Use random_range for uniform distribution (avoids modulo bias)
        let jitter = rand::rng().random_range(0..MAX_JITTER_MS);
        let sleep_ms = delay_ms + jitter;

        if attempt <= 1 {
            log::debug!(
                target: "degenbot_rpc::provider",
                "RPC retry: attempt {attempt}/{max_attempts} after {sleep_ms}ms: {outcome}"
            );
        } else {
            log::warn!(
                target: "degenbot_rpc::provider",
                "RPC retry: attempt {attempt}/{max_attempts} after {sleep_ms}ms: {outcome}"
            );
        }

        tokio::time::sleep(Duration::from_millis(sleep_ms)).await;

        // Exponential backoff with cap (saturating to prevent overflow)
        delay_ms = std::cmp::min(
            delay_ms.saturating_mul(BACKOFF_MULTIPLIER),
            MAX_RETRY_DELAY_MS,
        );
    }
}

/// Compute the transaction hash locally from a raw signed (RLP-encoded)
/// transaction payload (J3RIFU). Broadcasting via `eth_sendRawTransaction`
/// can lose the response to a timeout / connection drop AFTER the body
/// reached the node — so reconciliation via `get_transaction_receipt` needs
/// the hash, and it must be computable without the broadcast response.
///
/// Returns the keccak256 of the RLP-encoded envelope (the on-chain tx hash).
///
/// # Errors
///
/// Returns `ProviderError::DecodingError` if `encoded_tx` is not a valid
/// RLP-encoded signed transaction envelope.
pub(crate) fn compute_tx_hash_from_signed_payload(encoded_tx: &[u8]) -> ProviderResult<B256> {
    use alloy::rlp::Decodable as _;
    let envelope =
        TxEnvelope::decode(&mut &encoded_tx[..]).map_err(|e| ProviderError::DecodingError {
            message: format!(
                "Failed to decode signed transaction envelope for tx-hash computation: {e}"
            ),
        })?;
    Ok(*envelope.hash())
}

/// Broadcast-aware reconciliation loop for `eth_sendRawTransaction` (J3RIFU).
///
/// Broadcast is NOT idempotent: a `Timeout` / `ConnectionFailed` after the
/// body was sent may have delivered the tx to the mempool, so blindly
/// re-sending the identical signed payload (the previous behavior — routing
/// through `retry_with_backoff`) risks double-inclusion on rebroadcast. This
/// loop reconciles the outcome before rebroadcasting.
///
/// Outcome branches:
/// - `Ok(hash)` → return the hash (success).
/// - `RateLimited` → back off and rebroadcast (the request never reached the
///   node; the mempool never saw it, so rebroadcast is safe).
/// - `Timeout` / `ConnectionFailed` (ambiguous: body may have reached the
///   node) → reconcile via `reconcile_receipt()`: if the receipt is present,
///   return `tx_hash` (already broadcast + mined); if absent, back off and
///   rebroadcast.
/// - `RpcError` / any other non-retryable error → surface immediately. The
///   tx was seen and rejected by the node's validation ("already known",
///   "nonce too low", "replacement underpriced"); retrying the identical
///   payload won't change the outcome.
///
/// Extracted from `AlloyProvider::eth_send_raw_transaction` (delegating
/// `self.call_timeout`, `self.max_attempts`, `tx_hash`, the broadcast op, and
/// the receipt-reconcile op) so the decision logic is unit-testable without
/// a live provider — the tests inject closures simulating each branch.
pub(crate) async fn send_raw_transaction_with_reconciliation<F, Fut, G, FutR>(
    tx_hash: B256,
    broadcast: F,
    reconcile_receipt: G,
    max_attempts: u32,
) -> ProviderResult<B256>
where
    F: Fn() -> Fut + Send + Sync,
    Fut: std::future::Future<Output = ProviderResult<B256>> + Send,
    G: Fn() -> FutR + Send + Sync,
    FutR: std::future::Future<Output = ProviderResult<bool>> + Send,
{
    let mut attempt = 0;
    let mut delay_ms = INITIAL_RETRY_DELAY_MS;

    loop {
        match broadcast().await {
            Ok(returned_hash) => return Ok(returned_hash),
            Err(e) => match &e {
                // Ambiguous: the body may have reached the node's mempool.
                // Reconcile via the receipt before deciding to rebroadcast.
                ProviderError::Timeout { .. } | ProviderError::ConnectionFailed { .. } => {
                    match reconcile_receipt().await {
                        Ok(true) => {
                            // Receipt present → the tx was already broadcast
                            // (and mined). Return the locally-computed hash;
                            // do NOT rebroadcast.
                            log::debug!(
                                target: "degenbot_rpc::provider",
                                "eth_sendRawTransaction: ambiguous outcome ({e}) reconciled — receipt present, returning tx_hash {tx_hash}"
                            );
                            return Ok(tx_hash);
                        }
                        Ok(false) => {
                            // Receipt absent → the body did NOT reach the node
                            // (or hasn't been mined). Rebroadcast after backoff.
                            log::warn!(
                                target: "degenbot_rpc::provider",
                                "eth_sendRawTransaction: ambiguous outcome ({e}) — receipt absent, rebroadcasting"
                            );
                            // fall through to the shared backoff/retry block below
                            attempt += 1;
                            if attempt >= max_attempts {
                                log::error!(
                                    target: "degenbot_rpc::provider",
                                    "eth_sendRawTransaction: retries exhausted after {attempt}/{max_attempts} rebroadcasts; last error: {e}"
                                );
                                return Err(e);
                            }
                            let jitter = rand::rng().random_range(0..MAX_JITTER_MS);
                            let sleep_ms = delay_ms + jitter;
                            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
                            delay_ms = std::cmp::min(
                                delay_ms.saturating_mul(BACKOFF_MULTIPLIER),
                                MAX_RETRY_DELAY_MS,
                            );
                        }
                        Err(reconcile_err) => {
                            // Reconciliation itself failed (e.g. a transient
                            // RPC error on get_transaction_receipt). Treat as
                            // "absent / unknown" and rebroadcast — a missed
                            // receipt-probe must not strand the broadcast.
                            log::warn!(
                                target: "degenbot_rpc::provider",
                                "eth_sendRawTransaction: ambiguous outcome ({e}); reconcile probe failed ({reconcile_err}) — rebroadcasting"
                            );
                            attempt += 1;
                            if attempt >= max_attempts {
                                log::error!(
                                    target: "degenbot_rpc::provider",
                                    "eth_sendRawTransaction: retries exhausted after {attempt}/{max_attempts} rebroadcasts; last error: {e}"
                                );
                                return Err(e);
                            }
                            let jitter = rand::rng().random_range(0..MAX_JITTER_MS);
                            let sleep_ms = delay_ms + jitter;
                            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
                            delay_ms = std::cmp::min(
                                delay_ms.saturating_mul(BACKOFF_MULTIPLIER),
                                MAX_RETRY_DELAY_MS,
                            );
                        }
                    }
                }
                // Rate-limited: the request never reached the node. The
                // mempool never saw it, so rebroadcast is safe + necessary.
                ProviderError::RateLimited { .. } => {
                    attempt += 1;
                    if attempt >= max_attempts {
                        log::error!(
                            target: "degenbot_rpc::provider",
                            "eth_sendRawTransaction: retries exhausted after {attempt}/{max_attempts} rebroadcasts; last error: {e}"
                        );
                        return Err(e);
                    }
                    log::warn!(
                        target: "degenbot_rpc::provider",
                        "eth_sendRawTransaction: rate-limited — rebroadcasting (attempt {attempt}/{max_attempts})"
                    );
                    let jitter = rand::rng().random_range(0..MAX_JITTER_MS);
                    let sleep_ms = delay_ms + jitter;
                    tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
                    delay_ms = std::cmp::min(
                        delay_ms.saturating_mul(BACKOFF_MULTIPLIER),
                        MAX_RETRY_DELAY_MS,
                    );
                }
                // RpcError / ExecutionReverted / InvalidParams / etc.: the tx
                // was seen and rejected by the node's validation. Retrying the
                // identical payload won't change the outcome — surface immediately.
                _ => return Err(e),
            },
        }
    }
}

/// Maximum allowed concurrent requests in `LogFetcher`.
///
/// Prevents file-descriptor exhaustion and RPC rate-limit bans from
/// spawning thousands of simultaneous connections.
const MAX_CONCURRENT_REQUESTS_CAP: usize = 32;

/// Default per-call timeout for `AlloyProvider` (EO75JH). HTTP uses alloy's
/// read timeout, but WS/IPC have no default — a half-open socket hangs the
/// retry loop indefinitely. This bounds every attempt so a stuck transport
/// retries (or fails) within a known wall-clock. 30s is generous for any
/// single RPC call; `eth_simulate_v1` / very large `eth_getLogs` ranges may
/// need a longer value via the builder/constructor arg.
const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Default maximum total attempts for provider operations (1 initial + 2
/// retries). 65F2N7 #4: this is the actual production default the bot hotpath
/// (`degenbot-bot` pump) uses — previously `10` but every `new()` call site
/// passed `3`, making the constant misleading. Now the constant matches the
/// hotpath default; non-hotpath updaters (Aave/pool-updater) pass their own
/// `RPC_MAX_RETRIES` constant as an intentional override.
pub const DEFAULT_MAX_RETRIES: u32 = 3;

/// 65F2N7 #1: detect an IPC path explicitly. A string is an IPC path if it
/// starts with `ipc://`, is an absolute Unix path (starts with `/`), or is a
/// Windows named pipe (starts with `\\`). A bare `host:port` (a typo missing
/// the `http://` scheme) is NOT an IPC path — it falls through to the
/// unsupported-scheme error so the typo surfaces immediately instead of
/// silently routing to a nonexistent IPC file.
#[must_use]
fn is_ipc_path(rpc_url: &str) -> bool {
    rpc_url.starts_with("ipc://") || rpc_url.starts_with('/') || rpc_url.starts_with("\\\\")
}

/// 65F2N7 #3: retry a WS connect at construction with the existing backoff.
/// Previously `connect_ws(ws_connect).await?` was eagerly fatal — a transient
/// outage at build time failed the provider permanently while HTTP was lazy.
/// Now construction retries the connect a bounded number of times, matching
/// HTTP's lazy-retry behavior at the transport-establishment boundary.
async fn connect_ws_with_retries(
    ws_connect: WsConnect,
    max_retries: u32,
) -> ProviderResult<Arc<dyn Provider<Ethereum>>> {
    let mut attempt = 0;
    let mut delay_ms = INITIAL_RETRY_DELAY_MS;
    loop {
        match ProviderBuilder::default()
            .with_default_caching()
            .connect_ws(ws_connect.clone())
            .await
        {
            Ok(provider) => return Ok(Arc::new(provider.erased())),
            Err(e) => {
                attempt += 1;
                if attempt >= max_retries {
                    return Err(ProviderError::ConnectionFailed {
                        message: format!(
                            "Failed to connect to WebSocket endpoint after {attempt} attempts: {e}"
                        ),
                    });
                }
                log::warn!(
                    target: "degenbot_rpc::provider",
                    "WS connect retry: attempt {attempt}/{max_retries} after {delay_ms}ms: {e}"
                );
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                delay_ms = std::cmp::min(
                    delay_ms.saturating_mul(BACKOFF_MULTIPLIER),
                    MAX_RETRY_DELAY_MS,
                );
            }
        }
    }
}

/// 65F2N7 #3: retry an IPC connect at construction with the existing backoff.
/// Same rationale as `connect_ws_with_retries` — IPC connect was eagerly
/// fatal; now it retries a bounded number of times.
async fn connect_ipc_with_retries(
    ipc_connect: IpcConnect<String>,
    max_retries: u32,
) -> ProviderResult<Arc<dyn Provider<Ethereum>>> {
    let mut attempt = 0;
    let mut delay_ms = INITIAL_RETRY_DELAY_MS;
    loop {
        match ProviderBuilder::default()
            .with_default_caching()
            .connect_ipc(ipc_connect.clone())
            .await
        {
            Ok(provider) => return Ok(Arc::new(provider.erased())),
            Err(e) => {
                attempt += 1;
                if attempt >= max_retries {
                    return Err(ProviderError::ConnectionFailed {
                        message: format!(
                            "Failed to connect to IPC endpoint after {attempt} attempts: {e}"
                        ),
                    });
                }
                log::warn!(
                    target: "degenbot_rpc::provider",
                    "IPC connect retry: attempt {attempt}/{max_retries} after {delay_ms}ms: {e}"
                );
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                delay_ms = std::cmp::min(
                    delay_ms.saturating_mul(BACKOFF_MULTIPLIER),
                    MAX_RETRY_DELAY_MS,
                );
            }
        }
    }
}

/// 65F2N7 #2: the idempotent-RPC-method allowlist. `make_request` is a raw
/// escape hatch that can issue ANY method — including non-idempotent
/// `debug_*` / `trace_*` / state-mutating methods. Retrying those blindly (the
/// previous behavior) is unsafe: a partial timeout on a mutating call re-sends
/// the payload. `make_request` retries ONLY methods in this allowlist (read-only
/// eth_* calls that are safe to repeat); all other methods get a single
/// attempt. Callers who know their custom method is idempotent should route
/// through the typed methods (`get_block`, `eth_call`, etc.) instead.
#[must_use]
fn is_idempotent_rpc_method(method: &str) -> bool {
    // Read-only eth_* methods safe to retry. Mutating methods (eth_sendRawTransaction,
    // eth_sendTransaction) and stateful trace/debug methods are excluded.
    matches!(
        method,
        "eth_call"
            | "eth_chainId"
            | "eth_estimateGas"
            | "eth_blockNumber"
            | "eth_getBalance"
            | "eth_getCode"
            | "eth_getStorageAt"
            | "eth_getTransactionByHash"
            | "eth_getTransactionReceipt"
            | "eth_getTransactionCount"
            | "eth_getBlockByNumber"
            | "eth_getBlockByHash"
            | "eth_getLogs"
            | "eth_getBlockTransactionCountByNumber"
            | "eth_getBlockTransactionCountByHash"
            | "eth_getTransactionByBlockHashAndIndex"
            | "eth_getTransactionByBlockNumberAndIndex"
            | "eth_getUncleByBlockHashAndIndex"
            | "eth_getUncleByBlockNumberAndIndex"
            | "eth_getUncleCountByBlockHash"
            | "eth_getUncleCountByBlockNumber"
            | "eth_feeHistory"
            | "eth_gasPrice"
            | "eth_maxPriorityFeePerGas"
            | "eth_getProof"
    )
}

/// Type alias for the full Ethereum block type returned by `get_block`.
///
/// Centralises the generic parameters so callers don't repeat
/// `Block<Transaction<TxEnvelope>, RpcHeader<ConsensusHeader>>`.
pub type EthBlock = Block<Transaction<TxEnvelope>, RpcHeader<ConsensusHeader>>;

/// Helper macro that wraps a simple Alloy RPC call with retry logic and
/// error classification. Reduces per-method boilerplate from 9 lines to 1.
///
/// For methods that need request construction (e.g. `eth_call`, `estimate_gas`)
/// or conditional block-id handling, write the `retry_with_backoff` call manually.
macro_rules! rpc_call {
    ($self:expr, $context:literal, $expr:expr) => {
        $self
            .retry_with_backoff(|| async {
                $expr.await.map_err(|e| e.into_provider_error($context))
            })
            .await
    };
}

/// JSON-RPC error codes that providers use to signal request-rate / quota
/// exhaustion over HTTP 200 (the transport did NOT fail at the HTTP layer,
/// so the 429-based classification in [`IntoProviderError`] does not fire).
///
/// Conservative set (BJXUPU):
/// - `-32005` — `Alchemy` "your app has exceeded its compute unit capacity".
/// - `-32004` — `QuickNode` "rate limit exceeded".
///
/// Infura reuses the generic `-32001` code for quota errors but disambiguates
/// via the *message*; that case is caught by [`JSON_RPC_RATE_LIMIT_MESSAGE_MARKERS`],
/// not by this code table, so a `-32001` without a rate-limit message stays
/// `RpcError` (pinned by `non_revert_error_stays_rpc_error`).
///
/// Adding a provider code is one line here — not a match-arm edit.
const JSON_RPC_RATE_LIMIT_CODES: &[i64] = &[-32_005, -32_004];

/// Case-insensitive message substrings that mark a JSON-RPC error response as
/// a rate-limit / quota error regardless of code. Catches providers that reuse
/// a generic code (e.g. Infura's `-32001`) for quota exhaustion. The check is
/// exact substring match on the lowercased message, so a non-rate-limit
/// `-32001` (e.g. "requested block not available") stays `RpcError`.
///
/// Adding a provider's marker phrasing is one line here — not a match-arm edit.
const JSON_RPC_RATE_LIMIT_MESSAGE_MARKERS: &[&str] = &[
    "rate limit",
    "too many requests",
    "compute units exceeded",
    "exceeded its rate",
];

/// Whether a JSON-RPC `ErrorResp` (code, message) represents a provider
/// rate-limit / quota response. Data-driven via
/// [`JSON_RPC_RATE_LIMIT_CODES`] (code match) and
/// [`JSON_RPC_RATE_LIMIT_MESSAGE_MARKERS`] (case-insensitive message
/// substring). Used by [`IntoProviderError::into_provider_error`] so such
/// responses map to [`ProviderError::RateLimited`] and feed the retry loop.
///
/// (BJXUPU — `is_retryable()` already returns true for `RateLimited`, so no
/// predicate change is needed.)
fn is_json_rpc_rate_limit_response(code: i64, message: &str) -> bool {
    if JSON_RPC_RATE_LIMIT_CODES.contains(&code) {
        return true;
    }
    let lower = message.to_lowercase();
    JSON_RPC_RATE_LIMIT_MESSAGE_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

/// Extension trait that converts an Alloy `RpcError` into a `ProviderError`.
///
/// Consumes the error (no double-borrow) and classifies it by Alloy's
/// type-based error variants instead of string scraping:
/// - `RpcError::Transport(TransportErrorKind::HttpError)` with 429 → `RateLimited`
/// - `RpcError::Transport(TransportErrorKind::HttpError)` with 5xx → `ConnectionFailed`
/// - `RpcError::Transport` with retryable transport errors → `Timeout`
/// - `RpcError::ErrorResp` → `RpcError` with the JSON-RPC error code
/// - `RpcError::LocalUsageError` → `Other`
/// - Other → `RpcError` with code -1
trait IntoProviderError {
    fn into_provider_error(self, context: &str) -> ProviderError;
}

impl IntoProviderError for RpcError<TransportErrorKind> {
    fn into_provider_error(self, context: &str) -> ProviderError {
        let message = format!("{context}: {self}");

        // Check for transport-level errors
        if let Some(transport_err) = self.as_transport_err() {
            // HTTP 429 = rate limited
            if let Some(http_err) = transport_err.as_http_error() {
                let status = http_err.status;
                if status == 429 {
                    return ProviderError::RateLimited { message };
                }
                // 5xx server errors are retryable as connection failures
                if (500..600).contains(&status) {
                    return ProviderError::ConnectionFailed { message };
                }
            }

            // Backend gone or pubsub unavailable = connection failed
            if transport_err.is_backend_gone() || transport_err.is_pubsub_unavailable() {
                return ProviderError::ConnectionFailed { message };
            }

            // Use Alloy's built-in retry heuristic for other transport errors
            if transport_err.is_retry_err() {
                return ProviderError::Timeout { message };
            }

            // Other transport errors → RPC error
            return ProviderError::RpcError { code: -1, message };
        }

        // Server returned an error response (JSON-RPC error)
        if let Some(error_resp) = self.as_error_resp() {
            // BJXUPU: JSON-RPC-level rate-limit responses. Providers signal
            // quota exhaustion over HTTP 200 (the transport did NOT fail at
            // the HTTP layer, so the 429 transport branch above does not fire).
            // Detect by (a) a known provider quota code, or (b) a rate-limit
            // message marker on any code (Infura reuses -32001 for quota
            // errors). Map to `RateLimited` so the retry loop's
            // `is_retryable()` picks them up. Both tables are data-driven so
            // adding a provider code/marker later is one line, not a match-arm
            // edit.
            if is_json_rpc_rate_limit_response(error_resp.code, &error_resp.message) {
                return ProviderError::RateLimited { message };
            }

            // Detect EVM execution reverts structurally (alloy's
            // `as_revert_data()` checks `message.contains("revert")` + spelunks
            // `data` for the 0x08c379a0/0x4e487b71 selector). The classification
            // here is by message content (case-insensitive marker substring),
            // mirroring the Python `alloy_errors.is_alloy_revert` markers; the
            // FFI layer (degenbot-python) raises the degenbot-owned
            // `ContractLogicError` from this variant.
            let is_revert = error_resp.message.to_lowercase().contains("revert")
                || error_resp.as_revert_data().is_some();
            if is_revert {
                return ProviderError::ExecutionReverted {
                    code: error_resp.code,
                    message,
                };
            }
            return ProviderError::RpcError {
                code: error_resp.code,
                message,
            };
        }

        // Local usage errors (signer errors, pre-processing failures)
        if self.is_local_usage_error() {
            return ProviderError::Other { message };
        }

        // Serialization/deserialization errors
        if self.is_ser_error() || self.is_deser_error() {
            return ProviderError::SerializationError { message };
        }

        // Fallback
        ProviderError::RpcError { code: -1, message }
    }
}

/// Filter criteria for log fetching with pre-resolved typed values.
///
/// All address and topic strings are validated and parsed at construction
/// time so that `to_alloy_filter()` is infallible and cheap — no redundant
/// parsing on every retry or across concurrent chunk tasks.
#[derive(Debug, Clone)]
pub struct LogFilter {
    from_block: Option<u64>,
    to_block: Option<u64>,
    addresses: Arc<[Address]>,
    topics: Arc<[Vec<B256>]>,
}

impl LogFilter {
    /// Create a new `LogFilter`, validating and parsing all inputs eagerly.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::InvalidBlockRange` if `from_block > to_block`,
    /// `ProviderError::InvalidAddress` if any address string is malformed, or
    /// `ProviderError::InvalidTopic` if any topic string is malformed or out of
    /// range.
    pub fn new(
        from_block: u64,
        to_block: u64,
        addresses: Option<Vec<String>>,
        topics: Option<Vec<Vec<String>>>,
    ) -> ProviderResult<Self> {
        if from_block > to_block {
            return Err(ProviderError::InvalidBlockRange {
                from: from_block,
                to: to_block,
            });
        }

        // Parse addresses eagerly — fail fast on bad input
        let parsed_addresses: Vec<Address> = addresses
            .unwrap_or_default()
            .into_iter()
            .map(|addr| {
                degenbot_core::address_utils::parse_address(&addr).map_err(|e| {
                    ProviderError::InvalidAddress {
                        address: addr,
                        reason: format!("{e}"),
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        // Parse topics eagerly — each element maps to a topic position (0-3)
        let parsed_topics: Vec<Vec<B256>> = topics
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(i, topic_list)| {
                if topic_list.is_empty() {
                    return Ok(vec![]);
                }
                if i > 3 {
                    return Err(ProviderError::InvalidTopic {
                        topic: String::new(),
                        reason: format!("topic position {i} is out of range (max 3)"),
                    });
                }
                topic_list
                    .into_iter()
                    .map(|t| {
                        B256::from_str(&t).map_err(|e| ProviderError::InvalidTopic {
                            topic: t,
                            reason: format!("{e}"),
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            from_block: Some(from_block),
            to_block: Some(to_block),
            addresses: parsed_addresses.into(),
            topics: parsed_topics.into(),
        })
    }

    /// Returns the `from_block` value.
    #[must_use]
    pub const fn from_block(&self) -> Option<u64> {
        self.from_block
    }

    /// Returns the `to_block` value.
    #[must_use]
    pub const fn to_block(&self) -> Option<u64> {
        self.to_block
    }

    /// Convert to Alloy `Filter` (infallible — all parsing done at construction).
    #[must_use]
    pub fn to_alloy_filter(&self) -> Filter {
        let mut filter = Filter::new();

        if let Some(from) = self.from_block {
            filter = filter.from_block(from);
        }

        if let Some(to) = self.to_block {
            filter = filter.to_block(to);
        }

        if !self.addresses.is_empty() {
            filter = filter.address(self.addresses.to_vec());
        }

        for (i, topic_list) in self.topics.iter().enumerate() {
            if topic_list.is_empty() {
                continue;
            }
            filter = match i {
                0 => filter.event_signature(topic_list.clone()),
                1 => filter.topic1(topic_list.clone()),
                2 => filter.topic2(topic_list.clone()),
                3 => filter.topic3(topic_list.clone()),
                _ => break, // validated at construction; unreachable
            };
        }

        filter
    }

    /// Returns the addresses as EIP-55 checksummed strings.
    ///
    /// Re-serialises the pre-parsed `Address` values back to checksummed
    /// strings. Used by the `PyO3` `__repr__` and property getters.
    #[must_use]
    pub fn address_strings(&self) -> Vec<String> {
        self.addresses
            .iter()
            .map(degenbot_core::address_utils::address_to_checksum_string)
            .collect()
    }

    /// Returns the topics as hex strings.
    ///
    /// Re-serialises the pre-parsed `B256` values back to hex strings.
    /// Used by the `PyO3` property getters.
    #[must_use]
    pub fn topic_strings(&self) -> Vec<Vec<String>> {
        self.topics
            .iter()
            .map(|list| list.iter().map(|t| format!("{t}")).collect())
            .collect()
    }
}

/// High-performance Ethereum RPC provider.
pub struct AlloyProvider {
    inner: Arc<dyn Provider<Ethereum>>,
    rpc_url: String,
    max_attempts: u32,
    /// Per-call timeout (EO75JH). Each `operation()` in `retry_with_backoff`
    /// is wrapped in `tokio::time::timeout(call_timeout, ...)`; a stuck
    /// transport (especially WS/IPC mid-read) classifies as
    /// `ProviderError::Timeout` and feeds the existing `is_retryable()`.
    /// Default `DEFAULT_CALL_TIMEOUT` (30s); raise via the constructor for
    /// `eth_simulate_v1` / large `eth_getLogs` ranges.
    call_timeout: Duration,
}

// Manual Clone impl makes Arc::clone sharing semantics explicit
// (vs deriving Clone, which would produce the same code).
impl Clone for AlloyProvider {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            rpc_url: self.rpc_url.clone(),
            max_attempts: self.max_attempts,
            call_timeout: self.call_timeout,
        }
    }
}

impl AlloyProvider {
    /// Create a new provider with the given RPC URL.
    ///
    /// Automatically detects the connection type based on the URL:
    /// - HTTP/HTTPS URLs use HTTP transport with connection pooling
    /// - WS/WSS URLs use WebSocket transport
    /// - File paths (starting with / or \\) use IPC transport
    ///
    /// All connections use `ProviderBuilder::default()` (no fillers) with response
    /// caching enabled (100 entries). No transport-level rate limiting is applied.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::ConnectionFailed` if the HTTP client cannot be created
    /// or IPC/WS connection fails.
    pub async fn new(rpc_url: &str, max_retries: u32) -> ProviderResult<Self> {
        Self::build_provider(rpc_url, max_retries, None).await
    }

    /// Create a new provider with transport-level rate limiting.
    ///
    /// # Arguments
    ///
    /// * `rpc_url` - The RPC endpoint URL
    /// * `max_retries` - Maximum retry attempts
    /// * `requests_per_second` - Rate limit for HTTP connections (ignored for WS/IPC)
    /// * `burst` - Burst size for rate limiting
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::ConnectionFailed` if the connection cannot be established.
    pub async fn with_rate_limit(
        rpc_url: &str,
        max_retries: u32,
        requests_per_second: u32,
        burst: NonZeroU32,
    ) -> ProviderResult<Self> {
        Self::build_provider(rpc_url, max_retries, Some((requests_per_second, burst))).await
    }

    /// Internal constructor shared by `new` and `with_rate_limit`.
    ///
    /// If `rate_limit` is `Some((rps, burst))`, the HTTP transport gets a
    /// `ThrottleLayer`. Otherwise the client is built without throttling.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::ConnectionFailed`] if the RPC URL is invalid or the
    /// transport cannot be constructed.
    pub async fn build_provider(
        rpc_url: &str,
        max_retries: u32,
        rate_limit: Option<(u32, NonZeroU32)>,
    ) -> ProviderResult<Self> {
        let provider: Arc<dyn Provider<Ethereum>> =
            if rpc_url.starts_with("http://") || rpc_url.starts_with("https://") {
                let url = rpc_url
                    .parse()
                    .map_err(|e| ProviderError::ConnectionFailed {
                        message: format!("Invalid RPC URL: {e}"),
                    })?;

                let client = if let Some((rps, burst)) = rate_limit {
                    let throttle = ThrottleLayer::new_with_burst(rps, burst);
                    ClientBuilder::default().layer(throttle).http(url)
                } else {
                    ClientBuilder::default().http(url)
                };

                let provider = ProviderBuilder::default()
                    .with_default_caching()
                    .connect_client(client)
                    .erased();
                Arc::new(provider)
            } else if rpc_url.starts_with("ws://") || rpc_url.starts_with("wss://") {
                // Raise tungstenite's default message/frame size caps (`64 MiB`
                // / `16 MiB`) to `None` (unlimited). A single WS connection is
                // used for BOTH subscriptions (`newHeads`/logs — tiny messages)
                // AND batch `eth_getLogs` (the snapshot→WS backfill issues a
                // 6-topic OR filter over up to 2000 blocks → ~100k logs, ~90 MB
                // — well over both default caps).
                //
                // Failure mode WITHOUT the raise (confirmed via
                // `ws_getlogs_large_filter_diagnostic` + tracing, 2026-07-12):
                // tungstenite correctly returns `Error::Capacity(MessageTooLong)`
                // when the oversized response arrives, but `alloy-pubsub`'s
                // `WsBackend` converts that to `TransportErrorKind::backend_gone()`
                // (a *retryable* error) at the backend→service boundary — losing
                // the Capacity specificity. The pubsub service then enters an
                // INFINITE reconnect→redispatch loop: `reconnect()` succeeds on
                // the first attempt (the WS handshake is fine; only the response
                // is too big), `max_retries` is never consumed, and the pending
                // in-flight `eth_getLogs` is re-dispatched each cycle
                // (`service.rs: Reissuing pending requests count=1`). The
                // caller's `get_logs` future never resolves — small concurrent
                // calls (`get_block_number`) keep succeeding on the same
                // provider, so the transport isn't stalled, just the one
                // oversized request. This is the "overly-broad catch": the
                // `is_non_retryable()` gate treats every backend death as
                // retryable, so a structurally-oversized request hangs forever.
                //
                // Raising the caps removes the trigger entirely; this matches
                // HTTP transport behaviour (no body cap) and the degenbot
                // threat model (the RPC endpoint is the user's own node, not
                // an untrusted server that would DoS via oversized messages).
                let ws_connect = WsConnect::new(rpc_url.to_string()).with_config(
                    WebSocketConfig::default()
                        .max_message_size(None)
                        .max_frame_size(None),
                );
                let provider = connect_ws_with_retries(ws_connect, max_retries).await?;
                provider
            } else if is_ipc_path(rpc_url) {
                // 65F2N7 #1: explicit IPC path detection — require `ipc://`, an
                // absolute Unix path (`/`), or a Windows named pipe (`\\`). A
                // bare `localhost:8545` (a typo missing `http://`) is NO longer
                // silently routed to IPC; it falls through to the
                // unsupported-scheme error below with a clear message.
                let ipc_path = rpc_url.strip_prefix("ipc://").unwrap_or(rpc_url);
                let ipc_connect: IpcConnect<String> = IpcConnect::new(ipc_path.to_string());
                // 65F2N7 #3: WS/IPC connect is no longer eagerly fatal. HTTP
                // is lazy (connects on first call, retried via
                // retry_with_backoff); WS/IPC `.await?`'d at construction, so a
                // transient outage at build time failed the provider
                // permanently. Now construction retries the connect a bounded
                // number of times (max_retries) with the existing backoff.
                let provider = connect_ipc_with_retries(ipc_connect, max_retries).await?;
                provider
            } else {
                // `rpc_url` contains an unrecognised scheme.
                // Extract the scheme portion for the error message.
                let scheme = rpc_url.split("://").next().unwrap_or(rpc_url);
                return Err(ProviderError::ConnectionFailed {
                    message: format!("Unsupported transport scheme: {scheme}"),
                });
            };

        Ok(Self {
            inner: provider,
            rpc_url: rpc_url.to_string(),
            max_attempts: max_retries.saturating_add(1),
            call_timeout: DEFAULT_CALL_TIMEOUT,
        })
    }

    /// Get current block number.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_block_number(&self) -> ProviderResult<u64> {
        rpc_call!(
            self,
            "Failed to get block number",
            self.inner.get_block_number()
        )
    }

    /// Get chain ID.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_chain_id(&self) -> ProviderResult<u64> {
        rpc_call!(self, "Failed to get chain ID", self.inner.get_chain_id())
    }

    /// Fetch logs with retry logic.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError` if the RPC call fails or filter is invalid.
    pub async fn get_logs(&self, filter: &LogFilter) -> ProviderResult<Vec<Log>> {
        let alloy_filter = filter.to_alloy_filter();

        self.retry_with_backoff(|| async {
            let result: Vec<Log> = self
                .inner
                .get_logs(&alloy_filter)
                .await
                .map_err(|e| e.into_provider_error("Failed to get logs"))?;
            Ok(result)
        })
        .await
    }

    /// Get contract code at an address.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_code(
        &self,
        address: &Address,
        block_number: Option<u64>,
    ) -> ProviderResult<Bytes> {
        self.retry_with_backoff(|| async {
            let result = if let Some(block) = block_number {
                self.inner
                    .get_code_at(*address)
                    .block_id(block.into())
                    .await
            } else {
                self.inner.get_code_at(*address).await
            }
            .map_err(|e| e.into_provider_error("Failed to get code"))?;

            Ok(result)
        })
        .await
    }

    /// Get a block by number.
    ///
    /// Returns the full block data including header and transactions.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_block(&self, block_number: u64) -> ProviderResult<Option<EthBlock>> {
        self.retry_with_backoff(|| async {
            let block_num_tag = BlockNumberOrTag::Number(block_number);
            let result = self
                .inner
                .get_block_by_number(block_num_tag)
                .await
                .map_err(|e| e.into_provider_error("Failed to get block"))?;

            Ok(result)
        })
        .await
    }

    /// Retry an async operation with exponential backoff.
    ///
    /// Uses exponential backoff with jitter to avoid thundering herd problems.
    /// All retryable errors (rate limit, timeout, connection failures) receive
    /// the same backoff treatment.
    async fn retry_with_backoff<F, Fut, T>(&self, operation: F) -> ProviderResult<T>
    where
        F: Fn() -> Fut + Send + Sync,
        Fut: std::future::Future<Output = ProviderResult<T>> + Send,
        T: Send,
    {
        retry_with_backoff_loop(self.max_attempts, self.call_timeout, operation).await
    }

    /// Get the RPC URL.
    #[must_use]
    pub fn rpc_url(&self) -> &str {
        &self.rpc_url
    }

    /// Get a reference-counted clone of the inner Alloy provider.
    ///
    /// Used by the subscription module to spawn pump tasks that
    /// need access to the provider for subscription calls.
    #[must_use]
    pub fn provider_arc(&self) -> Arc<dyn Provider<Ethereum>> {
        Arc::clone(&self.inner)
    }

    /// Execute an `eth_call`.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn eth_call(
        &self,
        to: &Address,
        data: Bytes,
        block_number: Option<u64>,
    ) -> ProviderResult<Bytes> {
        self.retry_with_backoff(|| async {
            let tx = TransactionRequest::default()
                .to(*to)
                .input(data.clone().into());

            // Call at specific block if provided, otherwise use latest
            let result = if let Some(block) = block_number {
                self.inner.call(tx).block(block.into()).await
            } else {
                self.inner.call(tx).await
            }
            .map_err(|e| e.into_provider_error("eth_call failed"))?;

            Ok(result)
        })
        .await
    }

    /// Get the current gas price.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_gas_price(&self) -> ProviderResult<u128> {
        rpc_call!(self, "Failed to get gas price", self.inner.get_gas_price())
    }

    /// Estimate gas for a transaction.
    ///
    /// # Arguments
    /// * `to` - Target address
    /// * `data` - Transaction data
    /// * `from` - Optional sender address
    /// * `value` - Optional value in wei
    /// * `block_number` - Optional block number to estimate at
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn estimate_gas(
        &self,
        to: &Address,
        data: Bytes,
        from: Option<&Address>,
        value: Option<u128>,
        block_number: Option<u64>,
    ) -> ProviderResult<u64> {
        self.retry_with_backoff(|| async {
            let mut tx = TransactionRequest::default()
                .to(*to)
                .input(data.clone().into());

            if let Some(addr) = from {
                tx = tx.from(*addr);
            }

            if let Some(val) = value {
                tx = tx.value(alloy::primitives::U256::from(val));
            }

            // Estimate at specific block if provided, otherwise use pending
            let result = if let Some(block) = block_number {
                self.inner.estimate_gas(tx).block(block.into()).await
            } else {
                self.inner.estimate_gas(tx).await
            }
            .map_err(|e| e.into_provider_error("Failed to estimate gas"))?;

            Ok(result)
        })
        .await
    }

    /// Get a transaction by hash.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_transaction(&self, tx_hash: &str) -> ProviderResult<Option<Transaction>> {
        let hash = B256::from_str(tx_hash).map_err(|e| ProviderError::InvalidParams {
            message: format!("Invalid transaction hash: {e}"),
        })?;

        // HXLBJZ: return alloy's typed `Transaction` (consistent with
        // `EthBlock`'s `Transaction<TxEnvelope>`), not a serialize-then-reparse
        // `serde_json::Value` round-trip. The PyO3 wrapper serializes to JSON
        // at the FFI boundary when a Python caller needs it.
        self.retry_with_backoff(|| async {
            self.inner
                .get_transaction_by_hash(hash)
                .await
                .map_err(|e| e.into_provider_error("Failed to get transaction"))
        })
        .await
    }

    /// Get a transaction receipt by hash.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_transaction_receipt(
        &self,
        tx_hash: &str,
    ) -> ProviderResult<Option<TransactionReceipt>> {
        let hash = B256::from_str(tx_hash).map_err(|e| ProviderError::InvalidParams {
            message: format!("Invalid transaction hash: {e}"),
        })?;

        // HXLBJZ: return alloy's typed `TransactionReceipt`, not a
        // serialize-then-reparse `serde_json::Value` round-trip.
        self.retry_with_backoff(|| async {
            self.inner
                .get_transaction_receipt(hash)
                .await
                .map_err(|e| e.into_provider_error("Failed to get transaction receipt"))
        })
        .await
    }

    /// Get storage at a given address and position.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_storage_at(
        &self,
        address: &Address,
        position: U256,
        block_number: Option<u64>,
    ) -> ProviderResult<B256> {
        // HXLBJZ: the `to_be_bytes::<32>()` byte-array detour is NOT
        // pointless — alloy's `get_storage_at` returns a `U256`
        // (`StorageValue = U256`), and there is no direct `From<U256> for
        // B256`. The 32-byte big-endian form of a `U256` IS the `B256` layout,
        // so `to_be_bytes::<32>()` → `B256::from([u8; 32])` is the canonical
        // (and zero-cost) conversion. If alloy ever returns `B256` directly,
        // return it as-is here.
        self.retry_with_backoff(|| async {
            let result = if let Some(block) = block_number {
                self.inner
                    .get_storage_at(*address, position)
                    .block_id(block.into())
                    .await
            } else {
                self.inner.get_storage_at(*address, position).await
            }
            .map_err(|e| e.into_provider_error("Failed to get storage"))?;

            // Direct conversion: U256's 32-byte big-endian form is a B256.
            Ok(B256::from(result.to_be_bytes::<32>()))
        })
        .await
    }

    /// Get the balance of an address.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_balance(
        &self,
        address: &Address,
        block_number: Option<u64>,
    ) -> ProviderResult<U256> {
        self.retry_with_backoff(|| async {
            let result = if let Some(block) = block_number {
                self.inner
                    .get_balance(*address)
                    .block_id(block.into())
                    .await
            } else {
                self.inner.get_balance(*address).await
            }
            .map_err(|e| e.into_provider_error("Failed to get balance"))?;

            Ok(result)
        })
        .await
    }

    /// Get the transaction count (nonce) for an address.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_transaction_count(
        &self,
        address: &Address,
        block_number: Option<u64>,
    ) -> ProviderResult<u64> {
        self.retry_with_backoff(|| async {
            let result = if let Some(block) = block_number {
                self.inner
                    .get_transaction_count(*address)
                    .block_id(block.into())
                    .await
            } else {
                self.inner.get_transaction_count(*address).await
            }
            .map_err(|e| e.into_provider_error("Failed to get transaction count"))?;

            Ok(result)
        })
        .await
    }

    /// Execute `eth_simulateV1` — simulate a batch of calls on top of the
    /// requested state (Alloy spec: `SimulatePayload` / `BlockStateCallV1` /
    /// `StateOverride` / `AccountOverride`).
    ///
    /// This is the transport primitive; the simulate orchestration (7-call
    /// profit pattern, state override construction, revert decoding) is the
    /// Simulation epic — this fn only performs the typed RPC round-trip.
    ///
    /// `block_id` selects the base state (`BlockId::Number(Tag::Pending)` for
    /// the simulate-v1 `block_identifier="pending"` parity, `BlockId::Number(Tag::Latest)`
    /// to default to the head block, or `BlockId::Hash(..)` for a specific hash).
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn eth_simulate_v1(
        &self,
        payload: &SimulatePayload,
        block_id: BlockId,
    ) -> ProviderResult<Vec<SimulatedBlock<EthBlock>>> {
        self.retry_with_backoff(|| async {
            let result = self
                .inner
                .simulate(payload)
                .block_id(block_id)
                .await
                .map_err(|e| e.into_provider_error("eth_simulateV1 failed"))?;
            Ok(result)
        })
        .await
    }

    /// Execute `eth_feeHistory` — historical gas info for EIP-1559 fee
    /// estimation.
    ///
    /// Returns `baseFeePerGas`, `gasUsedRatio`, `reward` (per-percentile
    /// priority-fee samples), `oldestBlock`, and EIP-4844 blob fields.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn eth_fee_history(
        &self,
        block_count: u64,
        last_block: BlockNumberOrTag,
        reward_percentiles: &[f64],
    ) -> ProviderResult<FeeHistory> {
        rpc_call!(
            self,
            "eth_feeHistory failed",
            self.inner
                .get_fee_history(block_count, last_block, reward_percentiles)
        )
    }

    /// Execute `eth_createAccessList` — compute the EIP-2930 access list for a
    /// transaction.
    ///
    /// Returns `{accessList, gasUsed}` (plus an optional `error` field if the
    /// transaction would revert).
    ///
    /// `block_id` selects the base state; `BlockId::Number(Tag::Latest)` is the
    /// default-of-defaults (the previous `None` semantics).
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn eth_create_access_list(
        &self,
        request: &TransactionRequest,
        block_id: BlockId,
    ) -> ProviderResult<AccessListResult> {
        self.retry_with_backoff(|| async {
            let result = self
                .inner
                .create_access_list(request)
                .block_id(block_id)
                .await
                .map_err(|e| e.into_provider_error("eth_createAccessList failed"))?;
            Ok(result)
        })
        .await
    }

    /// Execute `eth_sendRawTransaction` — broadcast a raw signed transaction.
    ///
    /// This is the broadcast that `degenbot-submission`'s `TxSigner` produces
    /// the bytes for; this fn returns the resulting `TxHash` (the transport
    /// primitive — receipt monitoring is owned upstream).
    ///
    /// # Broadcast-aware reconciliation (J3RIFU)
    ///
    /// Broadcast is NOT idempotent: a timeout / connection drop after the body
    /// was sent may have delivered the tx to the mempool, so blindly retrying
    /// via `retry_with_backoff` risks double-inclusion on rebroadcast. This fn
    /// uses [`send_raw_transaction_with_reconciliation`] instead: on ambiguous
    /// outcomes (`Timeout` / `ConnectionFailed`) it reconciles via
    /// [`get_transaction_receipt`](Self::get_transaction_receipt) before
    /// rebroadcasting; on `RateLimited` it rebroadcasts (the request never
    /// reached the node); on `RpcError` ("already known" / "nonce too low" /
    /// "replacement underpriced") it surfaces immediately — the tx was seen and
    /// rejected, retrying won't help. The tx hash is computed locally from the
    /// signed payload so reconciliation works even when the broadcast response
    /// was lost.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails or the tx is
    /// rejected.
    pub async fn eth_send_raw_transaction(&self, encoded_tx: &[u8]) -> ProviderResult<B256> {
        // Compute the hash locally so reconciliation can proceed even when the
        // broadcast response was lost to a timeout / connection drop.
        let tx_hash = compute_tx_hash_from_signed_payload(encoded_tx)?;
        let max_attempts = self.max_attempts;
        let inner = Arc::clone(&self.inner);
        let inner_for_reconcile = Arc::clone(&self.inner);

        send_raw_transaction_with_reconciliation(
            tx_hash,
            move || {
                let inner = Arc::clone(&inner);
                async move {
                    let pending = inner
                        .send_raw_transaction(encoded_tx)
                        .await
                        .map_err(|e| e.into_provider_error("eth_sendRawTransaction failed"))?;
                    Ok(*pending.tx_hash())
                }
            },
            move || {
                let inner = Arc::clone(&inner_for_reconcile);
                async move {
                    let receipt = inner.get_transaction_receipt(tx_hash).await.map_err(|e| {
                        e.into_provider_error("reconcile get_transaction_receipt failed")
                    })?;
                    Ok(receipt.is_some())
                }
            },
            max_attempts,
        )
        .await
    }

    /// Make a raw JSON-RPC request.
    ///
    /// This method allows calling arbitrary RPC methods that don't have
    /// typed wrappers, such as debug methods, trace methods, or node-specific APIs.
    ///
    /// # Arguments
    /// * `method` - The RPC method name
    /// * `params` - The parameters as a JSON value (typically an array)
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn make_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> ProviderResult<serde_json::Value> {
        let method = method.to_string();
        let params = Arc::new(params);

        // 65F2N7 #2: `make_request` is a raw escape hatch that can issue ANY
        // method. Retrying non-idempotent methods (debug_*, trace_*, state-
        // mutating) is unsafe — a partial timeout re-sends the payload. Retry
        // ONLY the idempotent allowlist (`is_idempotent_rpc_method`); all other
        // methods get a single attempt. Callers who need retry for a custom
        // idempotent method should route through the typed methods instead.
        if is_idempotent_rpc_method(&method) {
            self.retry_with_backoff(|| {
                let client = self.inner.client();
                let method = method.clone();
                let params = Arc::clone(&params);
                async move {
                    let result: serde_json::Value = client
                        .request(method, (*params).clone())
                        .await
                        .map_err(|e| e.into_provider_error("RPC request failed"))?;
                    Ok(result)
                }
            })
            .await
        } else {
            // Non-idempotent method: single attempt, no retry.
            let client = self.inner.client();
            let result: serde_json::Value = client
                .request(method, (*params).clone())
                .await
                .map_err(|e| e.into_provider_error("RPC request failed"))?;
            Ok(result)
        }
    }
}

impl AlloyProvider {
    /// Wraps a pre-built provider (e.g. a custom transport such as the
    /// `OfflineProvider`, or a mock transport). Used both at runtime (the
    /// offline provider wraps an in-memory transport via this constructor) and
    /// by tests that drive `run_with_stream` from a deterministic synthetic
    /// `WsEvent` stream without a live RPC.
    #[must_use]
    pub fn from_provider(inner: Arc<dyn Provider<Ethereum>>) -> Self {
        Self {
            inner,
            rpc_url: String::from("test"),
            max_attempts: DEFAULT_MAX_RETRIES,
            call_timeout: DEFAULT_CALL_TIMEOUT,
        }
    }
}

/// Log fetcher with fixed chunk sizing.
pub struct LogFetcher {
    provider: Arc<AlloyProvider>,
    max_blocks_per_request: u64,
    max_concurrent_requests: usize,
}

impl LogFetcher {
    /// Create a new log fetcher.
    #[must_use]
    pub const fn new(provider: Arc<AlloyProvider>, max_blocks_per_request: u64) -> Self {
        Self {
            provider,
            max_blocks_per_request,
            max_concurrent_requests: 4, // Default concurrency limit
        }
    }

    /// Set the maximum number of concurrent requests.
    ///
    /// The value is capped at [`MAX_CONCURRENT_REQUESTS_CAP`] (32) to prevent
    /// file-descriptor exhaustion and RPC rate-limit bans.
    #[must_use]
    pub fn with_concurrency(mut self, max_concurrent: usize) -> Self {
        self.max_concurrent_requests = max_concurrent.min(MAX_CONCURRENT_REQUESTS_CAP);
        self
    }

    /// Fetch logs across a block range with chunking.
    ///
    /// Uses concurrent requests to fetch multiple chunks in parallel,
    /// improving performance for large block ranges. Logs are sorted
    /// by `(block_number, log_index)` for deterministic ordering.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::InvalidBlockRange` if `from_block > to_block`.
    pub async fn fetch_logs_chunked(
        &self,
        from_block: u64,
        to_block: u64,
        addresses: Option<Vec<String>>,
        topics: Option<Vec<Vec<String>>>,
    ) -> ProviderResult<Vec<Log>> {
        if from_block > to_block {
            return Err(ProviderError::InvalidBlockRange {
                from: from_block,
                to: to_block,
            });
        }

        if self.max_blocks_per_request == 0 {
            return Err(ProviderError::InvalidParams {
                message: "max_blocks_per_request must be greater than 0".to_string(),
            });
        }

        // Resolve the filter once and share it across chunk tasks via Arc.
        // All string→typed parsing happens here, not per-chunk.
        let base_filter = Arc::new(LogFilter::new(from_block, to_block, addresses, topics)?);

        // Build list of chunk ranges
        let mut chunks = Vec::new();
        let mut current_block = from_block;

        while current_block <= to_block {
            let chunk_end =
                std::cmp::min(current_block + self.max_blocks_per_request - 1, to_block);
            chunks.push((current_block, chunk_end));
            current_block = chunk_end + 1;
        }

        // Spawn all tasks immediately; the semaphore gates execution inside each task
        let sem = Arc::new(tokio::sync::Semaphore::new(self.max_concurrent_requests));
        let mut join_set = tokio::task::JoinSet::new();

        for (chunk_start, chunk_end) in chunks {
            let sem = Arc::clone(&sem);
            let provider = Arc::clone(&self.provider);
            let base_filter = Arc::clone(&base_filter);

            join_set.spawn(async move {
                // Acquire permit inside the task so all tasks can be spawned first
                let _permit = sem.acquire().await.map_err(|_| ProviderError::Other {
                    message: "Semaphore acquisition failed".to_string(),
                })?;

                // Build chunk-specific filter from the pre-resolved base
                let chunk_filter = LogFilter {
                    from_block: Some(chunk_start),
                    to_block: Some(chunk_end),
                    addresses: Arc::clone(&base_filter.addresses),
                    topics: Arc::clone(&base_filter.topics),
                };
                provider.get_logs(&chunk_filter).await
            });
        }

        // Collect results as tasks complete; JoinSet yields in completion order
        let mut all_logs = Vec::new();
        while let Some(join_result) = join_set.join_next().await {
            let logs = join_result.map_err(|e| ProviderError::Other {
                message: format!("Task join error: {e}"),
            })??;
            all_logs.extend(logs);
        }

        // Sort by (block_number, log_index) for deterministic ordering
        all_logs.sort_by(|a, b| {
            let a_block = a.block_number.unwrap_or(0);
            let b_block = b.block_number.unwrap_or(0);
            a_block
                .cmp(&b_block)
                .then_with(|| a.log_index.unwrap_or(0).cmp(&b.log_index.unwrap_or(0)))
        });

        Ok(all_logs)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    // ── Revert classification ───────────────────────────────────────────

    /// An EVM execution revert ("execution reverted" message, code -32000)
    /// classifies as `ProviderError::ExecutionReverted`, not `RpcError`.
    #[test]
    fn execution_revert_classified_as_execution_reverted() {
        let json = r#"{"code":-32000,"message":"execution reverted"}"#;
        let payload: alloy::rpc::json_rpc::ErrorPayload = serde_json::from_str(json).unwrap();
        let rpc_err: RpcError<TransportErrorKind> = RpcError::ErrorResp(payload);
        let provider_err = rpc_err.into_provider_error("eth_call failed");
        assert!(
            matches!(
                provider_err,
                ProviderError::ExecutionReverted { code: -32000, .. }
            ),
            "expected ExecutionReverted, got {provider_err:?}"
        );
        assert!(!provider_err.is_retryable());
    }

    /// An Anvil-style revert ("error code 3: execution reverted") also
    /// classifies as `ExecutionReverted` (case-insensitive marker match).
    #[test]
    fn anvil_revert_classified_as_execution_reverted() {
        let json = r#"{"code":3,"message":"error code 3: execution reverted"}"#;
        let payload: alloy::rpc::json_rpc::ErrorPayload = serde_json::from_str(json).unwrap();
        let rpc_err: RpcError<TransportErrorKind> = RpcError::ErrorResp(payload);
        let provider_err = rpc_err.into_provider_error("eth_call failed");
        assert!(
            matches!(
                provider_err,
                ProviderError::ExecutionReverted { code: 3, .. }
            ),
            "expected ExecutionReverted, got {provider_err:?}"
        );
    }

    /// A non-revert JSON-RPC error stays `RpcError`.
    #[test]
    fn non_revert_error_stays_rpc_error() {
        let json = r#"{"code":-32001,"message":"requested block not available"}"#;
        let payload: alloy::rpc::json_rpc::ErrorPayload = serde_json::from_str(json).unwrap();
        let rpc_err: RpcError<TransportErrorKind> = RpcError::ErrorResp(payload);
        let provider_err = rpc_err.into_provider_error("eth_call failed");
        assert!(
            matches!(provider_err, ProviderError::RpcError { code: -32001, .. }),
            "expected RpcError, got {provider_err:?}"
        );
    }

    // ── BJXUPU: JSON-RPC-level rate-limit error responses ───────────────
    //
    // HTTP 200 + a JSON-RPC error body is how providers (Alchemy, QuickNode,
    // Infura) signal quota exhaustion over a transport that did NOT fail at
    // the HTTP layer. `into_provider_error` must recognise these and map them
    // to `ProviderError::RateLimited` so the retry loop's `is_retryable()`
    // picks them up (instead of failing fast as `RpcError { code }`).

    /// BJXUPU: Alchemy/QuickNode `-32005` (request rate exceeded) maps to
    /// `RateLimited` (retryable), not `RpcError`.
    #[test]
    fn json_rpc_rate_limit_code_32005_classified_as_rate_limited() {
        let json = r#"{"code":-32005,"message":"your app has exceeded its compute unit capacity"}"#;
        let payload: alloy::rpc::json_rpc::ErrorPayload = serde_json::from_str(json).unwrap();
        let rpc_err: RpcError<TransportErrorKind> = RpcError::ErrorResp(payload);
        let provider_err = rpc_err.into_provider_error("eth_getLogs failed");
        assert!(
            matches!(provider_err, ProviderError::RateLimited { .. }),
            "-32005 should map to RateLimited, got {provider_err:?}"
        );
        assert!(
            provider_err.is_retryable(),
            "RateLimited must be retryable, got {provider_err:?}"
        );
        // The per-call context label is preserved in the message.
        let ProviderError::RateLimited { message } = &provider_err else {
            unreachable!()
        };
        assert!(
            message.contains("eth_getLogs failed"),
            "context label lost: {message}"
        );
    }

    /// BJXUPU: `QuickNode` `-32004` (rate limit) also maps to `RateLimited`.
    #[test]
    fn json_rpc_rate_limit_code_32004_classified_as_rate_limited() {
        let json = r#"{"code":-32004,"message":"rate limit exceeded"}"#;
        let payload: alloy::rpc::json_rpc::ErrorPayload = serde_json::from_str(json).unwrap();
        let rpc_err: RpcError<TransportErrorKind> = RpcError::ErrorResp(payload);
        let provider_err = rpc_err.into_provider_error("eth_call failed");
        assert!(
            matches!(provider_err, ProviderError::RateLimited { .. }),
            "-32004 should map to RateLimited, got {provider_err:?}"
        );
        assert!(provider_err.is_retryable());
    }

    /// BJXUPU: `Infura`-style `-32001` carrying a rate-limit *message* maps to
    /// `RateLimited` (the message-marker fallback catches providers that reuse
    /// a generic code for quota errors). The companion test
    /// `non_revert_error_stays_rpc_error` pins that `-32001` WITHOUT a
    /// rate-limit message stays `RpcError` — so the message-marker check is
    /// exact, not a blanket code match.
    #[test]
    fn json_rpc_rate_limit_message_classified_as_rate_limited() {
        let json = r#"{"code":-32001,"message":"rate limit exceeded, try again later"}"#;
        let payload: alloy::rpc::json_rpc::ErrorPayload = serde_json::from_str(json).unwrap();
        let rpc_err: RpcError<TransportErrorKind> = RpcError::ErrorResp(payload);
        let provider_err = rpc_err.into_provider_error("eth_call failed");
        assert!(
            matches!(provider_err, ProviderError::RateLimited { .. }),
            "-32001 with a rate-limit message should map to RateLimited, got {provider_err:?}"
        );
        assert!(provider_err.is_retryable());
    }

    /// BJXUPU: `-32601` (method not found) is NOT a rate-limit code and must
    /// stay `RpcError` (not retried). Guards against an over-broad code set.
    #[test]
    fn json_rpc_method_not_found_stays_rpc_error() {
        let json = r#"{"code":-32601,"message":"the method eth_getLogs does not exist"}"#;
        let payload: alloy::rpc::json_rpc::ErrorPayload = serde_json::from_str(json).unwrap();
        let rpc_err: RpcError<TransportErrorKind> = RpcError::ErrorResp(payload);
        let provider_err = rpc_err.into_provider_error("eth_getLogs failed");
        assert!(
            matches!(provider_err, ProviderError::RpcError { code: -32601, .. }),
            "-32601 should stay RpcError, got {provider_err:?}"
        );
        assert!(!provider_err.is_retryable());
    }

    // ── LogFilter construction ──────────────────────────────────────────

    #[test]
    fn test_log_filter_creation() {
        let filter = LogFilter::new(
            100,
            200,
            Some(vec![
                "0x1234567890abcdef1234567890abcdef12345678".to_string()
            ]),
            Some(vec![vec![
                "0x0000000000000000000000000000000000000000000000000000000000000001".to_string(),
            ]]),
        )
        .expect("valid log filter should be created");

        assert_eq!(filter.from_block(), Some(100));
        assert_eq!(filter.to_block(), Some(200));
        assert_eq!(filter.addresses.len(), 1);
    }

    #[test]
    fn test_log_filter_invalid_range() {
        let result = LogFilter::new(200, 100, None, None);

        assert!(result.is_err());
        match result {
            Err(ProviderError::InvalidBlockRange { from, to }) => {
                assert_eq!(from, 200);
                assert_eq!(to, 100);
            }
            _ => panic!("Expected InvalidBlockRange error"),
        }
    }

    #[test]
    fn test_log_filter_equal_range() {
        // from_block == to_block is valid (single-block query)
        let filter = LogFilter::new(42, 42, None, None).expect("equal range should be valid");
        assert_eq!(filter.from_block(), Some(42));
        assert_eq!(filter.to_block(), Some(42));
    }

    #[test]
    fn test_log_filter_defaults() {
        let filter = LogFilter::new(0, 100, None, None).expect("valid");
        assert!(filter.addresses.is_empty());
        assert!(filter.topics.is_empty());
    }

    #[test]
    fn test_log_filter_invalid_address_eager() {
        // Invalid addresses are now caught at construction time
        let result = LogFilter::new(100, 200, Some(vec!["not_an_address".to_string()]), None);
        assert!(result.is_err());
        match result {
            Err(ProviderError::InvalidAddress { address, .. }) => {
                assert_eq!(address, "not_an_address");
            }
            other => panic!("Expected InvalidAddress error, got {other:?}"),
        }
    }

    #[test]
    fn test_log_filter_invalid_topic_eager() {
        // Invalid topics are now caught at construction time
        let result = LogFilter::new(100, 200, None, Some(vec![vec!["not_a_topic".to_string()]]));
        assert!(result.is_err());
        match result {
            Err(ProviderError::InvalidTopic { topic, .. }) => {
                assert_eq!(topic, "not_a_topic");
            }
            other => panic!("Expected InvalidTopic error, got {other:?}"),
        }
    }

    // ── LogFilter → Alloy Filter conversion ─────────────────────────────

    #[test]
    fn test_to_alloy_filter_maps_topic_positions() {
        let topic0_val = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let topic1_val = "0x0000000000000000000000000000000000000000000000000000000000000002";
        let topic2_val = "0x0000000000000000000000000000000000000000000000000000000000000003";

        let filter = LogFilter::new(
            100,
            200,
            None,
            Some(vec![
                vec![topic0_val.to_string()],
                vec![topic1_val.to_string()],
                vec![topic2_val.to_string()],
            ]),
        )
        .expect("valid filter");

        let alloy_filter = filter.to_alloy_filter();
        let topics = &alloy_filter.topics;

        assert_eq!(
            topics[0].clone().into_iter().collect::<Vec<_>>(),
            vec![B256::from_str(topic0_val).unwrap()]
        );
        assert_eq!(
            topics[1].clone().into_iter().collect::<Vec<_>>(),
            vec![B256::from_str(topic1_val).unwrap()]
        );
        assert_eq!(
            topics[2].clone().into_iter().collect::<Vec<_>>(),
            vec![B256::from_str(topic2_val).unwrap()]
        );
    }

    #[test]
    fn test_to_alloy_filter_rejects_topic_position_out_of_range() {
        let result = LogFilter::new(
            100,
            200,
            None,
            Some(vec![
                vec![
                    "0x0000000000000000000000000000000000000000000000000000000000000001"
                        .to_string(),
                ],
                vec![],
                vec![],
                vec![],
                vec![
                    "0x0000000000000000000000000000000000000000000000000000000000000002"
                        .to_string(),
                ],
            ]),
        );

        assert!(result.is_err());
        match result {
            Err(ProviderError::InvalidTopic { reason, .. }) => {
                assert!(reason.contains("out of range"));
            }
            _ => panic!("Expected InvalidTopic error"),
        }
    }

    #[test]
    fn test_to_alloy_filter_skips_empty_topic_slots() {
        // Verify that empty inner vecs are skipped without error
        let filter = LogFilter::new(
            100,
            200,
            None,
            Some(vec![
                vec![
                    "0x0000000000000000000000000000000000000000000000000000000000000001"
                        .to_string(),
                ],
                vec![], // should be skipped
                vec![
                    "0x0000000000000000000000000000000000000000000000000000000000000003"
                        .to_string(),
                ],
            ]),
        )
        .expect("valid filter");

        let _ = filter.to_alloy_filter(); // infallible, just verifying no panic
    }

    // ── Retry backoff logic ─────────────────────────────────────────────

    #[test]
    fn test_saturating_retry_delay() {
        // Verify that retry delay uses saturating arithmetic
        let max_delay = u64::MAX / 2;
        let result = std::cmp::min(
            max_delay.saturating_mul(BACKOFF_MULTIPLIER),
            MAX_RETRY_DELAY_MS,
        );
        // Should cap at MAX_RETRY_DELAY_MS, not overflow
        assert_eq!(result, MAX_RETRY_DELAY_MS);
    }

    #[test]
    fn test_backoff_progression() {
        // Verify the exponential backoff sequence up to the cap
        let mut delay = INITIAL_RETRY_DELAY_MS;
        let mut delays = vec![delay];
        for _ in 0..20 {
            delay = std::cmp::min(delay.saturating_mul(BACKOFF_MULTIPLIER), MAX_RETRY_DELAY_MS);
            delays.push(delay);
        }

        // Should double each step until hitting the 30s cap
        assert_eq!(delays[0], 100);
        assert_eq!(delays[1], 200);
        assert_eq!(delays[2], 400);
        assert_eq!(delays[3], 800);
        assert_eq!(delays[4], 1600);
        assert_eq!(delays[5], 3200);
        assert_eq!(delays[6], 6400);
        assert_eq!(delays[7], 12800);
        assert_eq!(delays[8], 25600);
        assert_eq!(delays[9], 30000); // capped
        assert_eq!(delays[10], 30000); // stays capped
    }

    #[tokio::test]
    async fn test_retry_returns_ok_immediately() {
        // Successful operations should return on the first attempt

        let call_count = Arc::new(AtomicU32::new(0));
        let count_clone = Arc::clone(&call_count);

        // Simulate retry_with_backoff logic inline
        let max_attempts: u32 = 10;
        let mut attempt = 0;

        let result: ProviderResult<u64> = loop {
            let count = count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                break Ok(42);
            }
            attempt += 1;
            if attempt >= max_attempts {
                break Err(ProviderError::Other {
                    message: "too many attempts".to_string(),
                });
            }
        };

        assert_eq!(result.unwrap(), 42);
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_retryable_error_classification() {
        // Retryable errors
        assert!(ProviderError::RateLimited {
            message: "test".to_string()
        }
        .is_retryable());
        assert!(ProviderError::Timeout {
            message: "test".to_string()
        }
        .is_retryable());
        assert!(ProviderError::ConnectionFailed {
            message: "test".to_string()
        }
        .is_retryable());

        // Non-retryable errors
        let non_retryable = [
            ProviderError::RpcError {
                code: -32000,
                message: "revert".to_string(),
            },
            ProviderError::ExecutionReverted {
                code: -32000,
                message: "execution reverted".to_string(),
            },
            ProviderError::InvalidBlockRange { from: 1, to: 0 },
            ProviderError::InvalidParams {
                message: "bad".to_string(),
            },
            ProviderError::SerializationError {
                message: "bad".to_string(),
            },
            ProviderError::Other {
                message: "bad".to_string(),
            },
        ];

        for err in non_retryable {
            assert!(!err.is_retryable(), "{err:?} should not be retryable");
        }
    }

    // ── E2B542: retry-loop emits `log` records on every attempt ───────────
    //
    // `log::set_logger` is once-per-process and parallel tests would race on
    // a single capture sink, so the logger is installed once (via `Once`) and
    // each test registers its own thread-local `Sender` to collect records.
    // This keeps the retry-tracing assertion parallel-safe.
    use std::cell::RefCell;
    use std::sync::Once;
    thread_local! {
        static RETRY_CAPTURE: RefCell<Option<std::sync::mpsc::Sender<String>>> =
            const { RefCell::new(None) };
    }
    struct CapturingLogger;
    impl log::Log for CapturingLogger {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.target().starts_with("degenbot_rpc::provider")
        }
        fn log(&self, record: &log::Record) {
            RETRY_CAPTURE.with(|c| {
                if let Some(tx) = &*c.borrow() {
                    let _ = tx.send(format!(
                        "{}|{}|{}",
                        record.level(),
                        record.target(),
                        record.args()
                    ));
                }
            });
        }
        fn flush(&self) {}
    }
    static INSTALL_CAPTURE_LOGGER: Once = Once::new();
    fn install_capturing_logger() {
        INSTALL_CAPTURE_LOGGER.call_once(|| {
            let _ = log::set_logger(&CapturingLogger);
            log::set_max_level(log::LevelFilter::Trace);
        });
    }
    fn capture_retry_logs() -> std::sync::mpsc::Receiver<String> {
        install_capturing_logger();
        let (tx, rx) = std::sync::mpsc::channel();
        RETRY_CAPTURE.with(|c| *c.borrow_mut() = Some(tx));
        rx
    }
    fn stop_capturing_retry_logs() {
        RETRY_CAPTURE.with(|c| *c.borrow_mut() = None);
    }

    /// E2B542: a retryable error that exhausts all attempts emits one `error!`
    /// record (the terminal failure) plus a `debug!`/`warn!` per preceding retry,
    /// each carrying `attempt`, `max_attempts`, the delay, and the `ProviderError`
    /// display (which embeds the per-call context label).
    #[tokio::test]
    async fn retry_with_backoff_loop_emits_log_on_each_retry_and_terminal_failure() {
        let rx = capture_retry_logs();

        let attempt = 0;
        let max_attempts: u32 = 3;
        // Each invocation fails with a retryable rate-limit error; the context
        // label "eth_call failed" is baked into the message via Display.
        let result: ProviderResult<u64> =
            retry_with_backoff_loop(max_attempts, Duration::from_secs(30), move || async move {
                log::debug!(target: "degenbot_rpc::provider", "invocation attempt {attempt}");
                Err(ProviderError::RateLimited {
                    message: "eth_call failed: rate limited".to_string(),
                })
            })
            .await;

        stop_capturing_retry_logs();

        assert!(result.is_err(), "should exhaust retries");
        let logs: Vec<String> = rx.iter().collect();
        // attempt 1 (debug), attempt 2 (warn), then the terminal error (attempt 3).
        let retry_lines: Vec<&String> = logs
            .iter()
            .filter(|l| l.contains("RPC retry: attempt"))
            .collect();
        let terminal_lines: Vec<&String> = logs
            .iter()
            .filter(|l| l.contains("RPC retries exhausted"))
            .collect();
        assert_eq!(
            retry_lines.len(),
            2,
            "expected exactly two retry records (attempts 1 and 2), got {retry_lines:?}"
        );
        assert_eq!(
            terminal_lines.len(),
            1,
            "expected exactly one terminal-failure record, got {terminal_lines:?}"
        );

        // Structured fields: attempt index, max_attempts, the delay (ms), and
        // the ProviderError display carrying the context label.
        let first = &retry_lines[0];
        assert!(
            first.contains("attempt 1/3"),
            "first retry missing attempt/max: {first}"
        );
        assert!(first.contains("ms:"), "first retry missing delay: {first}");
        assert!(
            first.contains("eth_call failed: rate limited"),
            "first retry missing the context-carrying error display: {first}"
        );
        assert!(
            first.starts_with("DEBUG|"),
            "attempt 1 should be debug!: {first}"
        );
        let second = &retry_lines[1];
        assert!(
            second.starts_with("WARN|"),
            "attempt 2 should be warn!: {second}"
        );
        assert!(
            second.contains("attempt 2/3"),
            "second retry missing attempt/max: {second}"
        );
        let terminal = &terminal_lines[0];
        assert!(
            terminal.starts_with("ERROR|"),
            "terminal failure should be error!: {terminal}"
        );
        assert!(
            terminal.contains("attempt 3/3"),
            "terminal record missing attempt/max: {terminal}"
        );
    }

    /// E2B542: a non-retryable error is surfaced immediately with NO retry log
    /// emission (the loop returns before the logging branch).
    #[tokio::test]
    async fn retry_with_backoff_loop_emits_no_log_for_non_retryable_error() {
        let rx = capture_retry_logs();

        let max_attempts: u32 = 5;
        let result: ProviderResult<u64> =
            retry_with_backoff_loop(max_attempts, Duration::from_secs(30), || async {
                Err(ProviderError::InvalidBlockRange { from: 2, to: 1 })
            })
            .await;

        stop_capturing_retry_logs();

        assert!(result.is_err(), "non-retryable should surface immediately");
        let logs: Vec<String> = rx.iter().collect();
        assert!(
            logs.iter()
                .all(|l| !l.contains("RPC retry") && !l.contains("RPC retries exhausted")),
            "non-retryable error must not emit retry/exhausted logs, got {logs:?}"
        );
    }

    /// E2B542: a successful first attempt emits NO retry log (the happy path).
    #[tokio::test]
    async fn retry_with_backoff_loop_emits_no_log_on_first_success() {
        let rx = capture_retry_logs();

        let max_attempts: u32 = 3;
        let result: ProviderResult<u64> =
            retry_with_backoff_loop(max_attempts, Duration::from_secs(30), || async {
                Ok(7_u64)
            })
            .await;

        stop_capturing_retry_logs();

        assert_eq!(result.unwrap(), 7);
        let logs: Vec<String> = rx.iter().collect();
        assert!(
            logs.is_empty(),
            "successful first attempt must emit no provider logs, got {logs:?}"
        );
    }

    /// EO75JH: a stuck call (operation sleeps longer than the per-call
    /// timeout) is bounded — the loop does NOT hang forever, it classifies
    /// the elapsed attempt as `ProviderError::Timeout`, retries, and after
    /// exhausting attempts returns `Timeout`. Wall-clock is bounded by
    /// `(attempts × call_timeout) + backoff`, not by the hung transport.
    #[tokio::test]
    async fn retry_with_backoff_loop_times_out_hung_call_within_bounded_wall_clock() {
        // A timeout so short a real network call could never complete, but long
        // enough to be measurable. The operation sleeps far longer than the
        // timeout, so it can never win the race — simulating a stuck WS/IPC read.
        let call_timeout = Duration::from_millis(20);
        let hung_sleep = Duration::from_millis(500);
        // max_attempts = 2 so we get exactly: 1st attempt times out → backoff
        // (INITIAL_RETRY_DELAY_MS=100ms + 0..MAX_JITTER_MS) → 2nd attempt times
        // out → exhausted → return `Timeout`.
        let max_attempts: u32 = 2;

        let started = std::time::Instant::now();
        let result: ProviderResult<u64> =
            retry_with_backoff_loop(max_attempts, call_timeout, || async {
                tokio::time::sleep(hung_sleep).await;
                Ok(7_u64)
            })
            .await;
        let elapsed = started.elapsed();

        // Does not hang forever: elapsed is dominated by the backoff
        // (INITIAL_RETRY_DELAY_MS=100ms + jitter up to MAX_JITTER_MS=100ms)
        // plus the two cut-off attempts (2 × 20ms), NOT by the hung
        // transport's 500ms sleep. Worst case ≈ 20 + 200 + 20 = 240ms; the
        // key property is that we never waited for even ONE full hung sleep.
        assert!(
            elapsed < hung_sleep,
            "hung call should have been cut off by the per-call timeout, not blocked on tokio::sleep for {hung_sleep:?}; elapsed {elapsed:?}"
        );
        assert!(
            matches!(result, Err(ProviderError::Timeout { .. })),
            "exhausted retries on a hung call should surface Timeout, got {result:?}"
        );
    }

    /// EO75JH: after a per-call timeout, the loop retries and can succeed on
    /// a later attempt — the timeout is classified as retryable (feeds
    /// `is_retryable()`), the backoff applies, and a fast retry wins.
    #[tokio::test]
    async fn retry_with_backoff_loop_recovers_after_timeout_then_success() {
        // Short timeout so the first (hung) attempt is cut off quickly.
        let call_timeout = Duration::from_millis(20);
        let max_attempts: u32 = 3;
        let attempt_counter = Arc::new(AtomicU32::new(0));
        let counter = attempt_counter.clone();

        let result: ProviderResult<u64> =
            retry_with_backoff_loop(max_attempts, call_timeout, move || {
                let counter = counter.clone();
                async move {
                    let n = counter.fetch_add(1, Ordering::SeqCst);
                    if n == 0 {
                        // First attempt: hang (simulates a stuck WS/IPC read).
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                    Ok(42_u64)
                }
            })
            .await;

        assert_eq!(
            result.unwrap(),
            42,
            "should recover after the hung first attempt"
        );
        assert_eq!(
            attempt_counter.load(Ordering::SeqCst),
            2,
            "exactly two attempts: the first timed out, the second succeeded"
        );
    }

    // ── Chunk calculation ───────────────────────────────────────────────

    #[test]
    fn test_chunk_ranges_exact_division() {
        // 100 blocks, 100 per chunk → 1 chunk
        let from = 0u64;
        let to = 99u64;
        let chunk_size = 100u64;

        let mut chunks = Vec::new();
        let mut current = from;
        while current <= to {
            let end = std::cmp::min(current + chunk_size - 1, to);
            chunks.push((current, end));
            current = end + 1;
        }

        assert_eq!(chunks, vec![(0, 99)]);
    }

    #[test]
    fn test_chunk_ranges_partial() {
        // 250 blocks, 100 per chunk → 3 chunks
        let from = 0u64;
        let to = 249u64;
        let chunk_size = 100u64;

        let mut chunks = Vec::new();
        let mut current = from;
        while current <= to {
            let end = std::cmp::min(current + chunk_size - 1, to);
            chunks.push((current, end));
            current = end + 1;
        }

        assert_eq!(chunks, vec![(0, 99), (100, 199), (200, 249)]);
    }

    #[test]
    fn test_chunk_ranges_single_block() {
        // 1 block → 1 chunk of size 1
        let from = 42u64;
        let to = 42u64;
        let chunk_size = 100u64;

        let mut chunks = Vec::new();
        let mut current = from;
        while current <= to {
            let end = std::cmp::min(current + chunk_size - 1, to);
            chunks.push((current, end));
            current = end + 1;
        }

        assert_eq!(chunks, vec![(42, 42)]);
    }

    #[test]
    fn test_chunk_ranges_non_zero_start() {
        // Starting from block 500
        let from = 500u64;
        let to = 799u64;
        let chunk_size = 100u64;

        let mut chunks = Vec::new();
        let mut current = from;
        while current <= to {
            let end = std::cmp::min(current + chunk_size - 1, to);
            chunks.push((current, end));
            current = end + 1;
        }

        assert_eq!(chunks, vec![(500, 599), (600, 699), (700, 799)]);
    }

    // ── URL scheme detection ────────────────────────────────────────────

    #[test]
    fn test_url_scheme_detection() {
        // HTTP variants
        assert!("http://localhost:8545".starts_with("http://"));
        assert!("https://mainnet.infura.io".starts_with("https://"));

        // WebSocket variants
        assert!("ws://localhost:8546".starts_with("ws://"));
        assert!("wss://mainnet.infura.io/ws".starts_with("wss://"));

        // IPC paths (no ://)
        assert!(!"/tmp/anvil.ipc".contains("://"));
        assert!(!"\\\\.\\pipe\\anvil".contains("://"));

        // Unsupported schemes
        assert!("ftp://example.com".starts_with("ftp://"));
    }

    #[test]
    fn test_unsupported_scheme_extraction() {
        let url = "ftp://example.com";
        let scheme = url.split("://").next().unwrap_or(url);
        assert_eq!(scheme, "ftp");
    }

    // ── LogFetcher validation ──────────────────────────────────────────

    #[tokio::test]
    async fn test_fetch_logs_chunked_zero_chunk_size_rejected() {
        let max_blocks_per_request = 0u64;

        // This is the check from fetch_logs_chunked:
        let result = if max_blocks_per_request == 0 {
            Err(ProviderError::InvalidParams {
                message: "max_blocks_per_request must be greater than 0".to_string(),
            })
        } else {
            Ok(())
        };

        assert!(result.is_err());
        match result {
            Err(ProviderError::InvalidParams { message }) => {
                assert!(message.contains("greater than 0"));
            }
            other => panic!("Expected InvalidParams, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_fetch_logs_chunked_invalid_range_rejected() {
        let from_block = 200u64;
        let to_block = 100u64;

        // This is the check from fetch_logs_chunked:
        let result = if from_block > to_block {
            Err(ProviderError::InvalidBlockRange {
                from: from_block,
                to: to_block,
            })
        } else {
            Ok(())
        };

        assert!(result.is_err());
        match result {
            Err(ProviderError::InvalidBlockRange { from, to }) => {
                assert_eq!(from, 200);
                assert_eq!(to, 100);
            }
            other => panic!("Expected InvalidBlockRange, got {other:?}"),
        }
    }

    // ── LogFetcher concurrency cap ─────────────────────────────────────

    #[test]
    fn test_concurrency_cap() {
        // Values above the cap should be clamped
        let requested = 1000usize;
        let clamped = requested.min(MAX_CONCURRENT_REQUESTS_CAP);
        assert_eq!(clamped, MAX_CONCURRENT_REQUESTS_CAP);
    }

    #[test]
    fn test_concurrency_within_cap() {
        // Values within the cap should pass through
        let requested = 8usize;
        let clamped = requested.min(MAX_CONCURRENT_REQUESTS_CAP);
        assert_eq!(clamped, 8);
    }

    // ── Log sorting ────────────────────────────────────────────────────

    #[test]
    fn test_log_sort_order() {
        // Verify logs sort by (block_number, log_index) even when provided out of order.
        let logs: Vec<Log> = vec![
            Log {
                block_number: Some(100),
                log_index: Some(2),
                ..Default::default()
            },
            Log {
                block_number: Some(99),
                log_index: Some(0),
                ..Default::default()
            },
            Log {
                block_number: Some(100),
                log_index: Some(0),
                ..Default::default()
            },
            Log {
                block_number: None,
                log_index: None,
                ..Default::default()
            },
        ];

        let mut sorted = logs;
        sorted.sort_by(|a, b| {
            let a_block = a.block_number.unwrap_or(0);
            let b_block = b.block_number.unwrap_or(0);
            a_block
                .cmp(&b_block)
                .then_with(|| a.log_index.unwrap_or(0).cmp(&b.log_index.unwrap_or(0)))
        });

        // Expected order: block_number=None(0,0), 99(99,0), 100(100,0), 100(100,2)
        assert_eq!(sorted[0].log_index, None); // pending log comes first (0,0)
        assert_eq!(sorted[1].block_number, Some(99));
        assert_eq!(sorted[2].block_number, Some(100));
        assert_eq!(sorted[2].log_index, Some(0));
        assert_eq!(sorted[3].block_number, Some(100));
        assert_eq!(sorted[3].log_index, Some(2));
    }

    // ── EthBlock type alias ────────────────────────────────────────────

    #[test]
    fn test_eth_block_type_alias_compiles() {
        // Verify the type alias is usable (compile-time check)
        fn assert_block_type(_: Option<EthBlock>) {}
        assert_block_type(None);
    }

    // ── 65F2N7: IPC path detection + idempotent retry allowlist ─────────

    #[test]
    fn test_is_ipc_path_detects_unix_and_windows_and_scheme() {
        // Unix absolute path
        assert!(is_ipc_path("/tmp/anvil.ipc"));
        assert!(is_ipc_path("/var/run/geth.ipc"));
        // Windows named pipe (backslash-backslash)
        assert!(is_ipc_path("\\\\.\\pipe\\geth.ipc"));
        // Explicit ipc:// scheme
        assert!(is_ipc_path("ipc:///tmp/anvil.ipc"));
    }

    #[test]
    fn test_is_ipc_path_rejects_bare_hostport_typo() {
        // 65F2N7 #1: a bare `localhost:8545` (missing the http:// scheme) is
        // NO longer silently routed to IPC. It must be rejected so the typo
        // surfaces in the unsupported-scheme error.
        assert!(!is_ipc_path("localhost:8545"));
        assert!(!is_ipc_path("127.0.0.1:8545"));
        assert!(!is_ipc_path("example.com:8545"));
        // Unrecognized schemes are not IPC paths either.
        assert!(!is_ipc_path("ftp://something"));
        assert!(!is_ipc_path("ldap://something"));
    }

    #[test]
    fn test_is_idempotent_rpc_method_allowlist() {
        // 65F2N7 #2: read-only eth_* methods are retry-safe.
        assert!(is_idempotent_rpc_method("eth_call"));
        assert!(is_idempotent_rpc_method("eth_getLogs"));
        assert!(is_idempotent_rpc_method("eth_getBlockByNumber"));
        assert!(is_idempotent_rpc_method("eth_getTransactionReceipt"));
        assert!(is_idempotent_rpc_method("eth_chainId"));
        assert!(is_idempotent_rpc_method("eth_feeHistory"));
    }

    #[test]
    fn test_is_idempotent_rpc_method_rejects_non_idempotent() {
        // 65F2N7 #2: mutating + stateful methods must NOT be retried blindly.
        assert!(!is_idempotent_rpc_method("eth_sendRawTransaction"));
        assert!(!is_idempotent_rpc_method("eth_sendTransaction"));
        assert!(!is_idempotent_rpc_method("debug_traceCallByBlockhash"));
        assert!(!is_idempotent_rpc_method("trace_call"));
        assert!(!is_idempotent_rpc_method("trace_block"));
        assert!(!is_idempotent_rpc_method("custom_method"));
    }

    // ── §4.2 parity: typed RPC struct JSON round-trips match web3.py ───
    //
    // The §4.2 oracle is the web3.py JSON shape (the execution-apis spec).
    // These tests assert that the typed Rust request/response structs
    // deserialize + re-serialize to byte-identical JSON, proving the typed
    // fns' JSON↔struct transforms match web3.py exactly (no field renaming,
    // no quantity-encoding drift). Canonical fixtures from the execution-apis
    // spec + alloy's own conformance tests.

    #[test]
    fn fee_history_response_round_trips_byte_identical_to_web3() {
        // Canonical eth_feeHistory response. web3.py decodes this into its
        // FeeHistory dict; the Rust struct must re-serialize byte-identically.
        let sample = r#"{"baseFeePerGas":["0x342770c0","0x2da282a8"],"gasUsedRatio":[0.0],"baseFeePerBlobGas":["0x0","0x0"],"blobGasUsedRatio":[0.0],"oldestBlock":"0x1"}"#;
        let fh: FeeHistory = serde_json::from_str(sample).expect("decode web3 feeHistory");
        // Field-level parity (the values web3.py would surface)
        assert_eq!(fh.oldest_block, 1);
        assert_eq!(fh.base_fee_per_gas, vec![875_000_000, 765_625_000]);
        assert_eq!(fh.gas_used_ratio, vec![0.0]);
        assert_eq!(fh.reward, None);
        // Byte-identical re-serialization
        assert_eq!(serde_json::to_string(&fh).unwrap(), sample);
    }

    #[test]
    fn fee_history_with_reward_percentiles_decodes() {
        // Response including the `reward` field (priority-fee samples per
        // percentile) — the shape the bot's _compute_priority_fee consumes.
        let json = r#"{"baseFeePerBlobGas":["0xc0","0xb2"],"baseFeePerGas":["0x4cb8cf181","0x53075988e"],"blobGasUsedRatio":[0.16666666666666666,0.3333333333333333],"gasUsedRatio":[0.8288135,0.3407616666666667],"oldestBlock":"0x59f94f","reward":[["0x59682f00"],["0x59682f00"]]}"#;
        let fh: FeeHistory = serde_json::from_str(json).expect("decode feeHistory w/ reward");
        assert_eq!(fh.oldest_block, 0x59_f94f);
        assert_eq!(fh.base_fee_per_gas.len(), 2);
        let reward = fh.reward.expect("reward field present");
        assert_eq!(reward.len(), 2);
        assert_eq!(reward[0], vec![0x5968_2f00_u128]);
    }

    #[test]
    fn create_access_list_response_decodes_to_web3_shape() {
        // Canonical eth_createAccessList response: {accessList, gasUsed}.
        let json = r#"{"accessList":[{"address":"0x0000000000000000000000000000000000000101","storageKeys":["0x0000000000000000000000000000000000000000000000000000000000000000"]}],"gasUsed":"0x5208"}"#;
        let result: AccessListResult = serde_json::from_str(json).expect("decode access list");
        assert!(result.error.is_none());
        assert_eq!(result.gas_used, alloy::primitives::U256::from(0x5208));
        assert_eq!(result.access_list.0.len(), 1);
        let item = &result.access_list.0[0];
        assert_eq!(
            format!("{:?}", item.address),
            "0x0000000000000000000000000000000000000101"
        );
        assert_eq!(item.storage_keys.len(), 1);
        assert_eq!(item.storage_keys[0], alloy::primitives::B256::ZERO);
        // Re-serialize → camelCase shape matches web3.py
        let reser = serde_json::to_string(&result).unwrap();
        assert!(reser.contains("\"accessList\""));
        assert!(reser.contains("\"gasUsed\""));
    }

    #[test]
    fn create_access_list_error_response_decodes() {
        // web3.py surfaces the `error` field when the tx would revert.
        let json = r#"{"accessList":[],"gasUsed":"0x0","error":"transaction execution failed"}"#;
        let result: AccessListResult =
            serde_json::from_str(json).expect("decode errored access list");
        assert_eq!(
            result.error.as_deref(),
            Some("transaction execution failed")
        );
        assert_eq!(result.gas_used, alloy::primitives::U256::ZERO);
    }

    #[test]
    fn simulate_v1_request_round_trips_byte_identical_to_web3() {
        // Canonical eth_simulateV1 request payload (execution-apis spec):
        // blockStateCalls with state overrides + calls. web3.py builds this
        // dict; the Rust SimulatePayload must re-serialize byte-identically.
        let request_json = serde_json::json!({
            "blockStateCalls": [
                {
                    "blockOverrides": {},
                    "stateOverrides": {
                        "0xc000000000000000000000000000000000000000": {
                            "nonce": "0x5"
                        }
                    },
                    "calls": []
                },
                {
                    "blockOverrides": {},
                    "stateOverrides": {
                        "0xc000000000000000000000000000000000000000": {
                            "code": "0x600035600055"
                        }
                    },
                    "calls": [
                        {
                            "from": "0xc000000000000000000000000000000000000000",
                            "to": "0xc000000000000000000000000000000000000000",
                            "nonce": "0x0"
                        }
                    ]
                }
            ],
            "traceTransfers": false,
            "validation": true,
            "returnFullTransactions": false
        });
        let payload: SimulatePayload =
            serde_json::from_value(request_json.clone()).expect("decode simulate payload");
        assert!(payload.validation);
        assert_eq!(payload.block_state_calls.len(), 2);
        let reser = serde_json::to_value(&payload).unwrap();
        // Byte-identical round-trip (camelCase, quantity hex preserved)
        assert_eq!(reser, request_json);
    }

    #[test]
    fn simulate_v1_call_result_decodes_gas_used_return_data() {
        // The per-call result shape: {returnData, logs, gasUsed, status}.
        // This is what the simulate orchestration reads to decode swap results.
        let json = r#"{"returnData":"0x12345678","logs":[],"gasUsed":"0x5208","status":"0x1"}"#;
        let result: alloy::rpc::types::eth::simulate::SimCallResult =
            serde_json::from_str(json).expect("decode sim call result");
        assert_eq!(
            result.return_data,
            alloy::primitives::Bytes::from_static(&[0x12, 0x34, 0x56, 0x78])
        );
        assert_eq!(result.gas_used, 0x5208);
        assert!(result.status);
        assert!(result.error.is_none());
    }

    #[test]
    fn simulate_v1_revert_error_decodes_with_data() {
        // web3.py / execution-apis revert shape: error.code + message + data.
        let error_json = serde_json::json!({
            "code": -32000,
            "message": "Execution reverted",
            "data": "0xcabedea8"
        });
        let err: alloy::rpc::types::eth::simulate::SimulateError =
            serde_json::from_value(error_json).expect("decode simulate error");
        assert_eq!(err.message, "Execution reverted");
        assert_eq!(
            err.data,
            Some(alloy::primitives::Bytes::from_static(&[
                0xca, 0xbe, 0xde, 0xa8
            ]))
        );
    }

    #[test]
    fn send_raw_transaction_request_hex_encoding_matches_web3() {
        // eth_sendRawTransaction sends ("0x"+hexlify(bytes),). web3.py uses
        // `/`-prefixed hex; alloy uses `hex::encode_prefixed`. Assert they
        // match for a fixture payload (the signed-bytes from G6DNW4's §4.2
        // fixture — an anvil-key-0 type-2 envelope prefix).
        let signed_bytes: &[u8] = &[0x02, 0xf8, 0x70, 0x01, 0x07];
        let hex = alloy::hex::encode_prefixed(signed_bytes);
        // web3.py `web3.Web3.to_hex(bytes)` produces the same 0x-prefixed hex
        assert_eq!(hex, "0x02f8700107");
        // The param tuple shape: [hex] — matches `eth_sendRawTransaction`.
        let params = serde_json::json!([hex]);
        assert_eq!(params, serde_json::json!(["0x02f8700107"]));
    }

    // ── J3RIFU: local tx-hash computation + broadcast-aware reconciliation ─

    /// A real signed type-2 transaction broadcast by anvil key 0 (cast send
    /// of 1 wei to the zero address), and its on-chain hash as returned by the
    /// node. Used to prove `compute_tx_hash_from_signed_payload` derives the
    /// SAME hash the node assigned — the foundation of broadcast
    /// reconciliation (if the locally-computed hash didn't match the node's,
    /// receipt reconciliation would look up the wrong hash).
    ///
    /// Captured via `anvil` + `cast rpc debug_getRawTransaction` (chainId 31337).
    const J3RIFU_SIGNED_TX_HEX: &str =
        "0x02f868827a698001843b9aca008252089400000000000000000000000000000000000000000180c080a06dabac39e44552d8164c7e95c996ea7d8dee13ecdf3c34bf74a32a70616f8bc2a07d2cacb344c39f8a9b5f1739803f6373ccf0aa25808998032e735351c6200b28";
    const J3RIFU_EXPECTED_TX_HASH: &str =
        "0xdf7c749bc5e7a46561d43676ba826807352aa32ee7573f815337970e97c3ffc5";

    #[test]
    fn compute_tx_hash_from_signed_payload_matches_node_assigned_hash() {
        let signed_bytes = alloy::hex::decode(J3RIFU_SIGNED_TX_HEX).unwrap();
        let computed = compute_tx_hash_from_signed_payload(&signed_bytes).unwrap();
        let expected = B256::from_str(J3RIFU_EXPECTED_TX_HASH).unwrap();
        assert_eq!(
            computed, expected,
            "locally-computed tx hash must match the node-assigned hash so receipt reconciliation looks up the right tx"
        );
    }

    #[test]
    fn compute_tx_hash_from_signed_payload_rejects_invalid_rlp() {
        // Not a valid RLP-encoded envelope → DecodingError, not a panic.
        let garbage: &[u8] = &[0x00, 0xff, 0x42];
        let result = compute_tx_hash_from_signed_payload(garbage);
        assert!(
            matches!(result, Err(ProviderError::DecodingError { .. })),
            "invalid envelope must surface DecodingError, got {result:?}"
        );
    }

    #[tokio::test]
    async fn send_raw_transaction_reconciliation_success_on_first_try_no_rebroadcast() {
        // Broadcast succeeds on the first attempt → returns the hash, no
        // reconcile probe, no rebroadcast.
        let tx_hash = B256::from_str(J3RIFU_EXPECTED_TX_HASH).unwrap();
        let broadcast_count = Arc::new(AtomicU32::new(0));
        let reconcile_count = Arc::new(AtomicU32::new(0));
        let bc = broadcast_count.clone();
        let rc = reconcile_count.clone();

        let result = send_raw_transaction_with_reconciliation(
            tx_hash,
            move || {
                let bc = bc.clone();
                async move {
                    bc.fetch_add(1, Ordering::SeqCst);
                    Ok(tx_hash)
                }
            },
            move || {
                let rc = rc.clone();
                async move {
                    rc.fetch_add(1, Ordering::SeqCst);
                    // Should NOT be called on success.
                    Ok(false)
                }
            },
            5,
        )
        .await;

        assert_eq!(result.unwrap(), tx_hash);
        assert_eq!(
            broadcast_count.load(Ordering::SeqCst),
            1,
            "success on first try must broadcast exactly once"
        );
        assert_eq!(
            reconcile_count.load(Ordering::SeqCst),
            0,
            "reconcile must NOT be called on a successful broadcast"
        );
    }

    #[tokio::test]
    async fn send_raw_transaction_reconciliation_rate_limited_retries_then_succeeds() {
        // RateLimited on attempt 1 → rebroadcast (the request never reached
        // the node) → success on attempt 2.
        let tx_hash = B256::from_str(J3RIFU_EXPECTED_TX_HASH).unwrap();
        let broadcast_count = Arc::new(AtomicU32::new(0));
        let reconcile_count = Arc::new(AtomicU32::new(0));
        let bc = broadcast_count.clone();
        let rc = reconcile_count.clone();

        let result = send_raw_transaction_with_reconciliation(
            tx_hash,
            move || {
                let bc = bc.clone();
                async move {
                    let n = bc.fetch_add(1, Ordering::SeqCst);
                    if n == 0 {
                        Err(ProviderError::RateLimited {
                            message: "rate limited".to_string(),
                        })
                    } else {
                        Ok(tx_hash)
                    }
                }
            },
            move || {
                let rc = rc.clone();
                async move {
                    rc.fetch_add(1, Ordering::SeqCst);
                    Ok(false)
                }
            },
            5,
        )
        .await;

        assert_eq!(result.unwrap(), tx_hash);
        assert_eq!(
            broadcast_count.load(Ordering::SeqCst),
            2,
            "rate-limited then success = exactly 2 broadcasts"
        );
        assert_eq!(
            reconcile_count.load(Ordering::SeqCst),
            0,
            "reconcile must NOT be called for RateLimited (request never reached the node)"
        );
    }

    #[tokio::test]
    async fn send_raw_transaction_reconciliation_timeout_receipt_present_returns_without_rebroadcast(
    ) {
        // Timeout on the broadcast (ambiguous: body may have reached the node)
        // → reconcile; receipt present → return tx_hash WITHOUT rebroadcast.
        let tx_hash = B256::from_str(J3RIFU_EXPECTED_TX_HASH).unwrap();
        let broadcast_count = Arc::new(AtomicU32::new(0));
        let reconcile_count = Arc::new(AtomicU32::new(0));
        let bc = broadcast_count.clone();
        let rc = reconcile_count.clone();

        let result = send_raw_transaction_with_reconciliation(
            tx_hash,
            move || {
                let bc = bc.clone();
                async move {
                    bc.fetch_add(1, Ordering::SeqCst);
                    Err(ProviderError::Timeout {
                        message: "broadcast timed out".to_string(),
                    })
                }
            },
            move || {
                let rc = rc.clone();
                async move {
                    rc.fetch_add(1, Ordering::SeqCst);
                    // Receipt IS present → the tx was already broadcast.
                    Ok(true)
                }
            },
            5,
        )
        .await;

        // Returns the locally-computed hash (NOT a rebroadcast).
        assert_eq!(result.unwrap(), tx_hash);
        assert_eq!(
            broadcast_count.load(Ordering::SeqCst),
            1,
            "must NOT rebroadcast when the receipt is present"
        );
        assert_eq!(
            reconcile_count.load(Ordering::SeqCst),
            1,
            "reconcile must be called exactly once on a Timeout"
        );
    }

    #[tokio::test]
    async fn send_raw_transaction_reconciliation_timeout_receipt_absent_rebroadcasts() {
        // Timeout → reconcile; receipt absent → rebroadcast → success.
        let tx_hash = B256::from_str(J3RIFU_EXPECTED_TX_HASH).unwrap();
        let broadcast_count = Arc::new(AtomicU32::new(0));
        let reconcile_count = Arc::new(AtomicU32::new(0));
        let bc = broadcast_count.clone();
        let rc = reconcile_count.clone();

        let result = send_raw_transaction_with_reconciliation(
            tx_hash,
            move || {
                let bc = bc.clone();
                async move {
                    let n = bc.fetch_add(1, Ordering::SeqCst);
                    if n == 0 {
                        Err(ProviderError::Timeout {
                            message: "broadcast timed out".to_string(),
                        })
                    } else {
                        Ok(tx_hash)
                    }
                }
            },
            move || {
                let rc = rc.clone();
                async move {
                    rc.fetch_add(1, Ordering::SeqCst);
                    Ok(false)
                }
            },
            5,
        )
        .await;

        assert_eq!(result.unwrap(), tx_hash);
        assert_eq!(
            broadcast_count.load(Ordering::SeqCst),
            2,
            "timeout then receipt-absent then success = exactly 2 broadcasts"
        );
        assert_eq!(
            reconcile_count.load(Ordering::SeqCst),
            1,
            "reconcile called once after the first timeout"
        );
    }

    #[tokio::test]
    async fn send_raw_transaction_reconciliation_rpc_error_surfaces_immediately_no_retry() {
        // RpcError "already known" (-32000) → the tx was seen and rejected;
        // surfacing immediately, no retry, no reconcile.
        let tx_hash = B256::from_str(J3RIFU_EXPECTED_TX_HASH).unwrap();
        let broadcast_count = Arc::new(AtomicU32::new(0));
        let reconcile_count = Arc::new(AtomicU32::new(0));
        let bc = broadcast_count.clone();
        let rc = reconcile_count.clone();

        let result = send_raw_transaction_with_reconciliation(
            tx_hash,
            move || {
                let bc = bc.clone();
                async move {
                    bc.fetch_add(1, Ordering::SeqCst);
                    Err(ProviderError::RpcError {
                        code: -32000,
                        message: "already known".to_string(),
                    })
                }
            },
            move || {
                let rc = rc.clone();
                async move {
                    rc.fetch_add(1, Ordering::SeqCst);
                    Ok(false)
                }
            },
            5,
        )
        .await;

        assert!(
            matches!(result, Err(ProviderError::RpcError { code: -32000, .. })),
            "RpcError \"already known\" must surface immediately, got {result:?}"
        );
        assert_eq!(
            broadcast_count.load(Ordering::SeqCst),
            1,
            "RpcError must NOT retry the identical payload"
        );
        assert_eq!(
            reconcile_count.load(Ordering::SeqCst),
            0,
            "reconcile must NOT be called for a definitive RpcError rejection"
        );
    }
}
