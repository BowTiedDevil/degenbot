# Add Rust BlockNotification type + block_tx channel on UniswapEngine

## Goal (epic)
The backrun bot's block clock must come from the `newHeads` stream, forwarded by the
Rust pump on every block, NOT inferred from the solver result batch's `solve_block`
field (which lags by debounce delay + only advances when a batch is actually sent).
This implements the `block_tx.send(BlockNotification)` channel that
`docs/architecture/rust-owned-bot.md` §6.1 already specifies but was never built.

Symptom this fixes: `[block: 25390114]` printed in Python while the pump's
`current_block` is already 25390117 (observed live via `[DIAG]` instrumentation). The
consumer keys `dispatcher.advance_block`, `fee_history(newest_block=…)`,
`record_block_time`, and the `[block:]` log off `batch["solve_block"]` today.

## Architecture / seam map (mirror the existing result-channel wiring)
- `PyUniswapArbEngine::new` (rust/crates/degenbot-python/src/bot/engine/register.rs:24)
  creates `mpsc::unbounded_channel::<ResultBatch>()` → `(result_tx, result_rx)`; calls
  `engine.set_result_channel(result_tx)`; stores `result_rx` on the pyo3 struct.
- `UniswapEngine::set_result_channel` + `result_tx` field
  (rust/crates/degenbot-bot/src/solvers/uniswap_engine/result_channel.rs:1) +
  `compute_diff_and_send` sends `ResultBatch { solve_block: self.results_block, .. }`.
- `DrainSink` trait (rust/crates/degenbot-bot/src/bot_core/drain_sink.rs:35) with
  `on_drain/on_send/finalize_block/last_processed_block`; `SolveCoordinator` impl fans
  to each `EngineHandle`; pump calls `self.sink.on_send(&metadata)` on the debounce.
- `BlockPump::run_with_stream` `WsEvent::BlockHeader` arm advances `current_block`
  (rust/crates/degenbot-bot/src/bot_core/block_pump.rs ~451). Inject point: after
  `current_block = number`, call `self.sink.notify_block(current_block,&current_metadata)`.
- pyo3 async iterator: `UniswapArbEngine.__aiter__/__anext__` → `result_rx.recv()`
  (rust/crates/degenbot-python/src/bot/engine/result_channel.rs:238,254).
- Python consumer deriving the clock from the batch:
  examples/eth_backrun_v2_v3_v4_rust.py:2454 (`block_number = batch["solve_block"]`).
- `BlockMetadata` struct: rust/crates/degenbot-bot/src/bot_core/mod.rs:3438.

## Design decisions
- **Separate `block_tx` channel** (not folded into ResultBatch) — the architecture
  mandates a distinct channel; folding the block tick into the result batch is the
  conflation that caused the bug. The block stream must tick on every newHeads
  regardless of solve/debounce state.
- **Two async iterators on one object.** A pyo3 class has one `__aiter__`/`__anext__`,
  so expose the block stream via a separate iterator object `BlockStream` (mirrors
  `rpc/subscription.rs`), constructed by `engine.block_stream()` and holding `block_rx`.
- **Consumer restructure:** `consume_result_batches` becomes a dual-await loop
  (`asyncio.wait` on two `__anext__` futures) — block clock drives `advance_block`,
  `record_block_time`, `fee_history`, `[block:]` log; result batch drives dispatch keyed
  off the current block clock held in `Dispatcher`.
- The block stream is forwarded from the pump's already-received newHeads (after
  `current_block = number`), so it is in lockstep with the block the solver just solved
  against — the authoritative clock, never stale by debounce.

## Children (in dependency order)
1. Add Rust `BlockNotification` type + `block_tx` channel on `UniswapEngine`
2. Extend `DrainSink` with `notify_block`, fan out in `SolveCoordinator`/`EngineHandle`
3. Emit `BlockNotification` from the pump's `WsEvent::BlockHeader` arm
4. Expose `block_stream()` pyo3 async iterator (`BlockStream` class)
5. Restructure `consume_result_batches` to consume the block stream as the clock
6. TDD: block-channel test (header → BlockNotification), consumer dual-await test

## Acceptance Criteria
- A new `BlockNotification` type carries `{number, timestamp, base_fee_per_gas,
  gas_used, gas_limit}` over its own mpsc channel, plumbed parallel to `result_tx`.
- The pump emits exactly one `BlockNotification` per `WsEvent::BlockHeader` it
  processes (after `current_block` is advanced), independent of debounce/solve state.
- `consume_result_batches` derives its block clock from the block stream, not from
  `batch["solve_block"]`. The `[block: N]` log, `dispatcher.advance_block`,
  `record_block_time`, and `fee_history(newest_block=…)` all use the stream block.
- Red tests written first.
- `result_tx`/`ResultBatch`/`solve_block` retained for results + solve-block metadata
  (dispatch still records `solve_block` age) — NOT used as the clock.
- `just test-rust`, `just test-python`, `just lint-rust` all green.

---

# DrainSink::notify_block + SolveCoordinator fan-out
Add `fn notify_block(&self, block: u64, metadata: &BlockMetadata);` to the `DrainSink`
trait (rust/crates/degenbot-bot/src/bot_core/drain_sink.rs:35). Implement on
`SolveCoordinator` (solve_coordinator.rs:113) to fan to each `EngineHandle` under the
drain lock — mirrors how `on_drain`/`on_send` fan out. `EngineHandle` (engine_handle.rs)
+ `UniswapEngine` implement `notify_block(block, metadata)` →
`self.block_tx.send(BlockNotification::from(block, metadata))`. Depends on the
`BlockNotification` + `block_tx` type existing. Add the FAILING red test in this task's
crate: a `DrainSink` mock asserting `notify_block` forwards to the engine's `block_tx`.

---

# Emit BlockNotification from the pump header arm
In `BlockPump::run_with_stream` `WsEvent::BlockHeader` arm, after `current_block =
number` is set on BOTH the first-header path and the normal `number > current_block`
path, call `self.sink.notify_block(current_block, &current_metadata)`. The notification
must fire on every header the pump accepts (including the backfill-gap first header
after a gap is backfilled). Do NOT fire on stale/duplicate headers (the `else` branch).
Add a red test using the existing `for_test` pump harness + a `DrainSink` mock that
records `notify_block` calls: feed a `WsEvent::BlockHeader` and assert one notification
with the header's number+metadata. Depends on `notify_block` being in the trait.

---

# Expose block_stream() pyo3 async iterator (BlockStream class)
In rust/crates/degenbot-python/src/bot/engine/, add a `BlockStream` pyo3 class holding
`block_rx: Arc<Mutex<Option<mpsc::UnboundedReceiver<BlockNotification>>>>` with
`__aiter__`/`__anext__` (mirrors `rpc/subscription.rs`'s async-iter pattern and the
existing `UniswapArbEngine.__anext__` result-channel impl). Return it from a new
`engine.block_stream()` method. In `PyUniswapArbEngine::new` (register.rs:24), create
the `(block_tx, block_rx)` channel alongside the result channel; call
`engine.set_block_channel(block_tx)`; store `block_rx` for `block_stream()`. System test:
drive a header through the pump from Python, `async for` on `block_stream()`, assert
the block dict keys/number. Depends on the Rust channel + pump emission.

---

# Restructure consume_result_batches to consume the block stream as the clock
examples/eth_backrun_v2_v3_v4_rust.py:2454 — rewrite the consumer as a dual-await loop
(`asyncio.wait` over `block_stream.__anext__` + `result_batch.__anext__`). The block
stream drives `dispatcher.advance_block`, `dispatcher.record_block_time`,
`fee_history(newest_block=block)`, and the `[block: N]` log. The result batch drives
`dispatch_results(...)` keyed off the current block clock held in `Dispatcher` (read
`dispatcher.current_block` rather than `batch["solve_block"]`). `solve_block` from the
batch is still recorded per-result for age/staleness, but is no longer the clock. Sync
engine_registry.py to expose `engine.block_stream()` if needed. Depends on the pyo3
block stream existing.

---

# TDD safety nets: block-channel + consumer dual-await tests
Red-first, both made green by their owning tasks above:
1. rust/crates/degenbot-bot: pump header → exactly one `BlockNotification` on `block_rx`
   (via the `for_test` + DrainSink mock seam).
2. python: a fake block stream + fake result stream driving the restructured
   `consume_result_batches`, asserting the block clock advances from the BLOCK stream
   (not the result batch) and that a result batch dispatch uses the block-stream block.
Cross-check these against the Acceptance Criteria before marking the epic done.