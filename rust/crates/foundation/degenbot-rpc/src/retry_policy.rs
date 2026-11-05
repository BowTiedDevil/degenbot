//! `degenbot-rpc`'s retry policy: the backoff curve its call sites spend, the
//! cumulative budget that bounds a chain's wall clock, and the two loops that
//! walk them (a per-call retry and the broadcast reconciliation).
//!
//! The policy TYPE is [`degenbot_core::retry::RetryPolicy`] — the workspace
//! owner of attempts, exponential growth, the cap and the jitter bound. This
//! module owns only what the rpc seam adds on top: the curve's constants, the
//! jitter draw, the budget arithmetic, and the loops. Error classification is
//! deliberately not here: each call site classifies through
//! [`crate::error_class`], and the retryability predicate stays on
//! `ProviderError`.

use std::time::Duration;

use alloy::primitives::B256;
use degenbot_core::diag;
use degenbot_core::errors::{ProviderError, ProviderResult};
use degenbot_core::retry::RetryPolicy;
use degenbot_core::{op_error, op_warn};
use rand::RngExt;

/// Resolve the config-declared cumulative retry budget
/// (`nodes.retry_budget_ms`): the wall-clock ONE call may spend retrying.
///
/// `None` (unset) is the production default and means no cumulative cap — the
/// schedule's own arithmetic bounds the wait, which for the default ceiling is
/// 81.1s of backoff per call. Constructors snapshot this value, so a provider
/// keeps the posture it was built with rather than reading live config on the
/// hot retry path.
pub(crate) fn configured_retry_budget() -> Option<Duration> {
    ::degenbot_config::holder::config()
        .nodes
        .retry_budget_ms
        .map(Duration::from_millis)
}

/// Constants for retry logic. `pub(crate)` so the subscription watchdog
/// ([`crate::subscription::pump_blocks`]) reuses the same backoff curve for
/// its reconnect retries — one retry vocabulary across the crate, not a
/// duplicate per-module tuning.
pub(crate) const INITIAL_RETRY_DELAY_MS: u64 = 100;
pub(crate) const MAX_RETRY_DELAY_MS: u64 = 30_000; // 30 seconds
const MAX_JITTER_MS: u64 = 100; // Add up to 100ms of jitter

/// The provider's retry policy for `max_attempts` total attempts, built on
/// the workspace-canonical [`RetryPolicy`] . The exponential
/// growth (x2, saturating at the cap) lives in the shared type — not a
/// hand-rolled multiplier at each loop.
///
/// The curve's arithmetic is the reason callers get a budget knob rather than
/// only an attempt count: from 100ms doubling to a 30s cap, the driver's
/// 11-attempt ceiling sleeps 81.1s in backoff alone
/// ([`RetryPolicy::cumulative_backoff`]) before a single call fails. An attempt
/// count therefore does not bound wall clock, and
/// [`configured_retry_budget`] is what a caller with its own deadline sets.
#[must_use]
pub(crate) fn rpc_retry_policy(max_attempts: u32) -> RetryPolicy {
    RetryPolicy::from_millis(
        max_attempts,
        INITIAL_RETRY_DELAY_MS,
        MAX_RETRY_DELAY_MS,
        MAX_JITTER_MS,
    )
}

/// The backoff for one retry: the capped exponential curve owned by the shared
/// policy plus this site's uniform jitter draw (uses `random_range`, avoiding
/// modulo bias).
pub(crate) fn jittered_backoff(policy: &RetryPolicy, retry: u32) -> Duration {
    policy.capped_backoff(retry) + Duration::from_millis(rand::rng().random_range(0..MAX_JITTER_MS))
}

/// The distinguishable terminal error for a retry chain that ran out of
/// budget rather than attempts.
///
/// `None` means the chain may keep retrying: no budget is configured (the
/// default, where the schedule's own arithmetic is the only bound) or the next
/// backoff still lands inside it. `Some` carries the error the caller
/// surfaces — never the bare transport failure it replaces, so a budget
/// refusal can never be read as "the endpoint returned 429 once".
///
/// The decision includes the backoff about to be slept, so the loop never
/// schedules a sleep that would land past the budget: the wait is bounded by
/// the budget itself, not by the budget plus one attempt's duration.
pub(crate) fn retry_budget_exhausted(
    retry_budget: Option<Duration>,
    started: std::time::Instant,
    attempt: u32,
    next_backoff: Duration,
    last_error: &ProviderError,
) -> Option<ProviderError> {
    let budget = retry_budget?;
    let elapsed = started.elapsed();
    if elapsed + next_backoff < budget {
        return None;
    }
    Some(ProviderError::RetryBudgetExhausted {
        attempt,
        elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        budget_ms: u64::try_from(budget.as_millis()).unwrap_or(u64::MAX),
        last_error: last_error.to_string(),
    })
}

/// Advance a broadcast retry chain by one retry: consume an attempt, then wait
/// the jittered backoff that attempt earned — unless the chain is out of
/// attempts or the backoff would land past the retry budget, in which case the
/// terminal error comes back for the caller to surface.
///
/// One home for the broadcast loop's three backoff sites: the attempt ceiling
/// and the budget are two different stops, and both must be logged and
/// surfaced the same way wherever the chain reached them.
pub(crate) async fn broadcast_retry_step(
    policy: &RetryPolicy,
    retry_budget: Option<Duration>,
    started: std::time::Instant,
    attempt: &mut u32,
    max_attempts: u32,
    last_error: &ProviderError,
) -> Option<ProviderError> {
    *attempt += 1;
    if *attempt >= max_attempts {
        op_error!(domain = rpc, attempt = *attempt,
            max_attempts,
            %last_error,
            "eth_sendRawTransaction: retries exhausted"
        );
        return Some(last_error.clone());
    }
    let next_backoff = jittered_backoff(policy, *attempt);
    if let Some(exhausted) =
        retry_budget_exhausted(retry_budget, started, *attempt, next_backoff, last_error)
    {
        op_error!(domain = rpc, attempt = *attempt,
            max_attempts,
            %exhausted,
            "eth_sendRawTransaction: retry budget exhausted"
        );
        return Some(exhausted);
    }
    tokio::time::sleep(next_backoff).await;
    None
}

/// Retry an async operation with exponential backoff, emitting `log` records on
/// every retry attempt so operators can see backoff in logs.
///
/// The codebase's logging vocabulary is `log` (used across every Rust core
/// crate); `log` is already a dependency of `degenbot-rpc`, so no new dep is
/// introduced. The per-call context label is embedded in the `ProviderError`
/// `Display` (baked in via `into_provider_error("<context>")` at the call site,
/// which formats `{context}: {self}`), so the emitted `{error}` carries the
/// context verbatim.
///
/// Adoption verdict (spike, 2026-09,): alloy 2.4.2's
/// `transports::layers::RetryBackoffLayer` was evaluated as the
/// transport-idiomatic replacement for this loop and rejected: it only
/// `trace!`s retries (no leveled records, no provider `{context}`
/// labels), has no per-attempt deadline knob, its
/// `initial_backoff` is a fixed base that is explicitly "not an exponential
/// base", and exhaustion collapses typed errors into a `custom_str`. It is
/// also replay-unsafe for the non-idempotent `eth_sendRawTransaction` path
/// (see the broadcast comment further down). Do not re-litigate without
/// alloy API changes (per-attempt observer hooks, policy flexibility,
/// error-preserving exhaustion).
///
/// Adoption verdict (`backoff` crate,): NOT adopted. It is not a
/// workspace dependency today, its error wrapper would collapse typed
/// `ProviderError`s, its jitter is a multiplicative `randomization_factor`
/// rather than the additive tenacity-style jitter the verification policy
/// uses (adopting it would CHANGE the curves), and it cannot wrap the
/// per-call `tokio::time::timeout`. The loop stays centralized here
/// and consumes the shared [`RetryPolicy`] curve.
///
/// Emission policy:
/// - First retry (attempt 1): `log::debug!` — transient, often benign.
/// - Subsequent retries (attempt >= 2): `log::warn!` — sustained backoff.
/// - Exhausted all attempts: `log::error!` — terminal failure.
///
/// `retry_budget` bounds the chain's wall clock (see
/// [`retry_budget_exhausted`]); `None` keeps the attempt count as the only
/// bound.
///
/// Extracted from `AlloyProvider::retry_with_backoff` (which delegates, passing
/// `self.max_attempts` + `self.call_timeout` + `self.retry_budget`) so the loop
/// is unit-testable without constructing a live provider, and so a test can
/// drive `max_attempts = 2` for a fast, deterministic assertion that the
/// expected structured fields are emitted.
pub(crate) async fn retry_with_backoff_loop<F, Fut, T>(
    max_attempts: u32,
    call_timeout: Duration,
    retry_budget: Option<Duration>,
    operation: F,
) -> ProviderResult<T>
where
    F: Fn() -> Fut + Send + Sync,
    Fut: std::future::Future<Output = ProviderResult<T>> + Send,
    T: Send,
{
    let policy = rpc_retry_policy(max_attempts);
    let started = std::time::Instant::now();
    let mut attempt = 0;

    loop {
        // bound every attempt so a stuck transport (especially
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
            op_error!(domain = rpc, attempt,
                max_attempts,
                %outcome,
                "RPC retries exhausted"
            );
            return Err(outcome);
        }

        // Capped exponential backoff + site-owned uniform jitter.
        let sleep = jittered_backoff(&policy, attempt);
        let sleep_ms = sleep.as_millis();

        // Budget check BEFORE the retry log line: a refused sleep must not be
        // announced as a retry that then never happens.
        if let Some(exhausted) =
            retry_budget_exhausted(retry_budget, started, attempt, sleep, &outcome)
        {
            op_error!(domain = rpc, attempt,
                max_attempts,
                %exhausted,
                "RPC retry budget exhausted"
            );
            return Err(exhausted);
        }

        if attempt <= 1 {
            diag!(domain = rpc, attempt,
                max_attempts,
                sleep_ms,
                %outcome,
                "RPC retry"
            );
        } else {
            op_warn!(domain = rpc, attempt,
                max_attempts,
                sleep_ms,
                %outcome,
                "RPC retry"
            );
        }

        tokio::time::sleep(sleep).await;
    }
}

/// Broadcast-aware reconciliation loop for `eth_sendRawTransaction`.
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
    retry_budget: Option<Duration>,
) -> ProviderResult<B256>
where
    F: Fn() -> Fut + Send + Sync,
    Fut: std::future::Future<Output = ProviderResult<B256>> + Send,
    G: Fn() -> FutR + Send + Sync,
    FutR: std::future::Future<Output = ProviderResult<bool>> + Send,
{
    let policy = rpc_retry_policy(max_attempts);
    let started = std::time::Instant::now();
    let mut attempt = 0;

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
                            diag!(domain = rpc, %e,
                                %tx_hash,
                                "eth_sendRawTransaction: ambiguous outcome reconciled — receipt present"
                            );
                            return Ok(tx_hash);
                        }
                        Ok(false) => {
                            // Receipt absent → the body did NOT reach the node
                            // (or hasn't been mined). Rebroadcast after backoff.
                            op_warn!(domain = rpc, %e,
                                "eth_sendRawTransaction: ambiguous outcome — receipt absent, rebroadcasting"
                            );
                            // fall through to the shared backoff/retry block below
                            if let Some(terminal) = broadcast_retry_step(
                                &policy,
                                retry_budget,
                                started,
                                &mut attempt,
                                max_attempts,
                                &e,
                            )
                            .await
                            {
                                return Err(terminal);
                            }
                        }
                        Err(reconcile_err) => {
                            // Reconciliation itself failed (e.g. a transient
                            // RPC error on get_transaction_receipt). Treat as
                            // "absent / unknown" and rebroadcast — a missed
                            // receipt-probe must not strand the broadcast.
                            op_warn!(domain = rpc, %e,
                                %reconcile_err,
                                "eth_sendRawTransaction: ambiguous outcome; reconcile probe failed — rebroadcasting"
                            );
                            if let Some(terminal) = broadcast_retry_step(
                                &policy,
                                retry_budget,
                                started,
                                &mut attempt,
                                max_attempts,
                                &e,
                            )
                            .await
                            {
                                return Err(terminal);
                            }
                        }
                    }
                }
                // Rate-limited: the request never reached the node. The
                // mempool never saw it, so rebroadcast is safe + necessary.
                ProviderError::RateLimited { .. } => {
                    op_warn!(
                        domain = rpc,
                        attempt,
                        max_attempts,
                        "eth_sendRawTransaction: rate-limited — rebroadcasting"
                    );
                    if let Some(terminal) = broadcast_retry_step(
                        &policy,
                        retry_budget,
                        started,
                        &mut attempt,
                        max_attempts,
                        &e,
                    )
                    .await
                    {
                        return Err(terminal);
                    }
                }
                // RpcError / ExecutionReverted / InvalidParams / etc.: the tx
                // was seen and rejected by the node's validation. Retrying the
                // identical payload won't change the outcome — surface immediately.
                _ => return Err(e),
            },
        }
    }
}

/// The idempotent-RPC-method allowlist. `make_request` is a raw
/// escape hatch that can issue ANY method — including non-idempotent
/// `debug_*` / `trace_*` / state-mutating methods. Retrying those blindly (the
/// previous behavior) is unsafe: a partial timeout on a mutating call re-sends
/// the payload. `make_request` retries ONLY methods in this allowlist (read-only
/// eth_* calls that are safe to repeat); all other methods get a single
/// attempt. Callers who know their custom method is idempotent should route
/// through the typed methods (`get_block`, `eth_call`, etc.) instead.
#[must_use]
pub(crate) fn is_idempotent_rpc_method(method: &str) -> bool {
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
