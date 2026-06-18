{
  "id": "d2cf1aee",
  "title": "ADR-006 Slice 3: Introduce BotState submodule; Bot becomes thin orchestrator facade",
  "tags": [
    "adr-006",
    "slice-3",
    "rust",
    "internal-seam"
  ],
  "status": "complete",
  "created_at": "2026-06-18T07:13:44.999Z"
}

**Master: `TODO-215e9e66` (ADR-006).** Deps: slice 1. Addresses ADR-006 D4 (internal-seam precondition).

**Goal.** Introduce `BotState` as the pure-data submodule (today's `Bot` struct renamed/refactored), and reshape `Bot` into a thin orchestrator facade that owns `chain_id`, the shared `Arc<RwLock<BotState>>`, and (placeholders for) the cohesive helpers introduced in slices 4-7. **`Bot`'s public interface stays small** (~4 methods: `new(chain_id, rpc_url)`, `attach_engine`, `register_pool`, `start`) — callers and tests cross that seam; `BotState` + the helpers are `pub(crate)`, private deep modules with their own test seams. Per the codebase-design principle: interface shrinks, implementation absorbs; no separate *public* `BotDriver`/`BotSession`.

**Rust work.**
- Rename today's `rust/src/bot_core/mod.rs` `Bot` struct (pure data: registries, swap math, reorg journal, V3/V4 liquidity-event buffers) → `pub(crate) struct BotState` (or keep the `Bot` struct's fields as a private `bot_state` field if a rename churns too much — but a named submodule gives a cleaner test seam). Decision: prefer the **named rename** — it makes the "pure-data internal module" seam explicit and testable without going through the orchestrator.
- `Bot` becomes a new struct holding: `chain_id: u64` (from D1), `core: Arc<RwLock<BotState>>` (from D1, shared), an `engines: Vec<Box<dyn EventSink>>` (the sink trait lands in slice 4; here it's a placeholder `Vec<...>`), and fields/placeholders for the helper structs (slices 4-7 fill them in). `Bot::new(chain_id, rpc_url)` constructs `BotState` + the placeholder helpers.
- Delegation: `Bot::register_pool(...)` → `self.core.write().register_*(...)` (the existing `Bot`/`BotState` method). `Bot::attach_engine(engine)` → push to `engines`. `Bot::start()` → starts the (slice-5) pump (placeholder no-op / panics "pump not yet wired" until slice 5).
- The PyO3 wrapper `PyBot` (`py_bot.rs`) now holds `Arc<RwLock<Bot>>` (one level up — wrapping the orchestrator, not `BotState` directly). `PyLiquidityPool`/`PyErc20Token` keys reach `BotState` through `Bot`'s delegate. **This is the cascade to verify carefully** — ADR-005's `PyLiquidityPool`/`PyErc20Token` directly shared `PyBot`'s `Arc<RwLock<Bot>>`; under slice 3 they share `Arc<RwLock<Bot>>` where `Bot` wraps `BotState`. Confirm the handle topology still gives N Python objects → one Rust-owned state (it does — same Arc, one extra deref level).
- Keep `Bot::new()` standalone sugar for no-pyo3 tests (allocates its own `BotState` + `Bot`).

**Tests.**
- `BotState`-in-isolation tests: construct a `BotState`, apply events directly (`apply_v2_sync` etc.), assert state transitions — no orchestrator, no I/O, no pyo3. (These may migrate from today's `mod.rs` unit tests that currently exercise `Bot` directly.)
- `Bot` facade tests: `Bot::new(...).register_pool(...)` delegates correctly to `BotState`; `attach_engine` appends.

**Acceptance.** `just test-all` green; `BotState` (pure-data, no I/O) is a `pub(crate)` module with its own test seam; `Bot` is a thin orchestrator facade with a ~4-method public interface; the `Py*` handles now share `Arc<RwLock<Bot>>` (wrapping `BotState`) and tests confirm N handles still reach one Rust-owned state. Helpers (LogDispatcher/BlockPump/SolveCoordinator/ReorgCoordinator) are placeholders owned but unwired here — slices 4-7 implement them.

## Progress log

- **DONE, green (Option B):** `Bot`(state) renamed to `pub(crate) struct
  BotState` (all 59 methods stay on it). A new thin `pub(crate) struct Bot`
  orchestrator holds `chain_id: u64` (ADR-006 D1, deferred from slice 1) +
  `state: Arc<RwLock<BotState>>`, and exposes `new(chain_id)`, `chain_id()`,
  `state_arc()`, `start()` (placeholder — panics; BlockPump lands slice 5).

- **Option B (agreed):** `PyBot` OWNS a `Bot` orchestrator outright (not behind
  a lock) and hands out clones of `bot.state_arc()`. `PyLiquidityPool` /
  `PyErc20Token` / `UniswapEngine` hold `Arc<RwLock<BotState>>` (renamed from
  `Arc<RwLock<Bot>>`). **Zero delegation methods, one lock per access, N
  handles → one Rust-owned state preserved.** Deviates from the todo's literal
  "`PyBot` holds `Arc<RwLock<Bot>>`" (Option A) — agreed with the user as the
  lower-churn, no-double-lock realization of D4's intent.

- **Visibility:** `BotState` is `pub(crate)` (the todo's "private deep module").
  This subjects its `pub` methods to dead-code checking; 3 test-only methods
  (`pool_address`, `v4_journal_len`, `v4_discard_before_block`) + the
  orchestrator's test-only `chain_id`/`start` got `#[allow(dead_code)]` with
  forward-ref comments (chain_id wires in slice 8; start in slice 5).

- **Deferred from this slice (per scope decision):** `rpc_url`, `engines`
  vec, `attach_engine`, `register_pool` delegator, helper-struct placeholders.
  These land with their owners (EventSink=slice 4, BlockPump=slice 5,
  single-chain chain_id wiring=slice 8). Storing them unused now would be
  speculative (codebase-design: no speculative features). Slice 3 is the
  internal-seam precondition only.

- **RED→GREEN tracer:** `bot_facade_holds_chain_id_and_shares_bot_state` —
  constructs `Bot::new(5)`, asserts `chain_id()==5`, registers a V2 pool via
  `state_arc().write()`, and asserts a SECOND `state_arc()` clone sees it
  (proves the shared-Arc invariant: N handles → one `BotState`).

- **No-pyo3 tests:** `Bot::new()` → `BotState::new()` (5 sites in mod.rs tests
  + engine test helpers). `with_core`/`core_arc`/handle constructors now take
  `Arc<RwLock<BotState>>`.

- **Verification:** `just test-rust` 508 green (484 lib + 1 + 14 + 9 doctest);
  `lint-rust`/`fmt-check`/`clippy --deny warnings` clean; `ruff check src/` +
  `ty check src/` + `ruff format --check src/` clean; 1005 Python tests green
  (arbitrage + rust + types + v2/v3 offline); pyo3 surface (PyBot/PyLiquidityPool/
  PyErc20Token/engine) intact — `PyBot()` Python construction unchanged.

**Scope note (deviation from todo, recorded):** the todo's `Bot::new(chain_id,
rpc_url)` signature is trimmed to `Bot::new(chain_id)` — `rpc_url` has no
consumer until slice 5's BlockPump, so storing it now is speculative. Added
when slice 5 wires the pump.
