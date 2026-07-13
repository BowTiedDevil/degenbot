# Scaffold `degenbot-fork` crate + enable `node-bindings` feature

## Goal
- Create `rust/crates/degenbot-fork/` (Cargo.toml + `src/lib.rs` skeleton) and
  register it in the workspace `rust/Cargo.toml` members list.

## Context — critical gotcha verified by the IPC spike
- `alloy = "^2.0"` with `features = ["full"]` does **NOT** enable `node-bindings`
  in alloy v2.1.1. The `full` feature array includes `provider-ipc` +
  `provider-anvil-api` but NOT `node-bindings`. It must be added explicitly:
  `alloy = { version = "^2.0", features = ["full", "node-bindings", "transport-throttle"] }`.
- Without it, `alloy::node_bindings::Anvil` is unresolved (cfg-gated re-export
  in `alloy/src/lib.rs`).
- The `alloy-node-bindings` crate (v2.1.1) is published on crates.io — no
  git-dep hell (unlike foundry's `anvil` crate, which is NOT published and
  pulls the entire foundry workspace + `tempo-*` EVM stack with cascading
  version drift — verified infeasible in an earlier spike).

## Spike evidence (the IPC spike that de-risked this epic)
A throwaway crate using `alloy = { version = "2", features = ["full", "node-bindings"] }`
verified end-to-end:
- `Anvil::new().ipc_path(path).try_spawn()` spawns the `anvil` binary (v1.7.1
  at `/home/dev/.foundry/bin/anvil`) as a subprocess exposing an IPC socket.
- `ProviderBuilder::new().connect_ipc(IpcConnect::new(path))` connects an
  alloy Provider over IPC (no HTTP regression vs the current Python AnvilFork).
- `AnvilApi` ext trait methods all drive the node over IPC:
  `anvil_node_info`, `evm_mine`, `anvil_snapshot`, `anvil_revert`,
  `anvil_set_balance` — all green. `Provider` trait methods (`get_block_number`,
  `get_balance`) over the same IPC transport — green.
- Whole run <2s on an in-memory (no-fork) anvil.

## Architectural alignment
- New crate `degenbot-fork` owns the embedded anvil-node lifecycle + Provider
  + the dev-RPC surface via `AnvilApi`. Standalone-Rust-core constraint: a
  Rust-only consumer can `cargo add degenbot-fork` to spin a fork — no Python
  required. Zero `pyo3` in this crate (enforced by `just check-no-pyo3-in-cores`).
- PyO3 wrapper lands under `degenbot-python/src/fork/` (task FF2).

## Acceptance Criteria
- `rust/crates/degenbot-fork/Cargo.toml` declares
  `alloy = { version = "^2.0", features = ["full", "node-bindings", "transport-throttle"] }`.
- Crate registered in `rust/Cargo.toml` workspace members.
- `src/lib.rs` skeleton with module structure (`pub mod node;` placeholder).
- `cargo check -p degenbot-fork` clean; `alloy::node_bindings::Anvil` resolves.
- `just check-no-pyo3-in-cores` still passes (crate is pyo3-free).

## Validation Gates
- `cargo check -p degenbot-fork`
- `just check-no-pyo3-in-cores`
- `just lint-rust`
---
# Rust `AnvilFork` struct: lifecycle + Provider + AnvilApi

## Goal
- Implement `degenbot_fork::AnvilFork` — a Rust-owned fork handle wrapping
  `alloy::node_bindings::Anvil` (subprocess lifecycle) + a connected alloy
  `Provider` (over IPC) + the `AnvilApi` dev-RPC surface.

## Constructor config (mirror Python `AnvilFork.__init__` 1:1)
Map these Python kwargs onto `Anvil::new()` builder + raw `.arg()` passthrough:
- `fork_url: Option<String>` → `.fork(url)` (None = in-memory, no fork).
- `block_number: Option<u64>` → `.fork_block_number(n)`.
- `base_fee: Option<u128>` → `.arg("--base-fee").arg(...)`.
- `storage_caching: bool` → `.arg("--no-storage-caching")` when false.
- `ipc_path: Option<String>` → `.ipc_path(path)` (default `/tmp/anvil.ipc`).
- `localhost`/`port`/`preserve_capture`/`transaction_hash`/mining-mode args →
  `.arg()` passthrough or specific builder methods where alloy exposes them.
- `chain_id`, `mnemonic` → builder methods (alloy exposes `.chain_id()`,
  `.mnemonic()`).

## Lifecycle
- `try_spawn() -> Result<Self, ForkError>` — builds the Anvil instance,
  connects the Provider over `ipc_path()`, returns the handle.
- `Drop` — alloy's `AnvilInstance` + Provider drop cleanly (kills the
  subprocess). No `_socket`/IPC-close logic (alloy manages the transport).
- New error enum `ForkError` (spawn-failed, connect-failed, rpc-error) —
  standalone-usable, no pyo3.

## Dev-RPC surface (typed methods delegating to `AnvilApi` — 1:1 with the
## current Python `AnvilFork` public methods)
- `mine() -> Result<(), ForkError>` → `provider.evm_mine(None)`.
- `reset(block_number: Option<u64>) -> Result<(), ForkError>` →
  `provider.anvil_reset(forking)` where `forking = Some(Forking { block_number, .. })`.
- `snapshot() -> Result<U256, ForkError>` → `provider.anvil_snapshot()`.
- `revert(id: U256) -> Result<bool, ForkError>` → `provider.anvil_revert(id)`.
- `set_balance(addr, bal)`, `set_code(addr, code)`, `set_nonce(addr, nonce)`,
  `set_storage_at(addr, slot, val)`, `set_next_block_base_fee(fee)`,
  `set_next_block_timestamp(ts)`, `set_block_timestamp_interval(secs)`,
  `set_coinbase(addr)` — straight `AnvilApi` delegations.
- `node_info() -> Result<NodeInfo, ForkError>` (bonus, not in current surface).
- Async variants (`mine_async`, `reset_async`, etc.) — the alloy Provider is
  async-native; expose `async fn` forms mirroring the Python `_async` methods.

## Provider accessor
- `provider(&self) -> &Provider` (or a cloned handle) so callers can do
  general RPC (`get_block`, `get_balance`, `eth_call`) over the same IPC
  transport. This replaces the Python `self.w3` handle.

## Tests
- In-process unit tests (no-fork anvil, like the spike) under
  `degenbot-fork/src/node.rs` `#[cfg(test)]` or a `tests/` dir:
  spawn → mine → snapshot → revert → set_balance → assert. Mirror the spike.
- Do NOT require a live RPC endpoint (use in-memory anvil for tests).

## Acceptance Criteria
- `AnvilFork` struct + `ForkError` compile; constructor covers all
  Python-kwarg equivalents.
- All 12 dev-RPC methods + the provider accessor present.
- Unit tests green on an in-memory anvil (no external RPC).
- Crate remains pyo3-free.

## Validation Gates
- `cargo test -p degenbot-fork`
- `just check-no-pyo3-in-cores`
- `just lint-rust`
---
# PyO3 wrapper `PyAnvilFork`

## Goal
- `#[pyclass]` `PyAnvilFork` under `rust/crates/degenbot-python/src/fork/mod.rs`
  wrapping `degenbot_fork::AnvilFork`. Per the three-layer architecture (ADR-005):
  pyo3-only — arg extraction → GIL release → core call → result wrap. **Zero
  business logic in the wrapper.**
- `degenbot-python/Cargo.toml` adds `degenbot-fork = { path = "../degenbot-fork" }`.

## Surface
- `#[new]` constructor extracting the Python kwargs (fork_url, block_number,
  base_fee, storage_caching, ipc_path, etc.) → GIL release →
  `AnvilFork::try_spawn()` → wrap in `PyAnvilFork` (or `PyValueError` on spawn failure).
- Methods mirroring the Rust struct: `mine`, `reset`, `snapshot`, `revert`,
  `set_balance`, `set_code`, `set_nonce`, `set_storage_at`,
  `set_next_base_fee`, `set_next_block_timestamp`,
  `set_block_timestamp_interval`, `set_coinbase`. Each: extract args →
  `Python::allow_threads` → core call → convert result to Python types
  (HexBytes for bytes, int for U256, etc.).
- Async methods: expose as `async def` via pyo3's async support OR sync-only
  with a note (decide during impl; the alloy Provider is async, so the
  pyo3 wrapper spawns a tokio runtime or uses the existing one).
- `provider` property → returns the alloy Provider pyclass (for general RPC).
- `http_url`/`ipc_path` properties (if retained — see task FF4's design Q).

## Stub
- Update `src/degenbot/degenbot_rs.pyi` with the `PyAnvilFork` class stub
  (methods + types). RUF022-safe section (separate from the rpc_types/crypto
  stubs already there — different section, no `__all__` collision).

## Acceptance Criteria
- `PyAnvilFork` compiles; `.pyi` stub updated.
- Wrapper contains zero business logic (pyo3 + delegation only).
- `just check-no-pyo3-in-cores` unaffected (wrapper is in degenbot-python,
  not a core).

## Validation Gates
- `cargo check -p degenbot-python`
- `just lint-rust`
- `just test-rust-python` (once Python shell lands in FF4)
---
# Rewrite Python `anvil_fork.py` as the companion shell

## Goal
- `src/degenbot/anvil_fork.py` becomes a thin companion shell over
  `PyAnvilFork`. Preserves the public constructor config + the dev-method
  surface; delegates everything to the Rust core.

## Interface evolution (the breaking change — document it)
- **Stays:** constructor kwargs (fork_url, block_number, base_fee,
  storage_caching, preserve_capture, ipc_path, mining-mode args) — passed
  through to `PyAnvilFork`.
- **Stays:** all dev methods (mine/mine_async, reset/reset_async,
  set_snapshot/return_to_snapshot, set_balance, set_code, set_coinbase,
  set_nonce, set_storage, set_next_base_fee/_async,
  set_next_block_timestamp/_async, set_block_timestamp_interval) — now
  delegate to `PyAnvilFork`.
- **Changes:** `self.w3` (Web3) → `self.provider` (the alloy Provider pyclass
  from `PyAnvilFork`). Callers do `fork.provider.get_block(...)` instead of
  `fork.w3.eth.get_block(...)`. This is the breaking interface change (flagged
  in the epic).
- **Changes:** `async_w3()` ctx mgr → `async_provider()` (or similar).
- **Removed:** the subprocess-management code (`_setup_process`,
  `_anvil_command`, `AnvilOptions` parsing), the IPC-socket close logic
  (`_close_ipc_socket`, `_socket` getattr), the web3 client setup
  (`_setup_w3`), the `middleware_onion.inject` middlewares path, the
  `make_request(RPCEndpoint(...))` calls, the `RPCEndpoint`/`Middleware`/
  `IPCProvider`/`AsyncIPCProvider`/`Web3`/`AsyncWeb3` imports.

## Design Q to resolve in-impl
- Subprocess-wire properties (`http_url`, `ws_url`, `ipc_filename`, `port`,
  stderr/stdout capture files): the alloy `Anvil` instance exposes
  `.endpoint()` (HTTP). Decide: keep `http_url` (delegates to anvil endpoint),
  keep `ipc_path` (delegates), drop `ws_url`/capture-files (subprocess stderr
  capture may not be accessible via alloy — if needed, add `.arg()` to
  redirect anvil's stderr to a file). Document the decision in the task result.

## Acceptance Criteria
- `anvil_fork.py` imports no `web3` (no `IPCProvider`/`AsyncIPCProvider`/
  `AsyncWeb3`/`Web3`/`Middleware`/`RPCEndpoint`).
- Constructor config surface unchanged.
- All dev methods present + delegate to `PyAnvilFork`.
- `self.provider` replaces `self.w3`.
- `rg "from web3|import web3" src/degenbot/anvil_fork.py` empty.

## Validation Gates
- `just lint` (ruff + ty on anvil_fork.py)
- `just test-python` (after caller migration in FF5)
---
# Migrate callers off `fork.w3.eth.X` → `fork.provider.X`

## Goal
- Update every `fork.w3.eth.X` / `fork.w3.X` callsite in tests + examples to
  use the new `fork.provider` handle (the alloy Provider pyclass).

## Callers (from the C3 survey)
- `tests/test_anvil_fork.py` — heavy `fork.w3.eth.get_block(...)` /
  `get_block_number` / `chain_id` / `get_balance` usage across ~12 test fns.
- `tests/uniswap/v4/test_uniswap_v4_onchain_parity.py:205` — `AnvilFork(...)`
  construction + `fork.w3` usage.
- Any `examples/` using AnvilFork (grep during impl).

## Method mapping (web3.eth.X → alloy provider.X)
- `fork.w3.eth.get_block("latest")` → `fork.provider.get_block("latest")`
  (return shape may differ — web3 returns a dict; alloy returns a typed
  `Block` — adapt assertions).
- `fork.w3.eth.get_block_number()` → `fork.provider.get_block_number()`.
- `fork.w3.eth.chain_id` → `fork.provider.get_chain_id()`.
- `fork.w3.eth.get_balance(addr)` → `fork.provider.get_balance(addr)`.
- `fork.w3.eth.get_code(addr)` → `fork.provider.get_code(addr)`.
- Note: web3's attribute access (`w3.eth.chain_id`) vs alloy's method call
  (`provider.get_chain_id()`) — adjust call sites.

## Return-shape adaptation
- web3 `get_block` returns a dict (`block["baseFeePerGas"]`); alloy returns a
  typed `Block` with field access (`block.header.base_fee_per_gas`). Update
  assertions. This is the bulk of the test-rewrite work.

## Acceptance Criteria
- `rg "fork\.w3|\.w3\.eth" tests/ examples/` empty.
- All AnvilFork-touching tests pass.

## Validation Gates
- `just test-python` (full sweep)
- `just lint`
---
# Reframe C3 (SNZRXX) + unblock C6 (DFPIAM)

## Goal
- Wire this epic's completion into the EN7WIA (web3py-retirement) graph:
  C3 (SNZRXX) is absorbed (its mechanical-swap plan superseded by this
  Rust-owned build); C6 (DFPIAM) gets its dependency on SNZRXX replaced by a
  dependency on this epic's capstone.

## Actions
- **Reframe C3 (SNZRXX):** set state to `canceled` with a result note
  pointing to this epic — the mechanical `make_request`-swap plan is
  superseded by the `degenbot-fork` + `alloy::node_bindings` build (tasks
  FF1–FF5). The web3 removal from `anvil_fork.py` (C3's AC) is now delivered
  by FF4 + FF5.
- **Repoint C6 (DFPIAM):** remove C6's dependency edge on SNZRXX; add a
  dependency edge on the capstone task of THIS epic (the C3/C6 wiring task,
  i.e. this task FF6). Once FF6 lands, C6 can drop `web3 ~= 7.14` from
  `pyproject.toml`.
- **Re-enable C6's full sweep:** when C6 runs, it also removes the
  C1/C2-deferred boundary imports (the `#[tool.uv]` lazy-import comment in
  pyproject.toml:159 if web3-only; `async_adapter.py` `TransactionNotFound`;
  `log_fetching.py` `Web3Exception` retry-policy; `sync_adapter.py`
  `_Web3ContractLogicError`). All web3 usage is gone after this epic.

## Acceptance Criteria
- SNZRXX state = `canceled`, result note references this epic (FF1–FF5).
- DFPIAM no longer depends on SNZRXX; depends on FF6 (this task).
- ergo dependency graph reflects the reframe (`ergo list --epic EN7WIA` +
  `ergo list --epic <this epic>` both sane).

## Validation Gates
- `ergo --json show SNZRXX` (state=canceled)
- `ergo --json show DFPIAM` (deps updated)
- `ergo list --epic EN7WIA` (C6's blocker now FF6, not SNZRXX)