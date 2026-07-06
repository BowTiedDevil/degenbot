# AlloyProvider reliability & API hygiene

`degenbot-rpc::provider::AlloyProvider` wraps alloy's `Provider<Ethereum>`
trait object and owns three cross-cutting concerns: connection construction
(HTTP/WS/IPC + optional throttling + caching), error classification
(`IntoProviderError` → `ProviderError`), and a retry-with-backoff loop.

A critical review surfaced that the **retry loop is the weak half**: it lacks
per-call timeout (so a stuck transport bypasses the entire apparatus), lacks
tracing (operators can't see backoff happening), applies one safety profile
to both reads and broadcast (re-broadcasting `eth_sendRawTransaction` on a
timeout is unsafe without receipt reconciliation), and ignores JSON-RPC-level
rate-limit codes that arrive over HTTP 200. The error-classification layer
(`IntoProviderError`) is the genuine asset and is left intact.

This epic addresses the six priority items from that review, ordered by
correctness/safety first, then operability, then API hygiene, then polish.

## Non-goals

- **Rewriting retry as a tower `Layer`.** A future refactor could move the
  backoff loop into a tower layer over the `RpcClient` while keeping
  `IntoProviderError` at the call site. That is a larger redesign, not in
  scope here. This epic keeps retry at the application level and fixes its
  gaps in place.
- **Removing `AlloyProvider` in favor of bare alloy `Provider`.** The wrapper
  earns its keep via classification + uniform error type + transport-agnostic
  construction. It stays.
- **Adaptive compute-unit-per-second rate-limit pacing** (alloy's
  `RateLimitRetryPolicy` model). Out of scope; the fixed exponential curve is
  kept, only its coverage gaps are fixed.
- **The lower-priority nits** from the review (hand-written `Clone`, 1751-line
  file split, `rpc_call!` macro DRY, tiny jitter range, `Fn` vs `FnOnce`
  closure bound). Tracked informally; not in this epic unless they pair with a
  task here.

## Constraints

- **Standalone-Rust-core constraint (AGENTS.md).** `degenbot-rpc` is a core
  crate — zero `pyo3` (enforced by `just check-no-pyo3-in-cores`). No task
  may add `pyo3` to this crate.
- **No breaking changes to `ProviderError`'s public variants** without a
  companion update to the PyO3 `From<ProviderError> for PyErr` impl in
  `degenbot-core`. New variants are fine; renaming/removing existing ones is
  a separate migration.
- **Broadcast safety is paramount.** `eth_send_raw_transaction` must never
  silently re-broadcast after an ambiguous outcome (timeout / connection drop
  after the body was sent). Reconciliation, not blind retry.
- **Red/Green TDD** for all behavioral changes (AGENTS.md). New behavior
  ships with tests; refactors keep existing tests green.

## Key decisions (resolved during planning)

1. **Per-call timeout lives inside `retry_with_backoff`, not on the alloy
   client.** Wrapping each `operation()` in `tokio::time::timeout` gives
   uniform behavior across HTTP/WS/IPC (alloy's client timeout only covers
   HTTP). The elapsed-timeout error is classified as `ProviderError::Timeout`
   so it feeds the existing retry predicate. A configurable timeout duration
   is stored on `AlloyProvider` alongside `max_attempts`.
2. **Broadcast is split out of `retry_with_backoff`.** `eth_send_raw_transaction`
   gets a dedicated path: on `Timeout`/`ConnectionFailed`, reconcile via
   `get_transaction_receipt` before deciding to rebroadcast; on
   `RateLimited`, back off and rebroadcast (idempotent). Non-retryable
   `RpcError` codes ("already known", "nonce too low") surface immediately —
   they indicate the tx was seen.
3. **JSON-RPC rate-limit codes are classified into `ProviderError::RateLimited`,
   not a new variant.** `into_provider_error` inspects `ErrorResp.code` for
   the known provider quota codes (-32005, -32004, and the -32001
   "request limit" family) and maps them to `RateLimited`, so the existing
   `is_retryable()` predicate picks them up without a new variant.
4. **Typed returns for tx/receipt.** Replace `Option<serde_json::Value>` with
   alloy's typed `Transaction` / `Receipt`. PyO3 wrappers serialize at the
   boundary if Python needs JSON. The `U256`→`B256` storage round-trip is
   replaced with a direct conversion.
5. **Tracing uses `tracing::warn!`** on retry (attempt, delay_ms, error, rpc
   method context). No new metrics dependency — `tracing` is already in the
   tree.
6. **`DEFAULT_MAX_RETRIES` is either used or deleted.** Production call sites
   pass `3`; the constant advertises `10`. The constant becomes the actual
   default for `AlloyProvider::new` (which gains an optional retry override via
   a small builder or a second constructor), removing the footgun.

## Risk

- **Highest risk: broadcast reconciliation (task 2).** Getting receipt-fetch
  semantics wrong (e.g. racing mempool propagation, mis-handling "not found"
  vs "rejected") can cause double-broadcast or dropped txs. Mitigation: TDD
  with a mock provider exercising each outcome branch; the `test-utils`
  `from_provider` seam already exists for this.
- **Medium risk: per-call timeout (task 1).** Choosing a default duration
  that is too short breaks large `eth_simulateV1` / `get_logs` responses. The
  timeout must be per-call-type overrideable, or generous enough (e.g. 30s
  default) not to interrupt legitimate long responses.
- **JSON-RPC code mapping (task 4)** depends on provider-specific code
  conventions that aren't standardized. Mapping must be conservative
  (only codes documented across Alchemy/QuickNode/Infura) to avoid retrying
  genuinely-fatal errors.

---

# Per-call timeout in the retry loop

## Goal
- Every `AlloyProvider` RPC call is bounded by a timeout so a stuck
  transport (especially WS/IPC mid-read) cannot block `retry_with_backoff`
  forever. Today the loop only retries when calls *return*; a hung call
  bypasses the entire retry apparatus.

## Context
- `retry_with_backoff` in `rust/crates/degenbot-rpc/src/provider.rs` awaits
  `operation().await` with no deadline. HTTP has alloy's read timeout, but
  WS/IPC have no default — a half-open socket hangs the loop indefinitely.
- Decision (epic): wrap each attempt in `tokio::time::timeout` inside
  `retry_with_backoff`; classify the elapsed error as
  `ProviderError::Timeout` so it feeds the existing `is_retryable()`.

## Acceptance Criteria
- `AlloyProvider` stores a `call_timeout: Duration` (default 30s) set at
  construction; `new` and `with_rate_limit` accept it (or expose a builder).
- `retry_with_backoff` wraps each `operation()` in
  `tokio::time::timeout(self.call_timeout, operation())`; on elapsed, the
  error is `ProviderError::Timeout { message }` and the retry predicate
  decides whether to retry.
- Per-call-type override is NOT required for v1 — a single generous default
  is acceptable. Document that `eth_simulate_v1` / large `get_logs` may need
  a longer value; expose a way to raise it (builder or constructor arg).
- A test using the `test-utils` `from_provider` seam injects a provider whose
  call hangs; the loop times out and retries (or returns `Timeout` after
  exhausting attempts) within a bounded wall-clock.

## Validation Gates
- `just test-rust` (focus: `degenbot-rpc` provider tests)
- `just lint-rust`
- New test asserts bounded wall-clock under a hung inner provider.

---

# Don't blindly retry eth_sendRawTransaction

## Goal
- `eth_send_raw_transaction` no longer route through the generic
  `retry_with_backoff`. On ambiguous outcomes (timeout, connection drop after
  the body was sent) it reconciles via `get_transaction_receipt` before
  deciding to rebroadcast, instead of blindly re-sending the identical
  signed payload.

## Context
- Current code:
  ```rust
  pub async fn eth_send_raw_transaction(&self, encoded_tx: &[u8]) -> ProviderResult<B256> {
      self.retry_with_backoff(|| async {
          let pending = self.inner.send_raw_transaction(encoded_tx).await
              .map_err(|e| e.into_provider_error("eth_sendRawTransaction failed"))?;
          Ok(*pending.tx_hash())
      }).await
  }
  ```
- A `Timeout` means the body may have reached the node; rebroadcasting is a
  guess. Mempools usually dedupe ("already known"), but the right behavior is
  to confirm outcome first.
- Decision (epic): dedicated path. On `Timeout`/`ConnectionFailed`, fetch the
  receipt by the computed tx hash; if present, return the hash (already
  broadcast); if absent, back off and rebroadcast. On `RateLimited`, back off
  and rebroadcast (idempotent). On non-rate-limit `RpcError` ("already known",
  "nonce too low", "replacement underpriced") surface immediately — the tx
  was seen and rejected, retrying won't help.

## Acceptance Criteria
- `eth_send_raw_transaction` does not use `retry_with_backoff` directly; it
  uses a dedicated broadcast-aware helper.
- Outcome branches each covered by a test using `test-utils`:
  - Success on first try → returns hash, no rebroadcast.
  - `RateLimited` → retries, succeeds.
  - `Timeout` → reconciles; receipt present → returns hash without
    rebroadcast; receipt absent → rebroadcasts after backoff.
  - `RpcError` "already known" (-32000/"already known") → surfaces
    immediately, no retry.
- The tx hash is computed locally from the signed payload (so reconciliation
  can happen even when the broadcast response was lost) — verify alloy
  exposes this or compute it; if not computable locally, document the
  limitation and reconcile via the pending-tx hash from a prior successful
  attempt instead.

## Validation Gates
- `just test-rust` (mock-provider broadcast tests)
- `just lint-rust`
- Manual: against a local anvil, send a tx that anvil confirms, then trigger
  a synthetic timeout on a second send and confirm no double-inclusion.

---

# Emit tracing on every retry

## Goal
- Operators can see, in logs, that a call is backing off — attempt number,
  delay, error, and which RPC method/context. Today `retry_with_backoff`
  sleeps and retries in complete silence.

## Context
- `retry_with_backoff` has no `tracing` emit. The prior-turn framing
  positioned operator observability as the value-add of owning this layer;
  silent retry contradicts that.
- Decision (epic): `tracing::warn!` on each retry attempt with `attempt`,
  `max_attempts`, `delay_ms`, `error = %e`, and a context string. No new
  metrics dependency — `tracing` is already in the tree.

## Acceptance Criteria
- Every retry attempt emits a `tracing::warn!` (or `tracing::debug!` for the
  first attempt, `warn!` once past attempt 2) including: attempt number,
  max_attempts, delay_ms, the `ProviderError` display, and a per-call context
  label (the same `context: &str` already threaded through `rpc_call!` /
  `into_provider_error`).
- The terminal failure (exhausted attempts) emits a `tracing::error!` with
  the same fields plus final error.
- A test captures the tracing subscriber (or asserts via a `tracing::mock`
  / `tracing-subscriber` test layer) that retry emits the expected span/event.

## Validation Gates
- `just test-rust`
- `just lint-rust`
- Test asserts a retry attempt emits the structured fields.

---

# Retry JSON-RPC-level rate-limit error responses

## Goal
- `ProviderError::is_retryable()` returns true for JSON-RPC error responses
  that signal rate limiting over HTTP 200 (codes like -32005, -32004), so
  they feed the retry loop instead of failing fast. Today `into_provider_error`
  maps any `ErrorResp` to `RpcError { code }`, which `is_retryable()` excludes.

## Context
- Decision (epic): `into_provider_error` inspects `ErrorResp.code` for the
  known provider quota codes and maps them to `ProviderError::RateLimited`
  (not a new variant), so the existing predicate picks them up. Conservative
  code set: Alchemy/QuickNode use -32005/-32004; some Infura responses use
  -32001 with a rate-limit message.
- `RateLimited { message }` already carries the original message for
  observability.

## Acceptance Criteria
- `IntoProviderError::into_provider_error` recognizes the documented
  rate-limit `ErrorResp` codes and returns `ProviderError::RateLimited`.
- A non-rate-limit `ErrorResp` (e.g. -32000 execution revert, -32601 method
  not found) still maps to `RpcError { code }` and is NOT retried.
- Tests cover: -32005 → RateLimited (retryable); -32000 revert → RpcError
  (not retryable); -32601 → RpcError (not retryable).
- Code mapping is data-driven (a `const` set/table) so adding provider codes
  later is one line, not a match-arm edit.

## Validation Gates
- `just test-rust` (classification unit tests)
- `just lint-rust`

---

# Typed returns for transaction and receipt; fix storage round-trip

## Goal
- `get_transaction` / `get_transaction_receipt` return alloy's typed
  `Transaction` / `Receipt` instead of `Option<serde_json::Value>`, removing
  a serialize-then-reparse round-trip on every call. `get_storage_at` drops
  the pointless `U256`→`B256` byte-array detour.

## Context
- Current `get_transaction`/`get_transaction_receipt` do
  `serde_json::to_value(&tx)` inside the retry closure, returning
  `Option<serde_json::Value>`. Inconsistent with the rest of the API
  (`EthBlock`, `Log`, `FeeHistory`) and wasteful.
- `get_storage_at` does `B256::from(result.to_be_bytes::<32>())` where a
  direct conversion suffices (and may be a no-op if alloy already returns
  `B256`).
- PyO3 wrappers serialize at the FFI boundary if Python needs JSON; the core
  returns typed values.

## Acceptance Criteria
- `get_transaction` returns `ProviderResult<Option<Transaction>>` (alloy's
  typed tx type, consistent with `EthBlock`'s `Transaction<TxEnvelope>`).
- `get_transaction_receipt` returns `ProviderResult<Option<Receipt>>` (alloy
  typed).
- `get_storage_at` returns `B256` via a direct conversion — no
  `to_be_bytes::<32>()` detour. If alloy returns `U256`, use
  `B256::from(result)` or document why the detour exists. If alloy already
  returns `B256`, return it directly.
- PyO3 wrapper(s) that consume these updated accordingly (serialize to JSON
  for Python only if a Python caller existed; check `degenbot-python` for
  callers).
- Existing tests updated to assert on typed fields rather than JSON paths.

## Validation Gates
- `just test-rust`
- `just test-rust-python` (if PyO3 wrappers change)
- `just lint-rust`

---

# Polish: IPC URL detection, make_request retry, eager WS/IPC connect, retry-config constant

Bundle of four independent, low-risk fixes. May be committed as up to four
commits; track as one ergo task for simplicity since each is small.

## Goal
- `build_provider` IPC detection is explicit (not "anything without `://`").
- `make_request` does not blindly retry non-idempotent RPC methods.
- WS/IPC connect is no longer eagerly fatal at construction (matches HTTP's
  lazy-connect, retried-on-first-call behavior).
- `DEFAULT_MAX_RETRIES` constant is no longer misleading (used as the actual
  default for `new`, or deleted).

## Context
- Sub-items (from the critical review):
  - **IPC detection (#4):** `else if !rpc_url.contains("://")` treats a typo
    like `localhost:8545` as an IPC path. Require `/`, `\\`, or `ipc://`.
  - **make_request retry (#5):** the raw escape hatch applies
    `retry_with_backoff` to any method, including `debug_*`/`trace_*`. Add a
    retry flag or a non-retried variant for non-idempotent methods.
  - **WS/IPC eager connect (#14):** `connect_ws`/`connect_ipc` `.await?` at
    construction; a transient outage fails provider build permanently while
    HTTP is lazy. Make WS/IPC lazy (connect on first call, retried) or add
    connect-time retry. Document the chosen approach in the task completion
    note.
  - **retry-config constant (#7):** `DEFAULT_MAX_RETRIES = 10` is exported
    but `new` callers pass `3`. Either make `new` default to the constant
    (per epic decision 6: constant becomes the actual default, with a
    builder/second constructor for override) or delete the constant.

## Acceptance Criteria
- `build_provider` rejects `localhost:8545`-style strings with a clear
  `ConnectionFailed` ("unsupported scheme / not an IPC path") instead of
  silently routing to IPC. A path like `/tmp/anvil.ipc` or `\\.\pipe\x` still
  routes to IPC.
- `make_request` either takes an explicit retry flag
  (`make_request_no_retry` or `make_request(method, params, retry: bool)`)
  or defaults to no-retry for methods not in an idempotent allowlist
  (`eth_call`, `eth_getLogs`, `eth_getBlockByNumber`, etc.). Documented.
- WS/IPC providers connect lazily on first RPC call (retried via
  `retry_with_backoff`) OR construction retries the connect a bounded number
  of times. Chosen approach documented.
- `DEFAULT_MAX_RETRIES` reflects the actual production default (callers no
  longer hardcode `3`); or the constant is removed and the default is
  inlined with a doc comment. `new(&rpc_url, 3)` call sites across
  `degenbot-bot` / `degenbot-python` / `degenbot-price` updated to the new
  signature or left passing the constant.

## Validation Gates
- `just test-rust` (URL-scheme detection tests; make_request retry-flag test)
- `just lint-rust`
- `just test-rust-python` if any call-site signatures change