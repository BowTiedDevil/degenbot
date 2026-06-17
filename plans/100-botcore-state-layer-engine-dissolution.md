# Plan 100: BotCore State Layer & Engine Dissolution (ADR-003)

## Overview

Execute ADR-003: make `BotCore` the single Rust owner of pool and token state, peer to `UniswapEngine` (which keeps solving + the pump). Dissolve the three block engines — their pool-state HashMaps and their buffer/apply/verify trinity move into `BotCore` as a first-class `LiquidityMap` concern (one per CL family); V2's engine deletes entirely. Wire reorg rollback into the live hot path via the `eth_subscribe` `removed`-flag. Delete the legacy `RustPoolCache` mirror (`ArbitragePath` keeps working on pure-Python fallback). Complete V3/V4 single-pool swap calc. The end state: one Rust state core, one pump-driven engine, thin Python handles (`PyPool`/`PyToken`/`PyBotCore`).

This plan does **not** touch the Python `Bot` session class or its registries — out of scope, tracked in `TODO-69cc2bea` as a follow-up session (prereq: this plan's S1).

## Problem

### Deletion test

If you delete `V2BlockEngine.pools`, `V3BlockEngine.pools`, `V4BlockEngine.pools`, `V3BlockEngine.pool_addresses`, `V4BlockEngine.pool_ids`, the three engines' `register_pool`/`apply_*`/buffer-drain methods, `RustPoolCache` (Rust PyO3), `ArbPoolCacheAdapter` (Python), `ArbSolver`'s registered-path surface (`register_pool`/`update_pool`/`solve_registered`/`solve_cached`/`solve_cached_batch`/`solve_registered_ints`/`get_pool_cache`/`update_path`/`update_all_paths`/`remove_pool`/`remove_path`), and `BotCore`'s V3 `calculate_tokens_out` stub — does the system keep working? **Yes**, because every live consumer (production `eth_backrun_v2_v3_v4_rust.py`) routes through `UniswapArbEngine.register_*_pool` + `solve_path`, and the only library consumer of `RustPoolCache`'s registered-path APIs (`ArbPoolCacheAdapter`) populates a cache nobody reads. Deletion concentrates complexity into `BotCore`/`UniswapEngine` rather than spreading it across three parallel state stores.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| Three parallel Rust pool-state stores (one per block engine) duplicating V2/V3/V4 reserves | `rust/src/optimizers/{v2,v3,v4}_block_engine.rs` `pools: HashMap<...>` | Sync invariants across copies; no single source of truth; `BotCore` (designed for this) sits unused |
| `BotCore` instantiated nowhere; its V3 calc returns `U256::ZERO` ("Slice 7 not implemented") | `rust/src/bot_core/mod.rs:421,484` | A designed-but-unfinished state layer; the live path runs on engines-as-state-owners instead |
| V4 has an orphan solve subsystem (`paths`, `results`, `rebuild_and_solve`) called only from backfill | `rust/src/optimizers/v4_block_engine.rs:365-380`, `rust/src/optimizers/uniswap_engine/event_routing.rs:333` | Parallel solve path distinct from `UniswapEngine.path_resolved`/`solver_dispatch`; must reconcile during dissolution, not delete |
| `removed: bool` on `eth_subscribe` logs never read | `rust/src/optimizers/uniswap_engine_pump.rs` (only test fixtures set it) | Canonical reorg signal dropped on the floor; pump applies events forward only, no rollback |
| Reorg journal (`ReorgJournal`) latent in `BotCore` — `update_v2_pool`/`update_v3_pool` push deltas but are never called from the live path | `rust/src/bot_core/mod.rs:330,366` | Designed-but-dead rollback capability; no V4 support |
| Legacy solve path is a second Rust backend (`RustPoolCache`) populated by Python subscriber (`ArbPoolCacheAdapter`) | `rust/src/optimizers/mobius_py.rs` `PyPoolCache`, `src/degenbot/arbitrage/optimizers/pool_cache_adapter.py` | Third live copy of pool state; `update_all_paths` is a load-bearing ordering invariant; registration mirrors reserves across FFI on every Pool State Message |
| `PyToken` is a stub returning only `address` (already on `PoolEntry`) | `rust/src/bot_core/py_token.rs` | Dead-weight PyO3 surface; doesn't expose the metadata Rust needs for decimal normalization / equivalence rules |
| Verify functions (`liquidity_verifier::*`) triggered ad-hoc from 9 call sites in `py_binding.rs` | `rust/src/bot_core/liquidity_verifier.rs`, `rust/src/optimizers/uniswap_engine/py_binding.rs` | Verification wiring smeared across the engine's registration code; no first-class "pool-state accuracy" concept |
| Stale glossary — `StateHistory`/`V2Snapshot` terms describe code that no longer exists | `rust/CONTEXT.md` (now updated) | Next reader inherits a wrong mental model |

## Solution

### Anatomy: `LiquidityMap` as the cross-cutting accurate-state owner

`BotCore` gains one `LiquidityMap<V3PoolState>` (keyed by `Address`) and one `LiquidityMap<V4PoolState>` (keyed by `(Address, PoolId)`). V2 has no `LiquidityMap` — reserves are scalar, settled in `PoolEntry::V2` directly with `ReorgJournal<V2BlockDelta>`. Each map bundles three coupled operations:

```rust
// Conceptual — concrete types in rust/src/bot_core/liquidity_map.rs
pub struct LiquidityMap<K, S> {
    state: HashMap<K, S>,              // authoritative current state + cached_tick_ranges
    buffer: LiquidityEventBuffer<K, BufferedLiquidityUpdate>,
    coverage: HashMap<K, PoolTickCoverage>,
}

impl<K, S> LiquidityMap<K, S> {
    pub fn apply_swap(&mut self, ...) -> Option<u64>          // mutate + invalidate cache + push journal delta
    pub fn apply_liquidity_update(&mut self, ...) -> Option<u64>  // tick_data mutation via update_tick_liquidity
    pub fn buffer_backfill_update(&mut self, ...)
    pub fn buffer_pump_update(&mut self, ...)
    pub fn apply_backfill_buffer(&mut self, k: &K)            // staged drain #1 (snapshot→backfill boundary)
    pub fn apply_pump_buffer(&mut self, k: &K)               // staged drain #2 (→ pump current block)
    pub fn expire_buffered(&mut self, current_block: u64)
    pub async fn verify_against_onchain(&self, ... block) -> Result<...>  // collapses liquidity_verifier free fns
    pub fn restore_before_block(&mut self, k: &K, block: u64) -> Option<...>  // reorg rollback
    pub fn get_pool(&self, k: &K) -> Option<&S>              // read-only — engine uses for re-derive
}
```

`UniswapEngine` becomes a *consumer*: `apply_log` calls `bot_core.v3_map.apply_swap(...)` / `apply_liquidity_update(...)` under the engine-then-core lock order; `solve_dirty` re-derives via `bot_core.v3_map.get_pool(&key).build_int_v3_sequence(zfo, 10)` (the per-pool derivation cache travels with the state value, including `cached_tick_ranges` invalidation).

### Design decision: who reads vs who mutates (`Pool's authority over its own math`)

A pool instance owns its state and its single-pool swap math. The solve engine reads state **by reference** (via `LiquidityMap::get_pool` returning `&S`) for path-level optimization or threads pool-to-pool by calling each pool's `calculate_tokens_out` — but never mutates pool state arbitrarily during a solve. **Only `LiquidityMap::apply_*` methods mutate, and every mutation pushes a delta to the `ReorgJournal`.** This rule prevents a future engine implementer from corrupting the journal or `cached_tick_ranges` mid-solve.

### Design decision: lock topology (Option A, per-call)

Per WS log: pump holds engine lock, briefly nests core lock, mutates BotCore inline (no buffer, no lag), releases both. At `solve_dirty`: engine lock + one core-lock window for the re-derive of all affected paths (single consistent snapshot), then `solve_path` runs `&self`-pure over the per-path resolved cache as today. **Option B (engine-side transient mutation buffer)** was rejected: the perf benefit was illusory at mainnet scale (uncontended `parking_lot::Mutex` ~25ns × 15 logs/block ≈ 375ns/block vs 12s block time), and a mut-flusk on the engine muddies ADR-003's "engine owns no pool state" claim. Option A preserves the eager-processing invariant literally — BotCore is current the instant `apply_log` returns.

### Design decision: reorg (Option α, 32-block journal, `removed`-flag detection)

Pump detects reorgs by reading `log.removed` on every WS log (authoritative — no false positives from out-of-order delivery; catches removed logs whose block number is still ≥ `last_solved_block`). On detection, pump calls `engine.handle_reorg(target)` under the engine lock, which (1) acquires core, calls `core.restore_all_pools_before_block(target)` per-pool (idempotent — untouched pools are no-ops), (2) invalidates all `path_resolved`, (3) the next `solve_dirty` re-derives and re-solves; `compute_diff_and_send` naturally emits `expired`/`removed`/`updated` diffs against `delivered`. **Journal depth is user-configurable, default 32 blocks (1 mainnet epoch)**. Reorgs deeper than depth fail-stop the pump with a diagnostic. Mid-block reorg window accepted as inherent to eager processing — the journal recovers, the alternative (defer apply until finality) destroys the zero-latency property.

### Design decision: legacy retirement is deletion, not migration (Option D)

Delete `RustPoolCache` PyO3, `ArbPoolCacheAdapter`, and `ArbSolver`'s registered-path surface. `ArbSolver.solve(SolveInput)` retained on pure-Python f64 (the Rust fast paths were already removed long ago). `ArbitragePath` unchanged — it builds `SolveInput` from pool state at solve time and never used the mirror. Migration (Option M) was rejected: would re-pollute `BotCore` with solver-shaped concerns (path registry + registered-path solve) that ADR-003 keeps on `UniswapEngine`; `BotCore` owns *state*, the engine owns *solving*.

### Design decision: token state splits along the pool line

`BotCore.tokens: HashMap<Address, TokenEntry>` (address/decimals/symbol/name) stays Rust-owned — Rust-core computation wants decimal normalization + token-equivalence rules; without Rust token state those round-trip Python on every comparison. `PyToken` is completed (decimals/symbol/name getters). Python's `Erc20Token` wraps a `PyToken` and keeps the price oracle (`ChainlinkPriceContract`, an I/O construct) + display concerns. Same line as `PyPool` (Rust: state+math) vs `Bot`/`PyBotCore` (Python: I/O orchestration). `build_erc20token` stays Python-side as the construction entry.

### Design decision: no `LiquidityMap` trait abstraction yet

Concrete types `LiquidityMap<V3PoolState>` and `LiquidityMap<V4PoolState>` share the generic `LiquidityEventBuffer<K,U>`. A trait `PoolFamilyState` is tempting but premature — only two CL families share the shape today, and Curve's per-block-cache architecture is explicitly different (mirror-free, inline dependency resolution). Defer abstraction until a third family ports and proves the shape.

## Files Involved

**Primary:**
- `rust/src/bot_core/mod.rs` — `BotCore` gains `v3_map`/`v4_map` fields; `PoolEntry::V4` added; `v2`/`v3` register/update/restore methods updated to live-map backed; reorg `restore_all_pools_before_block` added
- `rust/src/bot_core/liquidity_map.rs` — **new** — the `LiquidityMap<K, S>` struct + apply/buffer/drain/verify/restore API; absorbs `liquidity_event_buffer.rs` usage and `liquidity_verifier.rs` as methods
- `rust/src/bot_core/state_history.rs` — `V4BlockDelta` added; `ReorgJournal` depth made configurable (default 32)
- `rust/src/bot_core/py_bot.rs` / `py_pool.rs` / `py_token.rs` — `PyToken` completed (decimals/symbol/name getters); `register_v3_pool`/`register_v4_pool` thin-wrap the map registration → staged drain → optional verify sequence; V3/V4 `calculate_tokens_out`/`calculate_tokens_in` implemented (not `U256::ZERO`)
- `rust/src/optimizers/uniswap_engine/event_routing.rs` — `apply_log` retargets to `bot_core.{v2,v3,v4}_map.apply_*`; `process_backfill_logs` similarly; `handle_reorg(target)` added
- `rust/src/optimizers/uniswap_engine_pump.rs` — read `log.removed` in the `WsEvent::Log` arm; call `engine.handle_reorg(target)` on true; expose journal-depth config
- `rust/src/optimizers/{v2,v3,v4}_block_engine.rs` — **dissolved**: V2 deletes, V3/V4 reduce to free functions or fold the surviving `build_int_v3/v4_sequence` + `cached_tick_ranges` into `V3PoolState`/`V4PoolState` (where they already live)
- `rust/src/optimizers/v4_block_engine.rs` — orphan solve subsystem reconciled: `rebuild_and_solve` at `event_routing.rs:333` moves onto `UniswapEngine.path_resolved`/`solver_dispatch`, or confirmed dead if backfill doesn't need it
- `rust/src/optimizers/mobius_py.rs` — `PyPoolCache`/`PyIntHopState`/`PyArbResult` + their `solve_registered`/`solve_cached` PyO3 surface **deleted**
- `rust/src/lib.rs` — remove `BotCore`/`RustPoolCache` registration surfaces from the module init appropriately
- `src/degenbot/arbitrage/optimizers/pool_cache_adapter.py` — **deleted**
- `src/degenbot/arbitrage/optimizers/solver.py` — registered-path methods deleted; `ArbSolver.solve(SolveInput)` retained
- `src/degenbot/arbitrage/path/arbitrage_path.py` — `_solve_in_subprocess` docstring updated (no longer mentions `RustPoolCache`); behavior unchanged

**Secondary:**
- `rust/src/bot_core/liquidity_verifier.rs` — kept as pure free functions but called only from inside `LiquidityMap` methods (re-exported or inlined); the 9 `py_binding.rs` call sites collapse
- `src/degenbot/types/pool_protocols.py` — `CacheablePool` protocol: deletion-order verified (see Slice S4); possibly deleted if it had only the adapter as consumer
- `src/degenbot/arbitrage/CONTEXT.md` — Pool Cache Adapter term marked removed
- `examples/eth_backrun_v2_v3_v4_rust.py` — `EngineRegistry` may simplify once `PyPool` is the construction entry (but that migration is tracked in `TODO-69cc2bea`; this plan only verifies the example still works end-to-end)

**No change needed:**
- `rust/src/optimizers/uniswap_engine/result_channel.rs` — result batching stays on `UniswapEngine` (`result_tx`, `delivered`, `compute_diff_and_send`), confirmed solver-shaped not state-shaped
- `rust/src/optimizers/mobius_int_exact.rs`, `mobius_v3_int.rs`, `mobius_int.rs` — pure solver math, engine-internal, unaffected
- `rust/src/optimizers/uniswap_engine/solver_dispatch.rs` — `solve_path`/`resolve_path` shape preserved; `resolve_path` now reads from `bot_core.v3_map.get_pool` instead of `v3_engine.get_pool`

## Implementation Order

### Slice 1: V2 consolidation + reorg-restore on the live path

Smallest blast radius — V2 has no buffer, no tick ranges. Proves the dissolution pattern.

1. Hoist V2 state from `V2BlockEngine.pools` into `BotCore.pools`. `apply_log`'s V2 Sync branch calls `bot_core.apply_v2_sync(...)` under engine-then-core locks; the existing delta-pushing `update_v2_pool` moves to be the live path (it already exists latent).
2. Implement `engine.handle_reorg(target)` for V2 — `core.restore_all_pools_before_block(target)` per-pool, invalidate `path_resolved` for affected paths, next `solve_dirty` re-derives.
3. Wire `log.removed` reading in the pump's `WsEvent::Log` arm → `engine.handle_reorg(log.block_number)` when true.
4. Delete `V2BlockEngine` entirely (no non-state concerns to preserve).
5. Run: `just test-rust` — expect the V2 engine tests adapted to drive `BotCore` instead; new reorg test asserts a `removed: true` log fires rollback + re-solve producing an `expired` diff.

### Slice 2: V3 consolidation + `LiquidityMap` extraction

1. Introduce `rust/src/bot_core/liquidity_map.rs` with `LiquidityMap<Address, V3PoolState>`. Move V3 pool state from `V3BlockEngine.pools` into the map. Fold the `LiquidityEventBuffer<Address, BufferedV3LiquidityUpdate>` + drain logic + `apply_backfill`/`apply_pump`/`expire_buffered` into map methods. Fold the 4 V3 `liquidity_verifier::*` call sites into `LiquidityMap::verify_against_onchain`.
2. `apply_log`'s V3 Swap branch calls `bot_core.v3_map.apply_swap(...)`; the Mint/Burn branches call `apply_liquidity_update` (or buffer if unregistered). `resolve_path`'s V3 hop reads `bot_core.v3_map.get_pool(key).build_int_v3_sequence(zfo, 10)`.
3. Implement V3 single-pool calc (`calculate_tokens_out`/`calculate_tokens_in`) on `BotCore` using CL swap math (delegates to existing `cl_lib` math, no new math invention). Slice 04 to be deleted — the `U256::ZERO` stub goes.
4. Add `V3LiquidityMap::restore_before_block` to the reorg path; extends Slice 1's `handle_reorg` to V3 (per-tick priors reverse-apply).
5. Delete `V3BlockEngine` (the surviving `build_int_v3_sequence` + `cached_tick_ranges` already live on `V3PoolState`).
6. Run: `just test-rust` — expect the V3 engine tests adapted; new per-tick reorg test asserts Mint/Burn priors restore correctly on a mid-range reorg.

### Slice 3: V4 consolidation + orphan-solve reconciliation

1. Add `LiquidityMap<(Address, PoolId), V4PoolState>`. Same shape as V3 — state + buffer + drain + verify.
2. Implement V4 single-pool calc (same CL math as V3, V4 sign convention negative for exact-input per CONTEXT ruling).
3. **Reconcile the orphan solve subsystem**: `v4_engine.rebuild_and_solve` at `event_routing.rs:333` is called during backfill. Decide (slice-time): move onto the unified `UniswapEngine.path_resolved`/`solver_dispatch` (preferred — single solve path), or confirm dead and remove the backfill solve call. Either way the V4 path registry must be `UniswapEngine.path_pools`, not a V4-engine-local `paths`/`results` map.
4. Add `V4BlockDelta` to `state_history.rs`; wire V4 into `handle_reorg`.
5. Delete `V4BlockEngine`.
6. Run: `just test-rust` — the V4 backfill tests must still solve V4 paths (via the unified engine now, not the deleted subsystem).

### Slice 4: Legacy retirement (Option D)

1. Verify `CacheablePool` protocol's only consumer is `ArbPoolCacheAdapter` (`rg -n "CacheablePool" src/degenbot/` confirms or refutes). If yes, plan its deletion; if the V2/V3/Aerodrome pool classes' `reserves_for_cache`/`fee_for_cache` methods are exposed elsewhere, keep the methods but the protocol becomes orphaned (delete the protocol only).
2. Delete `src/degenbot/arbitrage/optimizers/pool_cache_adapter.py`.
3. Delete the Rust `PyPoolCache`/`PyIntHopState`/`PyArbResult` PyO3 classes + registered-path methods from `rust/src/optimizers/mobius_py.rs`; remove from `rust/src/lib.rs` module init.
4. Delete `ArbSolver`'s registered-path methods (`register_pool`/`update_pool`/`remove_pool`/`register_path`/`update_path`/`update_all_paths`/`remove_path`/`solve_registered`/`solve_registered_ints`/`solve_cached`/`solve_cached_batch`/`get_pool_cache`). `ArbSolver.solve(SolveInput)` retained.
5. Update `_solve_in_subprocess` docstring — no longer mentions `RustPoolCache`.
6. Run: `just test-python` + `just test-rust-python` — `ArbitragePath` construction path tests must pass; production example must still solve a path end-to-end (no `AttributeError` on the deleted methods anywhere).

### Slice 5: `PyToken` completion + `PyBotCore` finalization

1. Complete `PyToken` with `decimals`/`symbol`/`name` getters reading `BotCore.tokens`.
2. `PyBotCore.register_token` constructs `TokenEntry`; `get_token` returns `PyToken`. Verify `Erc20Token` can wrap a `PyToken` (Python-side constructor change tracked separately in `TODO-69cc2bea`, but the Rust handle must be ready for it).
3. Run: `just test-all` — expect green.

### Slice 6: Validate and clean up

1. Run `just lint` + `just test-all`.
2. `rust/CONTEXT.md` — confirm updates hold (`LiquidityMap`, `Buffered Liquidity Update`, `Verify-Against-Onchain`, `Removed-Flag`, `Reorg Journal`, `Swap Orientation`, `Pool's authority over its own math`, `BotCore`/`UniswapEngine` peer-module framing, `LiquidityMap` relationships); mark any legacy terms removed.
3. `src/degenbot/arbitrage/CONTEXT.md` — mark `Pool Cache Adapter` removed; update `ArbSolver` entry (no more registered-path surface).
4. `docs/adr/ADR-003-botcore-state-layer.md` — flip Status from `proposed` to `accepted`/`implemented` once S1-S5 land.
5. Remove any deprecated shims introduced during migration.

## Testing

### Per-slice test runs

Each slice runs `just test-rust` (S1–S3, S5) or `just test-python` + `just test-rust-python` (S4). Each slice leaves the suite green. The Rust extension auto-rebuilds on import via maturin per AGENTS.md.

### New unit tests

```rust
// rust/src/bot_core/liquidity_map.rs (inline)
#[test]
fn apply_swap_mutates_state_pushes_journal_delta_and_invalidates_tick_range_cache() {
    // What: round-trips a V3 Swap through LiquidityMap.apply_swap.
    // Why: pins the Pool's-authority-over-its-own-math rule — apply_* mutates
    //      and journals, no other mutation path exists.
}

#[test]
fn restore_before_block_undoes_apply_swap_and_per_tick_priors() {
    // What: applies a Swap + a Mint, restores to before; state matches the
    //      pre-event snapshot including the Mint-initialized tick.
    // Why: the reorg path is new; this is the core safety assertion.
}

#[test]
fn removed_flag_true_triggers_reorg_and_emits_expired_diff() {
    // What: pump sees log.removed=true → handle_reorg → solve → delivered diff
    //      contains expired for the reorged path.
    // Why: pins the removed-flag detection wiring (ADR-003).
}

#[test]
fn restore_exceeding_journal_depth_panics_or_fail_stops() { /* default 32, configurable */ }
```

```python
# tests/arbitrage/test_arb_path_still_solves_after_mirror_deletion.py
def test_arb_path_build_solve_input_runs_without_rustpoolcache():
    """What: ArbitragePath constructs an ArbSolver and solve()s without RustPoolCache.
    Why: Slice S4 deletes the mirror; the library solve path must keep working
         on pure-Python f64 fallback for unforked families."""
```

### Integration tests

- `examples/eth_backrun_v2_v3_v4_rust.py` reconstructed on a forked mainnet state (or its existing test harness) must still: subscribe, backfill, register V2/V3/V4 pools, solve paths, produce `ResultBatch` diffs. This is the end-to-end regression and the only integration test that exercises the full pump + reorg + verify pipeline.
- Existing V3/V4 engine tests in `rust/src/optimizers/uniswap_engine/tests.rs` (path resolution, batch diff computation, buffer staged drain) cover the bulk of the changes — adapted to the new `LiquidityMap` API rather than rewritten.

## Benefits

- **Locality**: pool state concentrates in `BotCore` (was spread across 3 engine structs + `RustPoolCache`). Bugs in state handling fix in one module.
- **Leverage**: one `LiquidityMap` API serves all consumers — solver engine, verification, diagnostics, future Curve port — without going through the solve engine.
- **Depth**: the engine↔core seam goes from shallow (engine owns state it shouldn't) to deep (engine owns *only* solving; all state access flows through recognized `LiquidityMap` methods). The `Pool's authority over its own math` rule makes the read-vs-mutate distinction structural.
- **Seam**: `LiquidityMap::apply_*` is the only mutation surface — a stable injectable boundary for state changes; tests cross the same seam callers do.
- **Locality** (reorg): rollback capability reaches the live path; `removed`-flag detection is authoritative, not inferred.
- **Leverage** (tokens): completing `PyToken` makes Rust-core decimal normalization + equivalence rules possible, removing per-comparison Python round-trips in a future Rust-owned `ArbitragePath`.

## Risks

- **V4 orphan solve reconciliation (Slice 3)**: `v4_engine.rebuild_and_solve` is live during backfill; deleting the engine without porting the solve to `UniswapEngine.path_resolved` silently breaks V4 cold-start. **Mitigation**: decide port-vs-delete before deleting the struct; the choice is in-slice, falsifiable by `just test-rust` against the existing V4 backfill tests.
- **Reorg restore cost**: `core.restore_all_pools_before_block(target)` is O(registered_pools × touched_ticks) — the most expensive live-path operation. On mainnet (~100 pools) this is microseconds-to-low-ms, not constant-time per-pool. **Mitigation**: per-pool idempotency (untouched pools no-op); the 32-block journal default bounds the worst case; benchmark before raising the default.
- **Legacy-path performance drop (Slice 4)**: `ArbitragePath` consumers lose the *unused* Rust-accelerated registered-path solve. **Mitigation**: verified during grilling — no library caller exercised `solve_registered`/`solve_cached`; the drop is from a speed nobody got to a speed they were always going to get. Pure-Python f64 fallback already exists for unforked families.
- **Mid-block reorg window**: accepted as inherent in eager processing. **Mitigation**: journal recovers; the alternative (defer apply until finality) destroys the zero-latency property.
- **`PyPool`/`PyToken` API completion (Slice 5)**: completing `PyToken` and implementing V3/V4 single-pool calc expands the Rust PyO3 surface. **Mitigation**: per-pool math is state-shaped and belongs on `BotCore` per ADR-003; V3/V4 calc delegates to existing `cl_lib` math, no new math to validate.
- **Lock-ordering violation surface (whole plan)**: the ADR-003 rule "no code path holds core-then-engine" is a global invariant. **Mitigation**: clippy doesn't enforce it; add a doc comment at every `engine.lock()` site; rely on the production-example end-to-end test and a `miri`-style review of the two lock-acquisition orderings.

## Relationship to Other Plans

- **Plan 079** (Rust-Owned Bot Core): **superseded-by, completed-predecessor**. 079 articulated the Polars-model goal ("BotCore owns everything") and proved the V2 thesis. This plan finishes 079 by dissolving the engines-as-state-owners that grew in during the `UniswapArbEngine` tracer-bullet phase, and by retiring the `RustPoolCache` second backend 079 left behind.
- **Plan 098** (Bulk Snapshot Transfer to Rust): **complementary, prerequisite**. 098's binary snapshot load + engine-lifecycle state machine are how `register_v3_pool`/`register_v4_pool` bootstrap `LiquidityMap` tick data; this plan moves that snapshot data from engine-side `SnapshotStore` into `LiquidityMap` (the data belongs to the map, the binary-deserialize orchestration stays on the PyO3 wrapper).
- **Plan 084** (Solver Per-Hop Outputs): **complementary**. 084 added `hop_outputs`/`consumed_inputs` to `SolvePathResult` for encoding; ADR-003 preserves that surface on `UniswapEngine` unchanged.
- **TODO-69cc2bea** (Bot/PoolRegistry migration): **follow-up, blocked-by this plan's S1**. The Python `Bot` session and its registries migrate to wrap `PyPool`/`PyToken` separately, with its own ADR.
- **ADR-001** (I/O-free pools) and **ADR-002** (pool-type-registry singleton): **orthogonal** — neither rules on FFI state ownership; ADR-003 extends their separation across the Rust seam.

## Status

[x] Slice 1: V2 consolidation + reorg-restore on the live path — **done**
    - V2 state hoisted into `BotCore`; `UniswapEngine` holds `core: Arc<Mutex<BotCore>>` and reads/writes V2 state through it (engine-then-core lock order).
    - Dual-orientation registration retired (`V2BlockEngine` deleted); paths store the single `pool_id` + `zero_for_one` (orientation derived at resolve). The legacy `reverse_id = forward_id + 1` scheme was used only by `ArbPoolCacheAdapter` (deleted in Slice 4) — production + tests were already aligned.
    - `BotCore::apply_v2_sync` (journals + mutates + returns `pool_id`) is the live mutation entry; `BotCore::get_v2_pool_state` is the read accessor the solve engine uses.
    - `handle_reorg` + `restore_all_pools_before_block` restore V2 state; `ReorgJournal::restore_before_block` fixed to handle the "first Sync at the reorg target block" case (single delta at target → restore to registration reserves, no panic); `ReorgJournal::newest_block` gates per-pool idempotent restore.
    - Pump reads `log.removed` in the `WsEvent::Log` arm → `engine.handle_reorg(log_block)` (ADR-003 Option α).
    - Side fix: `compute_diff_and_send` now actually drops `expired` paths from `delivered` (was retaining stale profitable entries) and refreshes `updated` values — fulfills the `send_result_batch_advances_delivered_to_above_threshold` contract the old code coincidentally passed.
    - Tests: new `handle_reorg_rolls_back_v2_sync_and_expires_delivered_result` (captures the `expired` batch diff) + `restore_before_block_at_earliest_returns_registration_state`; proptest model updated. 482 Rust + 62 Python engine/bot_core green; clippy clean.
[x] Slice 2a: V3 state consolidation into BotCore (structural) — **done** (deferred `LiquidityMap` struct extraction to Slice 3; V3 follows S1's inline-`PoolEntry`+`apply_*` pattern; see details in commit body)
[x] Slice 2b (part 1): V3 reorg rollback on the live path — **done**
    - `apply_v3_swap` now journals scalar priors (sqrt_price, liquidity, tick) + any per-tick priors before mutating (was non-journaling in S2a).
    - `apply_v3_liquidity_update` (Mint/Burn) journals the two affected ticks' priors (tick_lower, tick_upper) — `liquidity_gross_before: None` for newly-initialized ticks so rollback deletes them. Scalar `liquidity` is NOT journaled by Mint/Burn (V3's active `liquidity` changes only on Swap's tick-crossing).
    - Bug fix in `ReorgJournal::<V3BlockDelta>::restore_before_block`: previously returned only the *last-popped* delta's `tick_priors` (the V2 full-state pattern), silently dropping intervening deltas' tick mutations when rolling back across multiple blocks. Now accumulates tick priors across ALL popped deltas (newest→oldest, oldest wins on duplicate tick idx) and returns the oldest-popped delta's scalar priors. Surfaces now that V3 journaling is on the live path (was latent before S2b).
    - `restore_all_pools_before_block` extended to V3 (dispatches V2 vs V3 by `PoolEntry` variant); `v3_restore_before_block` now also invalidates the tick-range cache on restore.
    - Test: `handle_reorg_rolls_back_v3_swap_and_mint_to_prior_state` (Swap at block 5 + Mint at block 6 → reorg to 5 removes Mint-initialized ticks AND restores Swap scalars). 483 Rust + 352 Python + clippy clean.
[ ] Slice 2b (part 2): V3 single-pool calc (`calculate_tokens_out`/`in` over full `V3PoolState` — tick walk via `gen_ticks` + `compute_swap_step_v3` loop + liquidity-net crossing, delegating to `cl_lib::swap_math`) — NEW behavior via TDD
[ ] Slice 3: V4 consolidation + orphan-solve reconciliation + V4 single-pool calc. Extract `LiquidityMap` generic from V3+V4 duplication here.
[ ] Slice 4: Legacy retirement (delete RustPoolCache + ArbPoolCacheAdapter + ArbSolver registered-path surface)
[ ] Slice 5: PyToken completion + PyBotCore finalization
[ ] Slice 6: Validate and clean up (lint, test-all, CONTEXT sync, ADR-003 status flip)
