# Move fetch_priority_fee_percentiles to degenbot-rpc
## Goal
- Relocate the `eth_feeHistory` market-data RPC leaf out of `degenbot-simulation` into `degenbot-rpc`, where it belongs as a generic market primitive. This is the isolated, zero-behavior-change step that shrinks `degenbot-simulation`'s surface before the larger retirements/merges.

## Context
- ADR-019 D5. `fetch_priority_fee_percentiles` + `parse_block_priority_fees` currently live in `rust/crates/degenbot-simulation/src/dispatch.rs`. They are a generic `eth_feeHistory` market oracle consumed by `compute_priority_fee` (a strategy-side function, per ADR-019 D4). A sandwich/liquidation searcher wanting the same oracle should reach it through the RPC crate, not a backrun-shaped simulation crate.
- `degenbot-rpc` already owns `AlloyProvider`, `EthBlock`, the typed block fetchers — a `fetch_block_priority_fee_percentiles` leaf fits alongside them.
- This step is deliberately first: it's a pure leaf-move with no behavior change, trivially green, and front-loads a clean shrink so steps 3/4/6 touch less.

## Acceptance Criteria
- `fetch_priority_fee_percentiles` and `parse_block_priority_fees` live in `degenbot-rpc` (under `provider.rs` or a `fees` submodule — pick whichever matches the existing layout).
- `degenbot-simulation/src/dispatch.rs` no longer defines them. If `degenbot-simulation` still needs the type for its remaining content (pre-merge), it re-imports `pub use degenbot_rpc::...` — but prefer updating call sites directly.
- The `BlockPriorityFees` struct: if it lives in `degenbot-evm`/`degenbot-simulation` today, decide its home deliberately. It's a market-data type, so it likely moves to `degenbot-rpc` too; if strategy code (`compute_priority_fee`) needs it, that code will be extracted in a later step, so importing from `degenbot-rpc` is correct.
- All existing tests for the fee history parsing move with the code and pass.

## Validation Gates
- `just test-rust` — Rust tests pass.
- `just lint-rust` — clippy clean.
- `rg -n 'fetch_priority_fee_percentiles' rust/crates/degenbot-simulation/src/` returns no definition (only an import if transitional, or nothing).
- `rg -n 'fetch_priority_fee_percentiles' rust/crates/degenbot-rpc/src/` returns the new definition.

---
# Wire Inspector-based access-list collection
## Goal
- Add an `Inspector`-based access-list collector that gathers warmed SLOAD/SSTORE slots **as a byproduct of the first `transact_one` run** on the revm path, replacing the post-re-`transact` `emit_access_list_from_state` as the **primary** AL source. This is the additive prerequisite for retiring `eth_createAccessList` (step 3) without an AL gap.

## Context
- ADR-019 D3. Today the revm path's AL comes from a **separate post-`transact`** of the execute() call (`emit_access_list_from_state` on `ResultAndState.state`). That works but runs execute() twice.
- revm-41 exposes the `Inspector` trait (`step` / `call` / `call_end` hooks in `revm-inspector-41/src/inspector.rs`) and `InspectEvm::inspect(tx, inspector)` (in `revm-inspector-41/src/inspect.rs`). A custom `AccessListCollector` inspector attached to execute()'s `transact_one` collects touched addresses + storage slots in-realtime.
- Verified fact: revm-41 does NOT ship a pre-built `AccessListInspector`; the collector is custom (a small struct impl `Inspector` with `call` / `call_end` / `step` hooks recording `context.journaled_state` touches, or similar per the verified trait shape — confirm the exact hook that exposes touched storage during execution when implementing).
- The post-re-`transact` `emit_access_list_from_state` in `rust/crates/degenbot-evm/src/access_list.rs` **stays as an engine-generic primitive** (emitting an AL from a `State` journal is a general capability) — it is just no longer the production AL path.
- AL output crosses the engine→strategy seam: the engine produces the warmed-slot set; the strategy decides whether/how to attach it to the submitted tx.

## Acceptance Criteria
- A new `AccessListCollector` (or similarly descriptive name) `Inspector` impl exists in `degenbot-evm` (or its post-merge home).
- `BlockSimHandle::simulate_path` (or the equivalent execute path) collects the AL via the Inspector on the first `transact_one` run, not via a post-re-`transact`.
- The AL emitted matches the slot set the existing post-re-`transact` path emits (parity test: same addresses + storage keys for a fixture execute() call).
- The post-re-`transact` `emit_access_list_from_state` still exists and still passes its tests (it's a surviving engine primitive, not deleted here).
- No behavior change to `SimResult.access_list` content for existing fixtures.

## Validation Gates
- `just test-rust`.
- `just lint-rust`.
- A parity test asserting Inspector-collected AL == `emit_access_list_from_state` AL over a fixture.

---
# Retire the RPC simulation surface
## Goal
- Delete the RPC simulation path now that the Inspector-based AL (step 2) provides an uninterrupted in-process AL source. This retires `eth_simulateV1`, `eth_createAccessList`, the `stateOverrides` JSON builder, and the RPC-path `simulate_one` orchestration. The duplication this whole effort targeted resolves by **deletion** here, not unification.

## Context
- ADR-019 D1, D2, D3 (retirements). The in-process revm path is the sole simulation executor (in-process sim is already the production default).
- Step 2 (Inspector AL) must be done first — otherwise there is an AL gap.
- Files/functions to delete:
  - `rust/crates/degenbot-simulation/src/dispatch.rs::simulate_v1` and its `parse_simulation_result` (the `eth_simulateV1` dispatcher).
  - `rust/crates/degenbot-simulation/src/dispatch.rs::create_access_list` (the `eth_createAccessList` RPC).
  - `rust/crates/degenbot-simulation/src/lib.rs::build_simulation_state_overrides` (the Alloy `StateOverride` builder). `apply_simulation_overrides` (`CacheDB` insertion in `degenbot-evm`) is the sole override mechanism (ADR-019 D2).
  - `rust/crates/degenbot-simulation/src/payload.rs` — `build_simulate_payload`, `SimulationParams`, `SIM_CALL_COUNT` (the `eth_simulateV1` JSON payload is dead).
  - `rust/crates/degenbot-simulation/src/simulate_one.rs` — the entire file (the RPC-path orchestration; `simulate_path_on_evm` is the sole surviving shape).
  - The legacy `None` arm of `dispatch_profitable_results` (the `buffer_unordered` RPC fan-out) — only the `Some(bot_state)` revm arm survives (and it moves to `examples/` in step 5).
- `SimResult`/`SimulateContext`/`SimulatePath`/`FailBuckets` survive this step (they're used by the revm path) — they move in step 5.
- `degenbot-simulation/src/dispatch.rs::SimulatedCall` / `SimulationResult` types retire with `simulate_v1` (they were the RPC response shape).
- The MockTransport-based tests in `dispatch_profitable.rs` retire with the code they cover. The revm-path smoke tests in `simulator.rs` survive and cover the sole remaining path.

## Acceptance Criteria
- No `eth_simulateV1`, `eth_createAccessList`, `StateOverride` builder, or `simulate_one` (RPC) symbol remains in `degenbot-simulation` or `degenbot-evm`.
- `dispatch_profitable_results` has no `Option<BotState>` branch — it always uses revm (collapse the signature: `bot_state` becomes required, `warm_cache` becomes required). (If this signature collapse is awkward to land before step 5's extraction, a transitional shape that still takes `Option` but panics/`unreachable!`s on `None` is acceptable — note it in the completion summary and finish the collapse in step 5.)
- The revm smoke tests pass.
- No reachable code path produces an `eth_simulateV1` or `eth_createAccessList` RPC call.

## Validation Gates
- `just test-rust`.
- `just lint-rust`.
- `rg -n 'simulate_v1|create_access_list|build_simulation_state_overrides|build_simulate_payload' rust/crates/degenbot-simulation/src/ rust/crates/degenbot-evm/src/` returns nothing.

---
# Merge degenbot-evm into degenbot-simulation
## Goal
- Fold `degenbot-evm` into `degenbot-simulation` as an internal `sim/evm` (or similar) submodule, and retire the `pub use degenbot_evm::{10+ symbols}` re-export bridge. After step 3, there is no strategy code left in either crate; the engine code is a cohesive set moving to one home.

## Context
- ADR-019 D6. The two crates existed as an accidental split: the shared sim primitives had to pick one home and be re-exported to the other, producing a forbidden back-compat bridge (`pub use degenbot_evm::{...}` in `degenbot-simulation/src/lib.rs`) and exiling `calldata` to `degenbot-evm` only to break the dependency cycle the split itself created. This is the same "split across two crates by an accidental line" pattern ADR-015 resolved for the solver seam.
- The "eventual dispatch swap" the comments cited as justification for the bridge has shipped (in-process sim is the default, step 3 retired the RPC path). The bridge outlived its reason.
- Naming: "simulation" describes the domain; "evm" describes one implementation. The umbrella is `degenbot-simulation`; the revm adapter + its DB stack become an internal submodule.
- `degenbot-evm` is consumed by: `rust/crates/degenbot/Cargo.toml` (the umbrella), `rust/crates/degenbot-python/Cargo.toml` (via `degenbot-simulation`'s re-export). After the merge, those point at `degenbot-simulation` directly.
- `degenbot-evm-math` is a SEPARATE crate (evm-math, not evm) — do not touch it.

## Acceptance Criteria
- `rust/crates/degenbot-evm/` is deleted (or emptied and removed from the workspace).
- `degenbot-simulation` owns: `simulator`, `state_override`, `bot_state_db`, `v4_transient`, `calldata`, `access_list`, `warm_code_cache` — as an internal `sim/evm` (or chosen) submodule.
- The `pub use degenbot_evm::{...}` re-export in `degenbot-simulation/src/lib.rs` is gone; symbols are exported from their real home directly.
- `rust/crates/degenbot/Cargo.toml` and `rust/crates/degenbot-python/Cargo.toml` no longer depend on `degenbot-evm` (only on `degenbot-simulation`).
- `rust/Cargo.toml` workspace members no longer list `crates/degenbot-evm`.
- Tier-0 standalone consumer (`examples/standalone_consumer.rs` + `just test-standalone`) still reaches the sim surface.
- Tier-1 reachability test (`rust/crates/degenbot/tests/reachability.rs`) — update `INTENTIONALLY_NOT_STANDALONE` if needed; the merged crate should now be reachable.

## Validation Gates
- `just test-rust` (incl. `just test-standalone`).
- `just lint-rust` (incl. `just check-no-pyo3-in-cores`).
- `rg -n 'degenbot-evm|degenbot_evm' rust/crates/*/Cargo.toml rust/Cargo.toml` returns nothing referencing the deleted crate (the `degenbot-evm-math` matches are fine — they're a different crate).

---
# Extract backrun strategy to examples
## Goal
- Move the surviving revm backrun-strategy code out of the merged `degenbot-simulation` engine crate into `examples/`, leaving the engine with only its thin per-call execution + override-application + AL-emission surface. The engine stops owning the 7-call bundle, `decode_balance`, `compute_priority_fee`, the sim value types, and `dispatch_profitable_results`'s fan-out policy.

## Context
- ADR-019 D4. The backrun 7-call bundle (3 pre-balance → `execute()` → 3 post-balance over WETH9 / Multicall3 / PoolManager ERC6909), `decode_balance`, the gross/net profit arithmetic, `compute_priority_fee` (TARGET_PROFIT_RATIO / age-decay), `SimResult`, `SimulateContext`, `SimulatePath`, `FailBuckets`, the int128 guard, `dispatch_profitable_results` (now revm-only after step 3) + its thin-margin / suppression / categorization policy, and `filter_thin_margin_results` are **strategy** code shaped by one example bot's funding model + executor contract — not a universal simulation surface.
- No new crate. `examples/` enforces the engine-vs-strategy distinction more cheaply and more honestly than a crate (an example file is self-evidently an example, carries no Cargo/PyO3/standalone-reachability surface). Pair with the existing `examples/eth_backrun_v2_v3_v4_rust.py`.
- The engine exposes a deliberately thin surface the example composes: `BlockSimHandle::build` + generic per-call execution + `apply_simulation_overrides` + the Inspector AL output + `WarmCodeCache` / `BotStateDb` / `emit_access_list_from_state` as engine internals.
- The example re-assembles these "more manually" (the grilling caveat): constructs its own 7-call vector, decodes balances itself, sizes its own priority fee, runs its own fan-out policy.
- `dispatch_profitable_results` signature collapse (if a transitional `Option<BotState>`-shape survived step 3) finishes here — the example owns the (revm-only) fan-out directly.
- The calldata builders (`encode_cmd_stream`, `wrap_execute_calldata`, the balance-of selectors) stay in `degenbot-executor` / the engine — they're generic ABI encode, not strategy.

## Acceptance Criteria
- The 7-call bundle, `decode_balance`, `compute_priority_fee`, `SimResult`, `SimulateContext`, `SimulatePath`, `FailBuckets`, the int128 guard, `dispatch_profitable_results`, `filter_thin_margin_results`, and the categorization/suppression policy live in `examples/` (a Rust example bin or a `examples/backrun_strategy/` library module — pick whichever the workspace supports for PyO3 reach in step 6).
- `degenbot-simulation`'s public surface is the thin engine: `BlockSimHandle`, `apply_simulation_overrides`, `SimulationOverrideParams`, `WarmCodeCache`, `BotStateDb`, `emit_access_list_from_state`, the AL Inspector collector, the calldata builders it owns.
- No backrun-strategy symbol (`SimResult`, `compute_priority_fee`, `dispatch_profitable_results`, `SimulateContext` carrying executor/weth/pm addresses as first-class fields) is re-exported from `degenbot-simulation`.
- The example (Rust side) compiles and runs against the thin engine surface.
- The Python example bot still works at this step (it may still be calling `dispatch_profitable_py` which now re-exports from the example location — the full PyO3 decompose is step 6/7; a transitional re-export is acceptable here IF noted in the completion summary).

## Validation Gates
- `just test-rust`.
- `just lint-rust`.
- `just check-no-pyo3-in-cores`.
- `rg -n 'compute_priority_fee|dispatch_profitable_results|SimResult' rust/crates/degenbot-simulation/src/` returns nothing (these are strategy types now in `examples/`).

---
# Decompose PyO3 surface into engine primitives
## Goal
- Retire the monolithic `dispatch_profitable_py` (`#[pyfunction]`) and its pyclasses (`PyDispatchCandidate`, `PyDispatchOutcome`, `PySimulateContext`) which bundle fan-out + suppression + thin-margin + decode + priority-fee sizing + categorization into one opaque Rust call from Python — the shape that wedged the strategy into the engine. In their place, expose thin PyO3 wrappers over the **engine primitives** the Python driver composes.

## Context
- ADR-019 D7. `degenbot-python/src/simulation/{candidate,context,dispatch,outcome}.rs` retire.
- New primitive wrappers (thin: arg-extract → GIL release → core call → result wrap, per ADR-005 §3 C + ADR-013 FFI-seam-private):
  - `PyBlockSimHandle` exposing `build(ctx, bot_state, warm_cache)` + the generic per-call execution (execute a tx / a sequence of txs → per-call outcomes).
  - the override-application primitive (`apply_simulation_overrides` over a `CacheDB`) — a PyO3 shell to drive the `CacheDB`-based override path from Python is explicitly in scope (the recently-added functionality; more wiring is needed). The Python driver supplies `SimulationOverrideParams` (owner, executor addresses, runtime bytecode, warmup slots, funding amounts) across the FFI.
  - `fetch_priority_fee_percentiles` (now in `degenbot-rpc` per step 1) — a thin wrapper if Python needs it, OR the strategy computes priority fees in Python and only the raw p10/p50 samples cross.
  - the AL Inspector output crossing back to Python.
- `degenbot-python/Cargo.toml`'s `simulation` feature may need updating (it currently depends on `degenbot-simulation`; after step 4/5 that's the merged engine crate, which is correct).
- The PyO3 wrappers for the strategy code (if the example Rust strategy is to be Python-driven during a transitional period) would live in `degenbot-python` but import the example location — but step 7 retires that, so prefer to go straight to the decomposed primitive surface.

## Acceptance Criteria
- `dispatch_profitable_py`, `PyDispatchCandidate`, `PyDispatchOutcome`, `PySimulateContext` are deleted from `degenbot-python/src/simulation/`.
- The new primitive wrappers exist and are registered on the `_ffi.simulation` module.
- Each wrapper is thin (no business logic; arg-extract → GIL release → core call → result wrap).
- The Python-side stub using the new primitive surface exists (even if the full example rewiring is step 7).
- `degenbot-python/src/simulation/mod.rs` doc updated to reflect the new primitive surface.

## Validation Gates
- `just test-python` — PyO3-wrapped tests pass.
- `just test-rust`.
- `just lint-rust`.
- `just check-no-pyo3-in-cores`.

---
# Rewire example Python bot onto engine primitives
## Goal
- Rewrite `examples/eth_backrun_v2_v3_v4_rust.py` to compose the PyO3 engine primitives (from step 6) instead of the retired `dispatch_profitable_py`. The Python driver constructs its own 7-call vector, decodes balances, sizes its priority fee, and runs its own fan-out policy — the explicit "managed more manually" intent.

## Context
- ADR-019 D4, D7. The Python example bot currently calls `await dispatch_profitable_py(...)` and reads `PyDispatchOutcome.gas_profitable`. After step 6, it instead composes: `PyBlockSimHandle.build(...)` → `execute_calls(...)` → reads per-call outcomes → decodes balances (Python) → computes priority fee (Python, using `fetch_priority_fee_percentiles` if that crossed PyO3, or `compute_priority_fee` ported to the Python strategy) → categorizes → hands winners to the existing submission seam (`dispatch_and_submit_py`).
- The `[sim] N candidates: X ok (Y profitable, Z below threshold), W failed, V exceptions` summary rendering stays Python (D4 `stays-python` from the existing module docs).
- This is the last step because it depends on step 6's primitive surface being final.
- The strategy code extracted in step 5 (Rust side, in `examples/`) and this Python rewiring are two faces of the same example: decide whether the canonical example is the Rust one (Python is a thin driver over it via a PyO3 wrapper to the example location) or the Python one (Rust strategy extracted in step 5 is a reference impl the Python bot re-derives). Per ADR-019 D4/D7, the Python bot is the cockpit composing engine primitives; the Rust strategy in `examples/` is a reference. Pick the cleaner integration and note it.

## Acceptance Criteria
- `examples/eth_backrun_v2_v3_v4_rust.py` no longer calls `dispatch_profitable_py`.
- The bot composes the new primitive PyO3 surface end-to-end: build handle → execute the 7-call vector → decode → compute fees → categorize → submit.
- The existing `[sim]` summary rendering still produces the same output shape for a fixture block.
- End-to-end smoke: the example runs against a mock provider (or the existing test harness) and produces a profitable submission candidate for a known-good fixture.

## Validation Gates
- `just test-python` — Python tests pass.
- `just test-python` — PyO3 integration tests pass.
- `just test` for confidence.
- Manual: `examples/eth_backrun_v2_v3_v4_rust.py --help` reflects the new composition (no `dispatch_profitable_py` reference).
