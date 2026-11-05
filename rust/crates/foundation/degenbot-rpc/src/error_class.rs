//! Error classification: how a transport/JSON-RPC failure becomes a typed
//! [`ProviderError`], and which provider responses mean "rate limited".
//!
//! Three decisions live here, each one a predicate the rest of the crate calls
//! instead of re-deriving: a DNS verdict that backoff cannot heal, a JSON-RPC
//! code/message pair that marks quota exhaustion over HTTP 200, and the
//! `RpcError` -> `ProviderError` mapping itself.

use alloy::transports::{RpcError, TransportErrorKind};
use degenbot_core::errors::ProviderError;

/// The resolver's verdict at boot is final: a lookup failure will not heal
/// after backoff, so the retry budget must not be spent on it — fail fast and
/// name the endpoint instead.
pub(crate) fn is_unresolvable_host<E: std::fmt::Display>(error: &E) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    text.contains("failed to lookup address information")
        || text.contains("name or service not known")
        || text.contains("no such host")
        || text.contains("nodename nor servname")
}
/// JSON-RPC error codes that providers use to signal request-rate / quota
/// exhaustion over HTTP 200 (the transport did NOT fail at the HTTP layer,
/// so the 429-based classification in [`IntoProviderError`] does not fire).
///
/// Conservative set:
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
/// (`is_retryable()` already returns true for `RateLimited`, so no
/// predicate change is needed.)
pub(crate) fn is_json_rpc_rate_limit_response(code: i64, message: &str) -> bool {
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
pub(crate) trait IntoProviderError {
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
            // JSON-RPC-level rate-limit responses. Providers signal
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
