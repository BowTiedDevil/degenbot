# Plan 086: Rust-owned result batch channel

## Overview

Replace Python's pull-based `latest_results()` with a Rust-owned push channel that delivers incremental result batches. Each batch contains only paths that are new, changed, expired, or removed since the last batch Python consumed — eliminating redundant FFI transfers and giving Python a high-signal stream instead of a full dump it must re-filter every block.

## Problem

### Deletion test

If you deleted `profitable_results()` and the `try_dispatch` loop, the bot would have no way to learn about solver results. The deletion test reveals that the current design is a *polling loop over a shared buffer* — Python pulls the entire result set every block, re-filters it, and re-simulates the same paths it already saw.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| Python re-simulates unchanged results | `dispatch_profitable_results` called every block with full result set | Wastes 150 RPC calls per block on paths the solver hasn't changed its mind about |
| `latest_results()` copies the entire result list across FFI | `PyUniswapArbEngine::latest_results()` | Every Python call allocates a `PyList` of N tuples — N can be 2500+, most unchanged since last call |
| Python has no diff signal | `EngineRegistry.profitable_results()` | Cannot distinguish "new result" from "same result as last block" — must simulate everything to find out |
| `profitable_results()` does Python-side filtering Rust could do | line 692 in `eth_backrun_v2_v3_v4_rust.py` | The `min_profit` / `MAX_PROFIT_WEI` filter walks every item — Rust already has this data |
| Block metadata and results arrive on separate channels | `wait_for_block()` + `latest_results()` | Python must coordinate two calls in the right order — fragile and verbose |

## Solution

### Step 1: Add `delivered` snapshot and `ResultBatch` to `UniswapEngine`

Rust already knows which paths were re-solved vs carried forward in `rebuild_and_solve_affected`. It currently discards the diff. Add a `delivered: HashMap<u64, SolvePathResult>` field that records the above-threshold results Python has consumed, and compute the diff against it.

`self.results` keeps *all* nonzero-profit paths (the solver already filters out zero-profit results). The `min_profit` / `max_profit` filter is applied at the batch boundary only — `delivered` tracks the above-threshold subset. This preserves diff semantics: a path oscillating around the profit threshold shows as `updated` (not fresh/expired churn), and Python's suppression history is preserved per `path_id`.

```rust
struct ResultBatch {
    solve_block: u64,
    timestamp: u64,
    base_fee_per_gas: Option<u64>,
    gas_used: u64,
    gas_limit: u64,
    // Paths above threshold and NOT in the previous delivered set
    fresh: Vec<(u64, SolvePathResult)>,
    // Paths above threshold in both, but any field changed (full PartialEq)
    updated: Vec<(u64, SolvePathResult)>,
    // Path IDs that were above threshold but are now below (still registered)
    expired: Vec<u64>,
    // Path IDs that were de-registered (permanently gone)
    removed: Vec<u64>,
}
```

```rust
fn compute_diff(&mut self, new_results: &[(u64, SolvePathResult)]) -> ResultBatch {
    // Above-threshold subset of new results
    let new_above: HashMap<u64, SolvePathResult> = new_results
        .iter()
        .filter(|(_, r)| r.profit > self.min_profit && r.profit < self.max_profit)
        .cloned()
        .collect();

    let new_above_ids: HashSet<u64> = new_above.keys().copied().collect();
    let delivered_ids: HashSet<u64> = self.delivered.keys().copied().collect();

    let fresh: Vec<_> = new_above
        .iter()
        .filter(|(id, _)| !self.delivered.contains_key(id))
        .map(|(&id, r)| (id, r.clone()))
        .collect();

    let updated: Vec<_> = new_above
        .iter()
        .filter(|(id, new)| matches!(self.delivered.get(id), Some(old) if old != new))
        .map(|(&id, r)| (id, r.clone()))
        .collect();

    let expired: Vec<_> = delivered_ids
        .difference(&new_above_ids)
        .copied()
        .collect();

    let removed: Vec<_> = self.deregistered.drain(..).collect();

    // Advance delivered to the above-threshold set
    self.delivered = new_above;

    ResultBatch { solve_block: self.results_block, fresh, updated, expired, removed, timestamp: 0, base_fee_per_gas: None, gas_used: 0, gas_limit: 0 }
}
```

Block metadata fields (`timestamp`, `base_fee_per_gas`, `gas_used`, `gas_limit`) are included from the start (zeroed initially, filled by `process_block` in a later slice).

### Step 2: Channel mechanism

Use `tokio::sync::mpsc` with `try_send`. If Python hasn't consumed the previous batch, the new batch is dropped — safe because the next batch will carry a correct cumulative diff from the last `delivered` state.

`rebuild_and_solve_affected` is the single batch emission point. All paths that mutate `self.results` converge there:
- `process_block()` → `rebuild_and_solve_affected()`
- `process_updates()` / `process_v4_updates()` / `process_all_updates()` → `rebuild_and_solve_affected()`
- `solve_all_paths()` → sets `self.results` directly, must also compute and send a batch
- `register_and_solve_path()` → appends to `self.results` eagerly, sets `has_unsent_results: bool` flag. The next `rebuild_and_solve_affected` call (triggered by `process_block`) will include the pending paths via `pending_new_paths` and produce a batch. The one-block latency is acceptable for a long-running bot.

### Step 3: `min_profit` filter at the batch boundary

`self.results` keeps all nonzero-profit paths (~2,568 out of ~509K registered). The batch channel filters to `profit > min_profit && profit < max_profit` at the `compute_diff` boundary. This means:
- `latest_results()` continues to return all nonzero-profit paths (tests pass unchanged)
- Python only receives above-threshold paths in batches
- Paths oscillating around the threshold show as `updated` (not fresh/expired churn)
- Python's `PathSuppression` history is preserved per `path_id`

`min_profit` and `max_profit` are configurable fields on `UniswapEngine`, set at construction time.

### Step 4: Path de-registration

Add `deregister_path(path_id: u64) -> bool` to `UniswapEngine`. Removes from:
- `self.paths`
- `self.pool_to_paths` reverse index entries for that path's hops
- `self.results`
- `self.delivered`
- `self.pending_new_paths`

Does **not** remove pool entries from the sub-engines (V2/V3/V4). A de-registered path's pools may still be used by other paths or by future paths registered during rolling-start.

De-registered path IDs are tracked in `self.deregistered: Vec<u64>` and included in the next batch's `removed` field. The `removed` signal is generic — it means "this path no longer exists in the engine." Whether Python or a future Rust-internal mechanism initiated the removal is irrelevant to the consumer.

The flow for suppression-driven de-registration:
1. Python's `PathSuppression` hits its de-registration threshold (e.g., 50 consecutive failures)
2. Python calls `engine.deregister_path(path_id)`
3. Next `process_block` → `rebuild_and_solve_affected` → path is absent from `new_results` → shows as `removed` in the batch
4. Python sees `removed` → permanently discards suppression tracking for that path

### Step 5: Expose the channel to Python as an async iterator

Implement `__aiter__` / `__anext__` directly on `PyUniswapArbEngine` (no separate iterator object). Follow the `PyAlloySubscription` pattern from `subscription_py.rs`: take the `mpsc::Receiver`, await `recv()` in a `future_into_py` coroutine (releasing the GIL), then re-acquire the GIL to convert `ResultBatch` to a Python dict.

The `mpsc::channel` is created in `PyUniswapArbEngine::new()` (Option A). The sender is passed into the engine. The receiver is stored in `PyUniswapArbEngine.result_rx: parking_lot::Mutex<Option<mpsc::Receiver<ResultBatch>>>`.

On engine stop/restart, the channel is recreated (replacing the old stale one).

### Step 6: Rewrite Python's main loop

Replace the `wait_for_block()` → `profitable_results()` → dispatch pipeline with a single `async for` loop. The full loop body:

```python
# After:
async for batch in engine_registry.engine:
    block_number = batch["solve_block"]
    block_timestamp = batch["timestamp"]
    base_fee = batch.get("base_fee_per_gas") or 0
    gas_used = batch["gas_used"]
    gas_limit = batch["gas_limit"]

    base_fee_next = next_base_fee(
        parent_base_fee=base_fee,
        parent_gas_used=gas_used,
        parent_gas_limit=gas_limit,
    )
    operator_nonce = await async_w3.eth.get_transaction_count(operator_address)

    try:
        fee_history = await async_w3.eth.fee_history(
            block_count=1,
            newest_block=block_number,
            reward_percentiles=[float(p) for p in FEE_PERCENTILES],
        )
        reward = fee_history.get("reward", [[]])
        if reward and reward[-1]:
            block_priority_fees[block_number] = dict(
                zip(FEE_PERCENTILES, reward[-1], strict=True)
            )
            if len(block_priority_fees) > FEE_HISTORY_WINDOW:
                block_priority_fees.pop(min(block_priority_fees))
    except Web3Exception:
        pass

    block_times.append((block_number, block_timestamp))
    if len(block_times) >= 2:
        oldest_bn, _oldest_ts = block_times[0]
        if block_number != oldest_bn:
            latency = time.time() - block_timestamp
            bot_logger.info(
                f"[{block_number}][+{latency:.1f}s]"
                f"[{base_fee / 10**9:.5f}/{base_fee_next / 10**9:.5f}]"
            )

    current_block_ref[0] = block_number

    # Build results list from fresh + updated entries
    results = []
    for path_id, opt_input, profit, hop_outs, consumed_ins in batch["fresh"]:
        results.append((path_id, int(opt_input), int(profit), tuple(int(h) for h in hop_outs), tuple(int(c) for c in consumed_ins), block_number))
    for path_id, opt_input, profit, hop_outs, consumed_ins in batch["updated"]:
        results.append((path_id, int(opt_input), int(profit), tuple(int(h) for h in hop_outs), tuple(int(c) for c in consumed_ins), block_number))

    # Expired: below threshold, still registered
    for path_id in batch["expired"]:
        pass  # Path still exists, may reappear as fresh

    # Removed: de-registered, permanently gone
    for path_id in batch["removed"]:
        path_suppression.discard(path_id)

    if results:
        await dispatch_profitable_results(results, ...)
```

`wait_for_block()` and `profitable_results()` are removed from the main loop path. `latest_results()` is kept as a diagnostic escape hatch.

### Step 7: Prime Python's state at startup

Between `subscribe()` and the first `async for` iteration, up to one block passes. The initial `solve_all_paths()` batch is buffered in the channel. When Python enters the `async for` loop, it consumes this batch immediately (no blocking wait). Subsequent batches arrive from the pump as blocks are processed.

If `build_paths` calls `register_and_solve_path` after `solve_all_paths` but before the first pump block, those paths set the `has_unsent_results` flag and are included in the first pump-triggered batch.

### Design decisions

- **`mpsc` over `watch`**: `watch` only keeps the latest value, coalescing intermediate updates. We *want* coalescing (if Python is slow, skip intermediate batches), but `mpsc::try_send` gives us explicit control and the diff is computed against `delivered` regardless. `watch` would require re-computing the diff on every consumer read — wasteful if the consumer reads the same value twice.
- **Send all `updated` entries, no threshold**: Some updated paths may have trivially different profit values. Sending them all avoids threshold tuning and false negatives. Python's suppression system handles the "not worth re-simulating" judgment. `SolvePathResult` derives `PartialEq` — all fields are compared (`optimal_input`, `profit`, `hop_outputs`, `consumed_inputs`). No previous result is included — Python gets only the new value and simulates accordingly.
- **`try_send` (non-blocking)**: The pump runs in a tokio task that holds the engine lock. Blocking on channel send would deadlock. If the channel is full, the batch is dropped — the next one will carry a correct cumulative diff.
- **`delivered` is advanced on compute, not on Python ACK**: Rust computes the diff and advances `delivered` immediately. If a batch is dropped (channel full), the next batch diffs against the current `delivered` state which includes the dropped batch's changes — nothing is permanently lost, just coalesced.
- **Block metadata folded into batch from day one**: `ResultBatch` includes `timestamp`, `base_fee_per_gas`, `gas_used`, `gas_limit` fields from Slice 1 (zeroed initially). This avoids a two-channel intermediate state. Later slices fill these fields from the pump's `process_block` call.
- **`min_profit` at batch boundary, not on `self.results`**: `self.results` keeps all nonzero-profit paths (~2,568). The filter is applied in `compute_diff` against `delivered`. This preserves backward compatibility for `latest_results()` (43 test references pass unchanged) and keeps diff semantics clean (profit oscillation shows as `updated`, not churn).
- **Channel created in `PyUniswapArbEngine::new()`**: Simplest lifecycle — one channel, created once, sender in engine, receiver in PyUniswapArbEngine. Works even if `register_and_solve_path` is called before the pump starts. On stop/restart, the channel is recreated.
- **`__anext__` on `PyUniswapArbEngine` directly**: No separate iterator object. The engine is the sole owner of the receiver — there's only ever one consumer.
- **`backfilled` flag dropped**: `BlockNotification.backfilled` was a reserved field hardcoded to `false` everywhere. Python never reads it. Not carried forward into `ResultBatch`.
- **Pool entries not removed on de-registration**: A de-registered path's pools stay in the sub-engines. Other paths (including future registrations during rolling-start) may still reference them.
- **Paths are never unregistered internally by Rust**: `removed` means "Python called `deregister_path()`." The signal is generic enough that future Rust-internal mechanisms could also trigger it.
- **`has_unsent_results` flag for `register_and_solve_path`**: Eagerly-solved paths between blocks set this flag. The next `rebuild_and_solve_affected` call produces a batch that includes them. One-block latency is acceptable.

## Files Involved

**Primary:**
- `rust/src/optimizers/uniswap_engine.rs` — Add `delivered`, `deregistered`, `min_profit`, `max_profit`, `result_tx`, `has_unsent_results` fields to `UniswapEngine`; add `ResultBatch` struct; add `compute_diff()`, `deregister_path()` methods; integrate batch emission into `rebuild_and_solve_affected` and `solve_all_paths`; add `__aiter__`/`__anext__` to `PyUniswapArbEngine`; add `result_rx` field to `PyUniswapArbEngine`; derive `PartialEq` on `SolvePathResult`
- `rust/src/optimizers/uniswap_engine_pump.rs` — Extend `process_block` signature with `BlockMetadata`; fill batch metadata fields from pump's block header data; remove `block_tx: watch::Sender<BlockNotification>` and all `block_tx.send()` calls
- `examples/eth_backrun_v2_v3_v4_rust.py` — Replace `wait_for_block` + `profitable_results()` with `async for batch in engine`; add `deregister_path()` call from suppression system; update `PathSuppression` to discard tracking on `removed` events

**Secondary:**
- `rust/src/optimizers/uniswap_engine_pump.rs` — `BlockNotification` struct kept for internal pump state during subscribe/resume, but `watch::Sender` is removed. `UniswapEnginePump` receives block metadata from WS headers and passes it to `process_block`.
- `rust/src/optimizers/uniswap_engine.rs` — `PyUniswapArbEngine.block_rx` replaced by `result_rx: parking_lot::Mutex<Option<mpsc::Receiver<ResultBatch>>>`. `wait_for_block()` removed (not deprecated — the `async for` replaces it completely).

**No change needed:**
- `rust/src/subscription.rs` / `subscription_py.rs` — Reference implementation for `__anext__` / `future_into_py` pattern, but not modified.
- `rust/src/optimizers/v2_engine_pump.rs` — Standalone pump, not used in the unified pump path.
- `rust/src/bot_core/` — No changes to sub-engines or decoders.
- `tests/` — 43 references to `latest_results()` pass unchanged because `self.results` still contains all nonzero-profit paths.

## Implementation Order

### Slice 1: Add `delivered`, `ResultBatch`, `deregistered`, and `compute_diff` to `UniswapEngine`

1. Derive `PartialEq` on `SolvePathResult`
2. Add `ResultBatch` struct (with all fields including zeroed block metadata)
3. Add `delivered: HashMap<u64, SolvePathResult>`, `deregistered: Vec<u64>`, `has_unsent_results: bool`, `min_profit: U256`, `max_profit: U256`, `result_tx: Option<mpsc::Sender<ResultBatch>>` fields to `UniswapEngine`
4. Add `compute_diff(&mut self, new_results: &[(u64, SolvePathResult)]) -> ResultBatch` method
5. Call `compute_diff` + `try_send` at the end of `rebuild_and_solve_affected` and `solve_all_paths`
6. Set `has_unsent_results = true` in `register_and_solve_path`
7. Add `deregister_path(path_id: u64) -> bool` method
8. Run: `just test-rust` — expect all existing tests to pass (channel is not yet consumed, `try_send` results are dropped harmlessly)

### Slice 2: Wire the channel through `PyUniswapArbEngine`

1. Add `result_rx: parking_lot::Mutex<Option<mpsc::Receiver<ResultBatch>>>` to `PyUniswapArbEngine`
2. In `PyUniswapArbEngine::new()`, create the `mpsc::channel(8)` and pass `result_tx` into the engine
3. Implement `__aiter__` (returns self) and `__anext__` on `PyUniswapArbEngine`: take `result_rx`, await `recv()` in `future_into_py`, convert `ResultBatch` to Python dict
4. Add `deregister_path(path_id: u64) -> bool` Python method
5. Run: `just test-rust` + `just test-rust-python` — expect all tests to pass

### Slice 3: Fill block metadata in `ResultBatch`

1. Add `BlockMetadata { timestamp: u64, base_fee_per_gas: Option<u64>, gas_used: u64, gas_limit: u64 }` struct (derive `Default` with zeros)
2. Extend `process_block(logs, block_number, metadata: BlockMetadata)` signature — the metadata is stored on the engine and included in the next batch
3. Extend `process_updates`, `process_v4_updates`, `process_all_updates` similarly
4. Update all Rust callers (test code uses `BlockMetadata::default()`)
5. Run: `just test-rust` — expect all tests to pass

### Slice 4: Replace pump's `BlockNotification` sends with block metadata

1. Remove `block_tx: watch::Sender<BlockNotification>` from `UniswapEnginePump` (and from `subscribe`/`spawn` return values)
2. Pass `BlockMetadata` from the pump's WS block header data into `engine.lock().process_block(...)` calls
3. Remove `block_rx` from `PyUniswapArbEngine` — `result_rx` replaces it
4. Remove `wait_for_block()` from `#[pymethods]` — the `async for` replaces it
5. Update `subscribe()` and `resume()` — no more `take_block_rx()` / `block_rx` wiring
6. Run: `just test-rust` — expect all tests to pass

### Slice 5: Rewrite Python main loop

1. Replace `while True: block_info = await ...; results = ...; await dispatch(...)` with `async for batch in engine_registry.engine: ...` (full loop body as shown in Step 6)
2. Remove `EngineRegistry.profitable_results()` — no longer needed
3. Add `deregister_path()` call from `PathSuppression` when de-registration threshold is hit
4. Update `PathSuppression` to discard tracking for paths in `batch["removed"]`
5. Keep `latest_results()` as diagnostic-only
6. Run: `just test-rust-python` + manual bot run — expect same behavior with fewer sim-fail log lines

### Slice 6: Validate and clean up

1. Run `just lint` + `just test-all`
2. Update `CONTEXT.md` / `rust/CONTEXT.md` if terminology changed
3. Run bot for 30+ minutes on mainnet to verify batch delivery rates match block rates

## Testing

### Per-slice test runs

Each slice runs `just test-rust` and/or `just test-rust-python`. The Rust test suite covers `UniswapEngine::process_block`, `rebuild_and_solve_affected`, and `solve_all_paths` — these must continue passing after each slice.

### New unit tests

```rust
// rust/src/optimizers/uniswap_engine.rs

#[test]
fn compute_diff_fresh_updated_expired_removed() {
    /* Register paths, solve, consume batch (advancing delivered),
       then process_block with updates — verify fresh/updated/expired/removed
       categorization is correct */
}

#[test]
fn compute_diff_dropped_batch_coalesces() {
    /* Verify that if try_send fails (channel full), the next batch
       correctly diffs against the last delivered state — no lost
       fresh/updated/expired entries */
}

#[test]
fn solve_all_paths_initial_batch_is_all_fresh() {
    /* First solve_all_paths after engine creation produces all-fresh
       batch (delivered is empty) */
}

#[test]
fn min_profit_filter_at_batch_boundary() {
    /* self.results contains all nonzero-profit paths.
       Batch only includes paths above min_profit threshold.
       latest_results() still returns all nonzero-profit paths. */
}

#[test]
fn deregister_path_produces_removed_in_batch() {
    /* deregister_path removes from paths/results/delivered.
       Next batch includes the path_id in removed. */
}

#[test]
fn deregister_path_preserves_pools_in_sub_engines() {
    /* After deregister_path, the path's pools are still in
       v2_engine/v3_engine/v4_engine. Other paths using those
       pools are unaffected. */
}

#[test]
fn register_and_solve_path_sets_unsent_flag() {
    /* Eagerly-solved paths set has_unsent_results.
       Next rebuild_and_solve_affected includes them in the batch. */
}

#[test]
fn profit_oscillation_shows_as_updated_not_churn() {
    /* Path going above threshold → below → above shows as
       updated (not fresh/expired/fresh). Delivered tracks
       above-threshold set, so re-appearance matches existing entry. */
}
```

### Integration tests

Existing Rust tests in `uniswap_engine.rs` (e.g., `process_block_routes_logs_to_sub_engines`, `rebuild_and_solve_affected_*`) cover the engine's solve path. New tests verify the diff computation and de-registration, which are the novel logic.

The existing Python bot run (`examples/eth_backrun_v2_v3_v4_rust.py`) is the end-to-end integration test — running it for 30+ minutes on mainnet and checking `[dispatch]` log lines for reduced sim-fail volume.

## Benefits

- **Leverage**: One channel replaces two coordination points (`wait_for_block` + `latest_results`). Rust owns the diff; Python consumes it.
- **Locality**: Result diff computation lives next to the solve logic that already knows what changed, instead of in Python's `profitable_results()` which must walk the entire list.
- **Depth**: Python's `EngineRegistry.profitable_results()` was a shallow seam that iterated over all results doing filtering Rust could do. Replaced by a deep seam — Rust delivers only actionable items.
- **Reduced FFI traffic**: Only fresh + updated entries cross the FFI boundary. Unchanged paths (typically 90%+ of results) stay in Rust.
- **De-registration**: Suppressing persistently-failing paths is now powered through to the engine level — Rust stops solving them entirely, saving CPU.

## Risks

- **`process_block` signature change** (Slice 3): Adding `BlockMetadata` parameter affects every caller, including test code. The change is mechanical but touches many call sites. Mitigated by `BlockMetadata::default()` — tests that don't care about block metadata can use the default.
- **Channel full → batch dropped**: If Python is slow to consume batches, intermediate diffs are lost. The next batch carries a cumulative diff from the last `delivered` state, so no information is permanently lost — it's just coalesced. But if a path goes fresh → updated → expired between two consumed batches, Python only sees the expired event (never the fresh). This is acceptable: if the path is no longer profitable, Python shouldn't simulate it anyway.
- **`mpsc` buffer size**: Too small → frequent drops and coarsened diffs. Too large → memory pressure if Python falls far behind. Mitigated by a bounded capacity of 8 — if Python is more than 8 blocks behind, the bot has bigger problems than dropped batches.
- **`delivered` grows with above-threshold results**: `delivered` holds ~2,568 entries (the profitable set). This is bounded by the number of registered paths and doesn't grow unboundedly — it's a small fixed cost per block.
- **`register_and_solve_path` one-block delay**: Eagerly-solved paths don't produce a batch until the next `process_block`. For a long-running bot, missing one block of results is negligible.

## Relationship to Other Plans

- **Plan 084** (solver per-hop-outputs): Independent. Plan 084 changed `SolvePathResult` to include per-hop outputs — this plan carries those outputs in the batch channel. No conflict.
- **Plan 082** (rust-owned state pipeline): Complementary. Plan 082 moved state updates to Rust-owned pipelines. This plan extends that by making the result delivery also Rust-owned. Same architectural direction, different layer.

## Status

[x] Slice 1: Add `delivered`, `ResultBatch`, `deregistered`, and `compute_diff` to `UniswapEngine`
[x] Slice 2: Wire the channel through `PyUniswapArbEngine`
[x] Slice 3: Fill block metadata in `ResultBatch`
[x] Slice 4: Replace pump's `BlockNotification` sends with block metadata
[x] Slice 5: Rewrite Python main loop
[x] Slice 6: Validate and clean up
