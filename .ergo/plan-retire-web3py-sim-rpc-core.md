# Retire web3py: wire the simulation & RPC Rust core through the cockpit

The Architectural Vision (AGENTS.md) names two infrastructure pieces still
un-routed through the Rust core: **simulation** and **RPC**. This epic closes
both, then completes web3py's removal as a runtime dependency. It is one graph
so a follow-up session inherits the full scope: the boundary work (Pass A)
delivers the architectural win; the type/exception sweep (Pass B) drops the
`web3` dependency from `pyproject.toml`. web3py stops being on any hot path
after Pass A — Pass B deletes the dependency without changing behavior.

Two candidates drive Pass A:

- **Candidate 1 — wire the stranded `degenbot-simulation` crate through the
  cockpit.** A complete 3660-line Rust port (`simulate_one`,
  `dispatch_profitable_results`, `eth_simulateV1` dispatch, payload, calldata)
  exists with **zero consumers** — no Cargo dep from `degenbot-python`, no
  `c_api` entry, no pyi symbol. Meanwhile `examples/eth_backrun_v2_v3_v4_rust.py`
  runs its own ~1900-line parallel Python copy (`simulate_one` = 846 lines,
  `dispatch_profitable_results` = 1055 lines). Sub-steps B (PyO3 seam) + C
  (route + delete the Python copy) were never executed. The crate's own module
  docs cite task IDs and reference the Python line ranges they port.
- **Candidate 2 — make `degenbot-rpc` the single RPC owner.** `PyBotIo` (the
  pyclass I/O façade pool builders receive) currently holds `Py<PyAny>` and
  GIL-round-trips every `get_block`/`call`/`get_logs` into the Python
  `ProviderAdapter` facade. The crate's own top-of-file doc (py_bot_io.rs:1–24)
  names this as the **14b/14c cutover**: swap the held `Py<PyAny>` for a direct
  `AlloyProvider` field + native method bodies. Separately, the polymorphic
  `ProviderAdapter`/`AsyncProviderAdapter` facade wraps three backends
  (`_Web3Adapter`/`_AlloyAdapter`/`_OfflineAdapter`); only alloy is real (and
  the async path is **already alloy-only**). Retire the wrapper + web3 backend;
  `Bot`/`AsyncBot` take the `AlloyProvider`/`AsyncAlloyProvider` pyclass
  directly.

Pass B is the mechanical-but-broad sweep that removes the `web3 ~= 7.14`
runtime dependency: replace `web3.types`/`web3.exceptions` imports with native
degenbot types/exceptions (~48 files), port `AnvilFork` + the two snapshot
modules off `Web3()`, and replace `web3.keccak(text=...)` selector sites with
`sol!`-typed calls or a degenbot keccak helper.

## Non-goals

- **Porting the DB-aware pool updaters to Rust.** The 5 pool hot-sync
  `make_request` sites in the example (reserves/slot0/liquidity/code — all
  sim-fail *diagnostics*, `try/except: pass`) are not pool-updater work; they
  route to existing typed `PyBotIo` fetchers in Pass A. The actual per-block
  pool-state updater migration is a separate Vision-named future epic.
- **Retiring the `AsyncProviderAdapter` Python class if residual
  `make_request` consumers remain.** Where Python still needs raw
  `make_request` after candidates 1+2 (AnvilFork until C3), the alloy pyclass
  already exposes `make_request` natively — Python calls the pyclass directly.
  The Python wrapper class retires in B4; the pyclass is the consumer's handle.
- **A backwards-compatibility layer for the retired wrapper / web3 backend.**
  AGENTS.md forbids it; breaking changes are scoped to the next release.
- **The Alembic/SQLAlchemy 0.7 kill list** (AGENTS.md) is unrelated and
  untouched.

## Constraints

- **Standalone-Rust-core constraint (AGENTS.md).** `degenbot-rpc` and
  `degenbot-simulation` are core crates — zero `pyo3` (enforced by
  `just check-no-pyo3-in-cores`). No task may add `pyo3` to a core crate.
  `#[pyclass]`/`#[pyfunction]` live only in `degenbot-python`.
- **Never hold the `Dispatcher` lock across `.await`s.** The submission seam's
  convention (submit.rs:212). The sim seam's `&mut PathSuppression` touches are
  bookends (pre-filter + post-record) around the fan-out; the cutover extracts
  `PathSuppression` into its own `Arc<Mutex<…>>` so the sim seam holds *that*
  lock, not the `Dispatcher` lock, across the fan-out. See Key Decision 1.
- **PyO3 seam = arg-extract → GIL release → core call → result wrap.** No
  business logic in the wrapper layer (three-layer-transition.md §3.2). Mirror
  the existing `submission/` subtree exactly.
- **Red/Green TDD** for all behavioral changes (AGENTS.md).
- **The uv editable-installed `.so` is rebuilt by `uv` (not `cargo build`).**
  Do NOT manually rebuild with `cargo build` after Rust changes. Recovery:
  `uv sync --reinstall-package degenbot`.

## Key decisions (resolved during planning)

1. **`PathSuppression` is extracted out of `Dispatcher` into a standalone
   `Arc<Mutex<PathSuppression>>`.** It is currently an inline field of
   `Dispatcher` (not separately `Arc`'d), and `is_suppressed` takes `&mut self`
   (records the retry block on read). The submission crate's own doc comment
   on `PathSuppression` (dispatcher.rs:138) sanctions this: it is "owned by
   the Simulation epic (`5LLJHX` `5FB5MW`) per the `4JGPDW` scope — this module
   REFERENCES its shape." `PyDispatcher` holds **both**
   `Arc<Mutex<Dispatcher>>` and `Arc<Mutex<PathSuppression>>`. The sim seam
   takes the latter directly, so it never locks the `Dispatcher` across the
   sim fan-out (matching the submission seam convention). Submission stays
   unaffected: it touches `path_suppression` only via the same
   `Arc<Mutex<PathSuppression>>`.
2. **The sim seam's output is the submission seam's input shape.**
   `dispatch_profitable_py` emits `list[PySubmitCandidate]` directly (the
   `SimResult → PySubmitCandidate` join happens at result-wrap time in the
   wrapper, which has `SimulateContext.executor_address` + the originating
   candidate's `path_info` hops for `path_pools`). `PySimResult` stays
   internal. `gas_unprofitable` collapses to a count (the cockpit only logs
   it). This is the ergonomic win: the cockpit's per-block loop chains
   `dispatch_profitable_py → dispatch_and_submit_py` with no field reshuffling.
3. **`sol!` static ABI adoption** (per https://alloy.rs/guides/static-dynamic-abi-in-alloy/).
   `PyBotIo`'s typed fetchers currently hand-roll selectors + byte-decode.
   Pass A adopts `sol! { function getReserves() …; }` + `#[sol(rpc)]` in
   `degenbot-rpc` for the handful of fetcher ABIs (`getReserves`/`slot0`/
   `liquidity`/`balanceOf`/`totalSupply`/`allowance`), giving compile-time-safe
   typed calls (~10× faster than ethers, zero JSON). Not a blocker for web3py
   retirement (the fetchers already work); adopted as the ergonomic upgrade the
   boundary work implies.
4. **Scope split: Pass A (boundary) vs Pass B (type/exception sweep).** Pass A
   retires web3py from the runtime provider path and the example. Pass B drops
   the `web3` dependency. They are sequenced (A before B) so the ~48-file
   type/exception sweep follows the architectural win, not blocks it.
5. **The 4 example sim-fail diagnostic `make_request` sites (L2125–2180) route
   to existing `PyBotIo` typed fetchers** (`fetch_v2_reserves`,
   `fetch_v3_slot0_liquidity`), not to new `make_request` exposure. They are
   `try/except: pass` log lines, never load-bearing.

## Risk (highest-risk tasks)

- **A3 (`PathSuppression` extraction)** — touches the submission crate's
  `Dispatcher`. Mitigation: the doc comment sanctions it; TDD on the
  submission seam; the extractor keeps submission's access via the shared
  `Arc<Mutex<PathSuppression>>`.
- **B4 (retire the wrapper)** — broad blast radius on `Bot`/`AsyncBot`
  constructors + every construction site. Mitigated by B6 (test migration)
  proving correctness before C6 (dep removal) lands.
- **C3 (AnvilFork off web3)** — anvil RPC (`evm_mine`/`anvil_reset`/
  `evm_revert`/`evm_snapshot`) over alloy `make_request`; IPC behavior parity.
  Mitigation: keep AnvilFork's anvil *process* management as-is; only swap the
  IPC client from web3 to alloy.

## Existing ergo references

- The simulation crate's module docs cite simulation-epic task IDs `5LLJHX` /
  `5FB5MW` and scope `4JGPDW`. If those tasks exist in `.ergo`, the candidate-1
  tasks here are their PyO3-seam + routing completion (the unfinished sub-steps
  B + C). Reconcile/ reparent if appropriate during A1.

---

# [sim] A1 — Simulation PyO3 module skeleton + Cargo dep

## Goal
- Add `degenbot-simulation` as a dependency of `degenbot-python` and create the
  `simulation/` PyO3 binding module skeleton, mirroring the existing
  `submission/` subtree layout. Register the submodule in `c_api.rs`.

## Context
- `rust/crates/degenbot-python/Cargo.toml` has **no** `degenbot-simulation`
  dependency line (confirmed by grep). It already pulls `degenbot-executor`,
  `degenbot-rpc`, `degenbot-submission`, so `PathInfo`/`EncodeOptions`/
  `AlloyProvider`/`PathSuppression` are transitive.
- Mirror `rust/crates/degenbot-python/src/submission/mod.rs` for the module
  registration pattern.
- File layout:
  `rust/crates/degenbot-python/src/simulation/{mod.rs,context.rs,candidate.rs,outcome.rs,dispatch.rs}`.
- Resolve the `.ergo` task-id reconciliation noted in the epic body
  (`5LLJHX`/`5FB5MW`/`4JGPDW`) — check `ergo show` on those IDs; if they are
  the simulation epic, note this task completes their unfinished seam sub-step.

## Acceptance Criteria
- `degenbot-python/Cargo.toml` adds `degenbot-simulation = { path = "../degenbot-simulation" }`.
- `simulation/mod.rs` exists and is registered in `c_api.rs` (the module
  imports without symbols yet).
- `cargo build -p degenbot-python` succeeds; `just check-no-pyo3-in-cores`
  stays green (no pyo3 added to `degenbot-simulation` — it has none).

## Validation Gates
- `just lint-rust`
- `uv run python -c "import degenbot_rs; degenbot_rs.simulation"` (module
  visible, no error)

---

# [sim] A2 — PySimulateContext + PyDispatchCandidate + result pyclasses

## Goal
- Implement the PyO3 seam's pyclasses: `PySimulateContext` (config bag),
  `PyDispatchCandidate` (builder), and the result wrap classes
  `PyDispatchOutcome` (public, read-only) + `PySimResult` (internal — built
  and immediately converted to `PySubmitCandidate`, never returned to Python).

## Context
- `PySimulateContext` mirrors `PyDispatcher::for_block` but session-long:
  holds `Arc<AlloyProvider>` (cloned via `provider_arc()` from a
  `&PyAsyncAlloyProvider` at construction) + `executor_owner`/`executor_address`/
  `weth_address`/`pool_manager_address`/`multicall3_address`/`inject_code`/
  `injected_address`/`executor_runtime_bytecode: Bytes`.
- `PyDispatchCandidate` mirrors `PySubmitCandidate`'s `#[new]`: holds
  `path_id`, `optimal_input`, `inp`/`engine_profit`/`hop_outputs`/`solve_block`/
  `path_info` (Python dataclass, extracted via `extract_path_info` like the
  executor seam), `encode_options` (built from bool flags).
- `PyDispatchOutcome`: `gas_profitable: list[PySubmitCandidate]` (direct handoff
  to `dispatch_and_submit_py`), `gas_unprofitable_count`/`exception_count`/
  `fail_count`/`candidate_count`/`suppressed_count`/`thin_dropped: int` (the
  `[sim]` summary line), `fail_buckets: dict[str,int]` (D4 stays-python
  rendering input). `PySimResult` is the internal join source.
- Mirror the `submission/` pyclass GIL discipline: hold `Arc<...>`, release GIL
  for core calls. **No business logic** in these classes.

## Acceptance Criteria
- All four pyclasses compile and register in `simulation/mod.rs`.
- `PySimulateContext::#[new]` accepts `(provider: &PyAsyncAlloyProvider,
  executor_owner: str, executor_address: str, weth_address: str,
  pool_manager_address: str, multicall3_address: str, inject_code: bool,
  executor_runtime_bytecode: bytes, injected_address: Option<str>)` and clones
  the provider arc.
- `PySimResult` is **not** exposed in the `.pyi` (internal only).

## Validation Gates
- `just lint-rust`
- `just test-rust`

---

# [sim] A3 — Extract PathSuppression out of Dispatcher into its own Arc

## Goal
- Move `PathSuppression` from an inline field of `Dispatcher` into a standalone
  `Arc<Mutex<PathSuppression>>` held by `PyDispatcher` alongside the
  `Arc<Mutex<Dispatcher>>`. Both the submission seam and the sim seam access it
  via this shared handle, so the sim seam never locks the `Dispatcher` across
  its fan-out.

## Context
- `PathSuppression` is currently `path_suppression: PathSuppression` inline in
  `Dispatcher` (dispatcher.rs:236+). `is_suppressed(&mut self, …)` records the
  retry block on read (mutates `last_retry_block`).
- Inside `dispatch_profitable_results`, `&mut path_suppression` is touched only
  at the bookends: step 1 pre-filter (`candidates.retain(|c|
  !path_suppression.is_suppressed(c.path_id, current_block))`) and step 6
  record (`record_success`/`record_failure`). Across the fan-out (steps 2–5,
  `.buffer_unordered().collect().await`) the `&mut` is dormant.
- The submission crate's doc comment on `PathSuppression` (dispatcher.rs:138)
  sanctions ownership by the simulation scope.
- `PyDispatcher` (the pyclass) gains a `suppression_arc()` accessor returning
  `Arc<Mutex<PathSuppression>>`.
- Submission access: update any submission-side `path_suppression` touch to use
  the shared `Arc<Mutex<PathSuppression>>`. Verify submission tests stay green.

## Acceptance Criteria
- `Dispatcher` no longer has `path_suppression` as an inline field;
  `PyDispatcher` holds `Arc<Mutex<PathSuppression>>` as a sibling.
- `core::dispatch_profitable_results` signature takes
  `path_suppression: &Arc<Mutex<PathSuppression>>` (or `&mut` acquired at the
  bookends only, never held across the fan-out `.await`s).
- `is_path_blocked`/`claim_nonce`/`reserve_pools`/`track_task` in the
  submission seam are unaffected (they touch `Dispatcher`, not suppression).
- Submission tests pass; the no-lock-across-awaits convention holds.

## Validation Gates
- `just test-rust` (focus: `degenbot-submission` + `degenbot-simulation`)
- `just lint-rust`

---

# [sim] A4 — dispatch_profitable_py pyfunction

## Goal
- Implement the `dispatch_profitable_py` async pyfunction that wraps the core
  `dispatch_profitable_results`, joining `SimResult → PySubmitCandidate` at
  result-wrap time, and returns `PyDispatchOutcome`. GIL discipline mirrors
  `dispatch_and_submit_py` exactly (arg-extract → `py.detach` → core call →
  wrap).

## Context
- Signature (Python-facing):
  `async def dispatch_profitable_py(candidates: list[PyDispatchCandidate],
  context: PySimulateContext, dispatcher: PyDispatcher, current_block: int,
  min_profit_net: int, min_profit_margin_bps: int) -> PyDispatchOutcome`.
- Internally: build `SimulateContext` borrowing from `PySimulateContext`;
  build `DispatchCandidate`s from `PyDispatchCandidate`s; call
  `core::dispatch_profitable_results(&candidates, &ctx, suppression_arc,
  current_block, …) → DispatchOutcome<SimResult>`; join each surviving
  `SimResult` → `PySubmitCandidate` (executor_address from `SimulateContext`,
  path_pools from the originating candidate's `path_info.hops` — the exact
  derivation at example L1719/L2476); assemble `PyDispatchOutcome`.
- `gas_used` source: confirm whether `PySubmitCandidate` expects raw sim
  `gasUsed` or the 1.5× `inflated_gas()`. `SimResult` exposes both — pick
  whichever `dispatch_and_submit` consumes (30-second lookup at impl time).
- Register as `#[pyfunction]` in `simulation/dispatch.rs`; wire into `mod.rs`.

## Acceptance Criteria
- `dispatch_profitable_py` registered as a pyfunction; appears in the `.pyi`.
- The function releases the GIL for the duration of the core call.
- The `SimResult → PySubmitCandidate` join produces candidates whose fields
  match what `dispatch_and_submit_py` consumes (verified by a handoff test).
- No business logic in the wrapper — pure delegation.

## Validation Gates
- `just test-rust` (pyo3 seam test: mock core, verify join + GIL release)
- `just lint-rust`

---

# [sim] A5 — Route example sim through the seam; delete the Python copy

## Goal
- Rewrite the example's per-block dispatch to call `dispatch_profitable_py`,
  then `dispatch_and_submit_py` on `outcome.gas_profitable`. Delete the
  Python `simulate_one` (846 lines), `dispatch_profitable_results` (1055
  lines), and `build_simulation_state_overrides`.

## Context
- Example hot loop (post-candidate-1):
  ```python
  candidates = [PyDispatchCandidate(pid, inp, prof, ho, sb,
                    engine_registry.paths[pid], opts)
                for (pid, inp, prof, ho, _ci, sb) in results[:MAX_SIMULATE_CONCURRENT]]
  outcome = await dispatch_profitable_py(
      candidates, context=self._sim_ctx, dispatcher=self.dispatcher,
      current_block=block, min_profit_net=MIN_PROFIT_NET,
      min_profit_margin_bps=MIN_PROFIT_MARGIN_BPS)
  _render_sim_summary(outcome)  # ports the existing [sim] line from fail_buckets + counts
  records = await dispatch_and_submit_py(
      candidates=outcome.gas_profitable, dispatcher=self.dispatcher,
      provider=self.async_alloy, signer=self.signer,
      operator_nonce=self.operator_nonce, current_block=block,
      dry_run=self.dry_run, inject_code=INJECT_EXECUTOR_CODE)
  ```
- `_render_sim_summary(outcome)` ports the existing `[sim] … N failed … by
  reason: {breakdown}` formatting (D4, stays Python).
- Deleted functions also remove their `web3.Web3.keccak(text=...)` sim-side
  selector computations (`execute`/`getEthBalance`/`balanceOf(address,uint256)`
  — those selectors live in the Rust crate's `calldata.rs`).

## Acceptance Criteria
- `simulate_one`, `dispatch_profitable_results`, `build_simulation_state_overrides`
  removed from the example; the sim path reaches the chain only via the Rust
  crate.
- `_render_sim_summary` reproduces the prior `[sim]` log line format from
  `PyDispatchOutcome` fields.
- The example runs end-to-end against a fork (dry-run) and produces the same
  profitability decisions as before (per the parity test A6).

## Validation Gates
- `just test-python` (example smoke / parity)
- `just lint`

---

# [sim] A6 — Sim-seam parity tests

## Goal
- Prove the Rust seam's profitability decisions match the deleted Python
  reference on a captured block, so the routing is a behavior-preserving
  cutover.

## Context
- Capture a representative block's solver-output batch (path candidates +
  expected profitable subset) as a golden fixture. Run it through
  `dispatch_profitable_py` and assert the `gas_profitable` set + gross/net
  figures match the pre-deletion Python reference (capture the reference output
  before deleting the Python copy in A5).
- Cover the suppression bookends: a path that is pre-filtered as suppressed,
  and a path that records a failure and crosses the threshold.

## Acceptance Criteria
- A golden-fixture parity test exists and passes; it pins the profitable subset
  + gross/net/gas/priority-fee figures.
- A suppression-path test exists (pre-filter + threshold-crossing record).

## Validation Gates
- `just test-python` (the parity test)
- `just test-rust`

---

# [rpc] B1 — PyBotIo native alloy bodies (slice 14b/14c)

## Goal
- Complete the 14b/14c cutover: `PyBotIo` holds `AlloyProvider` directly and
  runs native Rust method bodies for every `PoolIO` method, dropping the
  `Py<PyAny>` provider + `call_kw` GIL round-trip for the alloy path.

## Context
- `PyBotIo` today:
  ```rust
  #[pyclass(name = "PyBotIo")]
  pub struct PyBotIo { /* holds Py<PyAny> provider */ fn new(provider, db, database_path) }
  ```
  Each of ~10 methods (`get_block_number`/`get_block`/`get_code`/`get_balance`/
  `call`→`eth_call`/`call_raw`/`get_logs`/`get_transaction_count`/
  `get_storage_at`) round-trips via `self.call_kw(py, "method", …)`.
- `AlloyProvider` (degenbot-rpc, pyo3-free) exposes every primitive natively:
  `get_block_number`/`get_block` (returns `EthBlock` with header.timestamp)/
  `get_code`/`get_balance`/`eth_call`/`get_logs`/`get_transaction_count`/
  `get_storage_at`. `get_block_timestamp` derives from
  `get_block(n).header.unix_timestamp()`.
- After B2, the fetchers (`fetch_v2_reserves` etc.) call `sol!`-typed methods
  instead of hand-rolled `eth_call` + byte-slice.
- `degenbot-rpc` stays pyo3-free; native bodies live in `degenbot-python`.

## Acceptance Criteria
- `PyBotIo` holds `AlloyProvider` (cloned arc) directly; the `Py<PyAny>`
  provider field + `call_kw` machinery is removed.
- Every `PoolIO` method has a native body (no Python round-trip).
- `get_block_timestamp` returns the block header timestamp without a separate
  RPC.
- Existing `PyBotIo` tests pass (pool-build choreography unchanged from the
  caller's view).

## Validation Gates
- `just test-rust` (focus: `degenbot-python` PyBotIo tests)
- `just lint-rust`

---

# [rpc] B2 — Adopt sol! static ABI in degenbot-rpc for fetcher ABIs

## Goal
- Replace `PyBotIo`'s hand-rolled selector + byte-decode fetchers with
  `sol!`-macro-typed contract calls, per the alloy static-ABI guide. The fetchers
  (`fetch_v2_reserves`/`fetch_v3_slot0_liquidity`/`fetch_v4_slot0_liquidity`/
  `fetch_token_balance`/`fetch_token_allowance`/`fetch_token_total_supply`)
  become typed `sol!`-generated calls.

## Context
- Guide: https://alloy.rs/guides/static-dynamic-abi-in-alloy/ —
  `sol! { function getReserves() external returns (uint112, uint112, uint32); }`
  + `#[sol(rpc)] contract IUniswapV2Pair { … }` gives
  `IUniswapV2Pair::new(addr, provider).getReserves().call().await`.
- New module `rust/crates/degenbot-rpc/src/abi.rs` with `sol!` definitions for
  the handful of fetcher ABIs. `degenbot-rpc` is a core crate — pyo3-free.
- `PyBotIo`'s fetchers call these typed methods, removing the manual
  `keccak(text=…)[ :4]` + `eth_call` + `bytes.fromhex(…)[a:b]` decode.
- The example's 4 sim-fail diagnostic sites (A5 may leave them routing to
  `make_request`; this task reroutes them to the typed fetchers, eliminating
  the last `web3.keccak` in the example if not already done).

## Acceptance Criteria
- `rust/crates/degenbot-rpc/src/abi.rs` exists with `sol!` definitions for the
  fetcher ABIs used by `PyBotIo`.
- `PyBotIo` fetchers use the typed calls; no hand-rolled selector computation
  in `PyBotIo`.
- `just check-no-pyo3-in-cores` stays green.
- Fetcher return values are byte-identical to the pre-refactor hand-rolled
  decode (parity test on a fork).

## Validation Gates
- `just test-rust` (fetcher parity on a fork)
- `just lint-rust`

---

# [rpc] B3 — Factory rewrite: alloy-only, delete web3 branches

## Goal
- `get_provider_from_config` becomes alloy-only; delete the web3 construction
  branches, `_fast_decode_rpc_response`, the middleware munging, the
  `optimize` param, and the `use_alloy`/`DEGENBOT_USE_ALLOY_PROVIDER` switch.

## Context
- `src/degenbot/provider/factory.py`: `get_provider_from_config` defaults
  `use_alloy` from env `DEGENBOT_USE_ALLOY_PROVIDER` (off by default) and
  branches to `Web3(HTTPProvider(...))` / `Web3(LegacyWebSocketProvider(...))` /
  `Web3(IPCProvider(...))` when off. `AlloyProvider(endpoint)` already handles
  all three schemes (http/ws/ipc — confirmed by the existing alloy branch).
- `get_async_provider_from_config` is already alloy-only — untouched.
- The `optimize` param only affects web3 (middleware clearing + fast JSON
  decode) — becomes vestigial; remove it.
- The chain-id enforcement stays (now via `AlloyProvider::get_chain_id`).

## Acceptance Criteria
- `get_provider_from_config` constructs `AlloyProvider` unconditionally; no
  `Web3`/`HTTPProvider`/`IPCProvider`/`LegacyWebSocketProvider` references.
- `_fast_decode_rpc_response` deleted; `optimize`/`use_alloy` params deleted.
- Chain-id mismatch still raises `ValueError`.
- Callers of `optimize=`/`use_alloy=` updated.

## Validation Gates
- `just test-python` (provider factory tests)
- `just lint`

---

# [rpc] B4 — Retire the provider wrapper (delete _Web3/_Alloy/_Offline adapters)

## Goal
- Delete `_Web3Adapter`, `_AlloyAdapter`, `_OfflineAdapter`, `ProviderBackend`,
  and the `ProviderAdapter`/`AsyncProviderAdapter` facades. `Bot`/`AsyncBot`
  take the `AlloyProvider`/`AsyncAlloyProvider` pyclass directly.
  `as_async_alloy()` deletes (it was identity once both sides are alloy).

## Context
- `_OfflineAdapter` wraps a `OfflineProvider` that does **not exist** in
  `src/`/`rust/` (confirmed by grep) — dead code, free deletion.
- `Bot`/`AsyncBot` do **not** call `self.provider.X` directly (confirmed —
  zero direct method-call hits in `bot.py`/`async_bot.py`); they hand the
  provider to `PyBotIo`. So the constructor change is: accept the pyclass,
  pass to `PyBotIo`.
- The example's `async_w3` direct usage (pool hot-sync `make_request` sites) is
  handled in B5 (those route to the pyclass's native `make_request`).
- `src/degenbot/degenbot_rs.pyi` `from_web3`/`from_alloy` entries for the
  adapter classes delete; the `AlloyProvider`/`AsyncAlloyProvider` entries stay.

## Acceptance Criteria
- `src/degenbot/provider/sync_adapter.py` and `async_adapter.py` deleted (or
  reduced to re-exports of the pyclasses if any residual import-site needs a
  transitional name — prefer full deletion).
- `Bot.__init__`/`AsyncBot.__init__` accept `provider: AlloyProvider`/
  `AsyncAlloyProvider`.
- `as_async_alloy()` removed from every call site (example L2468/L2722).
- No `ProviderAdapter`/`AsyncProviderAdapter`/`ProviderBackend`/
  `_Web3Adapter`/`_AlloyAdapter`/`_OfflineAdapter` symbols remain.

## Validation Gates
- `just test-python`
- `just lint`

---

# [rpc] B5 — Example async_w3 → AsyncAlloyProvider pyclass directly

## Goal
- The example holds the `AsyncAlloyProvider` pyclass where it held
  `async_w3`; the residual `make_request` pool-hot-sync sites call the
  pyclass's native `make_request`; `get_transaction_count`/`get_block` call
  the pyclass directly.

## Context
- The 2 sim `make_request` sites (L1848/L1893) are gone after A5.
- The 5 pool-hot-sync `make_request` sites (L2125/L2154/L2169/L2180 +
  reserves) are sim-fail diagnostics; route to `PyBotIo` typed fetchers
  (`fetch_v2_reserves`/`fetch_v3_slot0_liquidity`/`fetch_v4_slot0_liquidity`)
  per A5/B2, eliminating the `web3.keccak` + `make_request` there.
- `get_transaction_count` (L2770) → `async_alloy.get_transaction_count`.
- `get_block("latest")` (L706) → `async_alloy.get_block`.
- The actual per-block pool-state updater (if separate from these
  diagnostics) is a non-goal — it keeps using whatever path it has, now over
  the pyclass.

## Acceptance Criteria
- The example no longer imports `Web3` or references an `AsyncProviderAdapter`.
- No `web3.Web3.keccak` calls in the example; no `make_request` for sim or
  sim-fail diagnostics.
- Example runs end-to-end (dry-run) against a fork.

## Validation Gates
- `just test-python` (example smoke)
- `just lint`

---

# [rpc] B6 — Test migration: from_web3 → from_alloy

## Goal
- Migrate the ~25 `ProviderAdapter.from_web3(fork.w3)` test call sites to
  `from_alloy(AlloyProvider(fork.http_url))` (or direct pyclass construction
  after B4). Delete `test_from_web3_creates_adapter`. Update
  `make_bot_with_provider`.

## Context
- `AnvilFork` already exposes `http://{localhost}:{port}"` (anvil_fork.py:193)
  — the property name is to be confirmed/added if missing (`http_url` or
  similar).
- Sites: `tests/conftest.py`, `tests/uniswap/{v2,v3,v4}/*`,
  `tests/registry/*`, `tests/test_functions.py`, `tests/rust/test_provider_interface.py`.
- AnvilFork *itself* still uses web3 internally (the `.w3` connector + anvil
  RPC) until C3 — that's fine; tests switch the *provider under test* to alloy.
  `fork.w3` stays available to AnvilFork internals; tests stop consuming it.

## Acceptance Criteria
- Every `from_web3(fork.w3)` test site constructs an alloy provider from the
  fork's HTTP URL.
- `test_from_web3_creates_adapter` deleted.
- `make_bot_with_provider` updated.
- `just test-python` green.

## Validation Gates
- `just test-python`
- `just lint`

---

# [web3py] C1 — Replace web3.types imports with native types

## Goal
- Replace all `from web3.types import …` (~33 files) with native degenbot/
  stdlib types. Define a `degenbot.types.rpc` module (or extend the existing
  `degenbot/types/rpc_types.py`) for any type that needs a real home.

## Context
- Common types: `BlockIdentifier` (→ `int | str | Literal["latest","earliest",
  "pending"]`), `TxParams` (→ a degenbot `TxParams` dataclass or the alloy
  pyclass's native), `LogReceipt`/`BlockData` (→ the degenbot Rust-decoded
  shapes already produced by `degenbot-rpc`'s `log_to_py_dict` etc.).
- `src/degenbot/provider/protocols.py` already defines a `PoolIO` protocol —
  extend it / a sibling for theRPC-shape types.
- This is mechanical-but-broad; touches builders, pools, curve, uniswap, aave.
  No behavioral change — type aliases point to the same runtime shapes.

## Acceptance Criteria
- No `from web3.types import` remains in `src/`/`examples/` (non-pyi).
- `degenbot_rs.pyi` `from web3.types import …` entries replaced with native.
- `just test-python` + `just lint` green.

## Validation Gates
- `just test-python`
- `just lint`

---

# [web3py] C2 — Replace web3.exceptions with degenbot exceptions

## Goal
- Replace all `from web3.exceptions import …` (~15 files:
  `Web3Exception`/`ContractLogicError`/`TransactionNotFound`) with degenbot
  exceptions. `alloy_errors.py` maps alloy errors to the new degenbot types.

## Context
- `src/degenbot/exceptions/` already has `DegenbotError`/`DegenbotValueError`
  and infrastructure exceptions. Add `ContractLogicError`/`TransactionNotFound`
  equivalents (or reuse existing `RpcError` shapes) if absent.
- `alloy_errors.py` currently imports `web3.exceptions.ContractLogicError` as
  the target of `is_alloy_revert`/`alloy_revert_error` — switch it to raise
  the degenbot equivalent.
- Builders use `Web3Exception` as a catch-all in `except` clauses — replace
  with the degenbot base or a union.

## Acceptance Criteria
- No `from web3.exceptions import` remains in `src/`/`examples/`.
- `alloy_errors.py` raises degenbot exceptions; the revert-detection tests pass.
- `just test-python` + `just lint` green.

## Validation Gates
- `just test-python`
- `just lint`

---

# [web3py] C3 — Port AnvilFork off web3

## Goal
- `AnvilFork` reaches anvil via alloy `make_request` (over IPC/HTTP) instead of
  web3's `IPCProvider`/`AsyncIPCProvider`. The `.w3` connector is removed;
  callers use `fork.http_url` + an alloy provider. `async_w3` path ported.

## Context
- AnvilFork anvil-RPC methods: `evm_mine`, `anvil_reset`, `evm_revert`,
  `evm_snapshot` — these are anvil-specific JSON-RPC, **not** ABI calls
  (`sol!` does not apply); they go through `AlloyProvider::make_request`.
- AnvilFork's anvil *process* management (spawning, `--fork-url`, port
  selection) is subprocess-based, not web3 — stays as-is.
- This is the highest-risk Pass B task: IPC behavior parity (the `evm_mine`
  blocking semantics, the `anvil_reset` re-launch path). Keep the anvil
  process lifecycle; only swap the IPC client.

## Acceptance Criteria
- `AnvilFork` no longer imports `web3` (`IPCProvider`/`AsyncIPCProvider`/
  `AsyncWeb3`/`Web3`/`Middleware`/`RPCEndpoint`).
- `evm_mine`/`anvil_reset`/`evm_revert`/`evm_snapshot` go through alloy
  `make_request` and behave identically (tests pass).
- `.w3` removed; `http_url` is the supported connector.

## Validation Gates
- `just test-python` (anvil fork tests)
- `just lint`

---

# [web3py] C4 — Port v3_snapshot / v4_snapshot off Web3()

## Goal
- `src/degenbot/uniswap/v3_snapshot.py` and `v4_snapshot.py` construct a
  `Web3()` for log/snapshot fetching; port them to `AlloyProvider`.

## Context
- These use `Web3` for `get_logs` + block iteration. `AlloyProvider::get_logs`
  + `get_block` cover both.

## Acceptance Criteria
- Neither snapshot module imports `web3`.
- Snapshot output byte-identical to pre-refactor (parity test on a fork).

## Validation Gates
- `just test-python` (snapshot tests)
- `just lint`

---

# [web3py] C5 — Replace remaining web3.keccak selector sites

## Goal
- Replace the ~18 `web3.keccak(text=…)[ :4]` selector-computation sites with
  `sol!`-typed calls (preferred) or a `degenbot` keccak-selector helper.

## Context
- Sites: `arbitrage/types.py` (4), `builders/erc20_builder.py` (3),
  `curve/data_provider_impl.py` (2), `provider/call_helpers.py` (1),
  `balancer/swap_amounts.py` (1), example (7 — gone after B5).
- Where the surrounding call is a single-function ABI fetch, use a `sol!`
  definition (B2's `abi.rs` may already cover it); otherwise a
  `degenbot.types.selectors` helper exposing `keccak_selector("balanceOf(address)")`.

## Acceptance Criteria
- No `web3.Web3.keccak` / `.keccak(text=` calls in `src/`/`examples/`.
- Selector values byte-identical to `web3.Web3.keccak` output (parity test).

## Validation Gates
- `just test-python`
- `just lint`

---

# [web3py] C6 — Drop the `web3` dependency

## Goal
- Remove `web3 ~= 7.14` from `pyproject.toml`; confirm zero `import web3`
  residual across `src/`/`examples/`/`tests/` (non-pyi). `just test-all` +
  `just lint` green.

## Context
- Gated on C1–C5 all complete (no residual web3 usage).
- Also remove the `PLC0415` lazy-import comment (pyproject.toml:159) if it was
  only about web3/ProviderAdapter.
- This task is the dependency-removal capstone; it should not introduce
  behavior, only delete the dep + confirm.

## Acceptance Criteria
- `pyproject.toml` has no `web3` entry.
- `rg "import web3|from web3" src/ examples/ tests/ --type py -g '!*.pyi'` empty.
- `uv lock` succeeds; `uv sync --reinstall-package degenbot` succeeds.
- `just test-all` + `just lint` green.

## Validation Gates
- `just test-all`
- `just lint`
- `rg "import web3|from web3" src/ examples/ tests/ --type py -g '!*.pyi'` (empty)