{
  "id": "3f5be5cf",
  "title": "Lift verify-plumbing (run_cl_verification) onto a VerifyRpc trait — finish slice-5 candidate-2",
  "tags": [
    "adr-006",
    "deferred",
    "rust",
    "py-binding-lift",
    "deep-module",
    "verify"
  ],
  "status": "done",
  "created_at": "2026-06-19T04:01:26.239Z"
}

**Ergo:** `ADLDG6` (epic `B4Y5GN` ADR-006). Deferred from slice 5b — COMPLETE.

## What landed
- `rust/src/optimizers/uniswap_engine/snapshot_verify.rs`:
  - Extended `VerifyError` with `Snapshot(String)` (verify-phase mismatch) + `Provider(String)` (provider construction failure).
  - Introduced the `VerifyRpc` trait (sync methods; the impl blocks internally) — the minimal on-chain surface the two-phase verification needs: `enabled()`, `verify_v3_snapshot`/`verify_v3_backfill`, `verify_v4_snapshot`/`verify_v4_backfill`.
  - Lifted `run_cl_verification` out of `py_binding.rs` as a pure orchestrator generic over `impl VerifyRpc`: decide (enabled gate) → snapshot phase → backfill phase, in order. No `pyo3`/`PyResult`/`AlloyProvider`/`tokio` at the type level.
  - 6 new unit tests with a `FakeVerifyRpc` (records calls, can fail per phase): disabled-skip, snapshot-then-backfill ordering, snapshot-mismatch short-circuits backfill, backfill-mismatch propagates, provider-error short-circuits, missing snapshot block still runs backfill. No live RPC.
- `rust/src/optimizers/uniswap_engine/py_binding.rs`:
  - Removed the old `run_cl_verification` (closure-over-`&AlloyProvider`→`PyResult`) + the `PyResult`-flavored `verification_provider`.
  - Added the single concrete `VerifyRpc` impl `EngineVerifyRpc<'a>` — borrows the engine's `verify_rpc_url` + `verify_provider` caches. Each method checks configuration via `enabled()`, lazily ensures the cached `AlloyProvider` via the retained `verification_provider` (now `Result<_, VerifyError>`) I/O seam, `block_on`s the underlying `crate::bot_core::liquidity_verifier` async call, and maps `VerificationMismatch` → `VerifyError::Snapshot` with byte-for-byte the legacy message (phase + pool label + mismatch detail).
  - `map_verify_err` extended to cover `Snapshot`/`Provider` → `PyRuntimeError`.
  - The `register_v3_pool` + `register_v4_pool` verify branches: the two `verify_snapshot`/`verify_backfill` closures shrank from ~15-line `block_on` + error-formatting bodies to thin delegations to the trait (`rpc.verify_v3_snapshot(addr, td, block)` etc.). The pure orchestrator drives them in order.

## Behavior preserved
- Same two-phase order (snapshot → backfill), same `RuntimeError`-on-mismatch contract, same error message text (phase + pool label + mismatch), same provider caching (lazily built once, reused across both phases), same `rpc_url: None` early-return (now the `enabled()` gate).

## Acceptance
- `just lint` green (cargo fmt/clippy + ruff + ty + markdownlint).
- `just test-rust` green; 9 `snapshot_verify` tests pass (3 pre-existing + 6 new).
- `tests/arbitrage/test_optimizers/`, `tests/rust/`, `tests/test_bot_single_chain.py` — 572 passed.
- No `pyo3`/`PyResult` in `snapshot_verify.rs`'s `VerifyRpc`/`run_cl_verification`; `AlloyProvider` construction+`tokio::block_on` confined to the `EngineVerifyRpc` impl in `py_binding.rs`.
- The `register_v3_pool`/`register_v4_pool` verify branches delegate to the trait (closures shrank to `.verify_v*` calls).
