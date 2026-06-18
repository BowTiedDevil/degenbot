{
  "id": "dff9df6b",
  "title": "ADR-006 Slice 5: Generalize BlockPump, relocate onto Bot (transport + drain unchanged)",
  "tags": [
    "adr-006",
    "slice-5",
    "rust",
    "transport",
    "py-binding-lift"
  ],
  "status": "completed",
  "created_at": "2026-06-18T07:14:45.773Z"
}

## Status: 5a delivered (Option B split). Commit pending.

5a done: pump generalization + `DrainSink` seam + `PyBot` lifecycle ownership (delegated from `PyUniswapArbEngine`) + `attach_engine` wiring in `register_path`/`register_and_solve_path` (so `dispatch_log` dirties the engine — essential now that `apply_log` is replaced).

### Files (slice 5a)
- `rust/src/bot_core/block_pump.rs` (new) — `BlockPump` holding `Arc<Bot>` + `Arc<dyn DrainSink>`; per-log → `bot.dispatch_log`; drain/send/reorg/finalize → `sink`. Mechanics unchanged (dual WS sub, Rust filter, block-boundary, debounce, gap/timeout backfill). Moved from `optimizers/uniswap_engine_pump.rs`.
- `rust/src/bot_core/drain_sink.rs` (new) — `DrainSink` trait (6 methods: has_dirty_paths, on_drain, on_send, finalize_block, on_reorg, last_processed_block). `apply_log` ABSENT — log application routes through `dispatch_log` (D4 goal).
- `rust/src/optimizers/uniswap_engine/engine_drain_sink.rs` (new) — `EngineDrainSink` placeholder pass-through adapter (wraps `Weak<Mutex<UniswapEngine>>`); slice 6 swaps for SolveCoordinator, slice 7 adds reorg.
- `rust/src/bot_core/mod.rs` — moved `BlockMetadata` to bot_core (general data; re-exported from optimizers) + `Bot::with_core(core)` + `block_pump`/`drain_sink` mod decls.
- `rust/src/bot_core/py_bot.rs` — `PyBot.bot: Arc<Bot>` + `bot_arc()` (so the spawned pump clones the same `Bot`). `core_arc()` now `#[allow(dead_code)]`.
- `rust/src/optimizers/uniswap_engine/py_binding.rs` — `PyUniswapArbEngine.bot: Arc<Bot>` (shared from `PyBot` or standalone `Bot::with_core`); `start`/`subscribe` construct `EngineDrainSink::arc_handle` + drive `BlockPump`; `register_path`/`register_and_solve_path` collect pool_ids + `attach_engine` per pool (the dispatch→dirty chain).
- `rust/src/optimizers/uniswap_engine/mod.rs` — `engine_drain_sink` mod + `BlockMetadata` re-export.
- `rust/src/optimizers/uniswap_engine_pump.rs` — DELETED (moved to block_pump.rs).

### Design
- `apply_log`/`process_block`/`process_block_and_send` stay on the engine (reached via `finalize_block` → `process_block_and_send(&[])` + the engine unit test) — NOT dead per clippy. Slice 6 (SolveCoordinator) will further refactor.
- `attach_engine` subscribed per-pool during path registration (duplicates harmless — `insert_dirty` idempotent via HashSet). No test exercises the live pump loop, so this chain is essential for live correctness (not just tests).
- `Bot::start` slice-3 placeholder left as-is (the Bot has no engine → can't construct an `EngineDrainSink`; the PyBot wiring layer spawns `BlockPump` directly). Slice 8 (Python facade) may revisit.

### Verification
- `cargo test --lib`: 490 passed (2 new: DrainSink forwarding + dropped-engine panic).
- `cargo test` doc: 9 passed.
- `just test-rust-python`: 193 passed.
- `just lint-rust` (+ `--fix`): clean. `cargo clippy --lib --tests`: clean. `cargo fmt`: clean.
- `just test-all`: RED ONLY on `tests/builders/test_context.py` — a PRE-EXISTING failure (commit 84f21384 "ADR-005 slice 4" added `py_bot: PyBot` to `BuilderContext` without updating the `_make_ctx` test fixture). Confirmed fails identically with slice-5 changes stashed. NOT a slice-5 regression. → separate `test(builders)` fix (1-line: add a fake `py_bot`).

### Deferred
- **5b** (own todo): candidate-2 helper extraction — `SnapshotStore`/`register_with_cl_buffers`/verify-plumbing off `py_binding.rs` (the snapshot/verify deep module; own RED→GREEN).
- `test_context.py` fixture fix for the pre-existing ADR-005 debt.
