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
  "status": "open",
  "created_at": "2026-06-18T07:14:45.773Z"
}

**Master: `TODO-215e9e66` (ADR-006).** Deps: slice 3. Addresses ADR-006 D4 (transport ownership relocation).

**Goal.** Generalize today's `rust/src/optimizers/uniswap_engine_pump.rs` (`UniswapEnginePump`) into `Bot`'s WS transport + drain loop (`BlockPump`), relocating RPC ownership from the engine/pump to `Bot`. The pump's **mechanics stay unchanged** (dual `newHeads`+`logs` subscription, Rust-side topic+address filtering, block-boundary detection, gap/timeout `eth_getLogs` backfill) — only the owner and the per-block dispatch targets change: instead of `engine.process_block(logs, block, metadata)` directly, the pump feeds logs to `LogDispatcher` (decode+apply+notify) and fires the drain/block-boundary tick to `SolveCoordinator` (slice 6).

**Rust work.**
- Generalize `UniswapEnginePump` → `pub(crate) struct BlockPump` holding: `bot: Arc<RwLock<Bot>>` (or a narrower handle to `LogDispatcher` + `chain_id` + the provider), `provider: Arc<AlloyProvider>`, `shutdown: Arc<AtomicBool>`, the `tokio::sync::watch`/`mpsc` for block notifications. The pump no longer references `UniswapEngine` by name.
- Per WS log: `log_dispatcher.dispatch(log, &bot_state)` — decode+apply+notify (slice 4's path). Filtering: keep the current Rust-side topic+address filter (`V2_SYNC`/`V3_SWAP/MINT/BURN`/`V4_SWAP/MODIFY_LIQUIDITY` against registered `Bot` pool addresses — `Bot` owns the registry, so the filter moves with the pump).
- Block boundary / empty-queue drain: fire `solve_coordinator.on_drain(block, metadata)` (slice 6) instead of `engine.solve_dirty` + `engine.send_result_batch`. The existing 50ms send-result debounce (`DEBOUNCE_MS`) stays as a *send* debounce (unchanged per ADR-006 — it's not a solve debounce).
- `Bot::start(rpc_url)` (slice 3's placeholder): now spawns the `BlockPump` tokio task.
- The `py_binding.rs` `PyUniswapArbEngine::start`/`subscribe`/`backfill_from_snapshot` PyO3 surface: these move to `PyBot` (Bot owns the pump now), or `PyUniswapArbEngine.start()` delegates to `bot.start()`. The `SnapshotStore`/`register_with_cl_buffers`/verify-plumbing pure helpers currently trapped in `py_binding.rs` (candidate-2 finding) relocate onto `Bot`/`BlockPump` here — making them testable without pyo3.

**Tests.**
- `BlockPump` with an in-memory/fake provider: assert block delivery, drain-tick firing, address filtering, gap backfill on simulated WS drop. (The existing `uniswap_engine_pump` tests migrate here, decoupled from `UniswapEngine`.)
- Reorg flag: assert a `removed: true` log routes to `ReorgCoordinator` (slice 7).
- Subscribe-phase ordering (`subscribe()` → `backfill` → `resume`) preserved.

**Out of scope here.** `SolveCoordinator` and `ReorgCoordinator` are placeholders wired in slices 6 + 7; slice 5 opens the dispatch seam + Drain-tick CALL but may compile-against placeholder sinks.

**Acceptance.** `just test-all` green; `BlockPump` lives on `Bot` (no engine reference); `PyBot.start()`/`subscribe()`/`backfill_from_snapshot()` own the pump lifecycle; `py_binding.rs`'s pure helpers (SnapshotStore/register_with_cl_buffers/verify-plumbing) relocated off the PyO3 file (candidate-2 absorbed).
