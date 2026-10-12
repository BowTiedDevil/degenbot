# WBCAMD — Injected-Snapshot Consumer Inventory (P2a)

**Task:** P2a: injected-snapshot consumer inventory (report)
**Epic:** 4ROQ6S — "Bot deep, Python light, DB privileged"
**Scope:** READ-ONLY inventory. No production code changed; the only tree effect is this file.
**Method:** `rg` sweeps over `src/ tests/ scripts/ examples/ rust/` for the snapshot class family
(`UniswapV3LiquiditySnapshot`, `UniswapV4LiquiditySnapshot`, `DatabaseSnapshot`), the runner
injected path (`_start_snapshots`, `engine_registry.start(v3_snapshot=/v4_snapshot=)`), the retired
Rust ingestion surface (`load_*_from_py`, `backfill_from_snapshot`, `stream_*_snapshot_to_engine`),
and targeted file reads.

---

## 0. Headline finding

**The injected snapshot *data* path is already structurally retired.** What survives is a
seed-block (`S`) plumbing seam plus stale documentation — not a live ingestion path.

Evidence:

- `src/degenbot/uniswap/snapshot_binary.py` is an intentionally-empty tombstone: *"Snapshot
  ingestion is now Rust-owned"* (its retired `stream_v3_snapshot_to_engine` /
  `stream_v4_snapshot_to_engine` names are pinned absent by
  `tests/ffi/test_ffi_registration_surface.py:170-180`).
- `load_v3_snapshot_from_py` / `load_v4_snapshot_from_py` no longer exist as callables. They appear
  **only** in docstrings/comments (`.pyi` stubs, `engine_registry.py`, Rust `engine/mod.rs`) and in
  the fake engine's `RETIRED_ENGINE_MEMBERS` tombstone tuple
  (`tests/fakes/engine.py:62-63`). `tests/arbitrage/test_engine_registry_start.py:69-84` asserts
  `start()` performs **no** `load_*_from_py` call.
- `engine_registry.start()` still accepts `v3_snapshot`/`v4_snapshot`, but **consumes only
  `s.newest_block`** to derive `S = min(...)` and stash it. The tick-data dicts are explicitly *not*
  ingested (`src/degenbot/arbitrage/engine_registry.py:182-195`): *"the Store is retired; per-pool
  tick data is fetched via the Chain arm (RPC) at registration."*
- **No production caller ever passes a snapshot object.** The only source of `v3_snapshot`/`v4_snapshot`
  is `InjectedActors.snapshots`, a test/DI seam (`src/degenbot/runner/bot_runner.py:399, 815-818`).
  Production `BotRunner(cfg)` builds `InjectedActors()` with `snapshots=None`, so
  `_start_snapshots()` returns `(None, None)`.

Consequence for Phase 2: the "risk item" (injected consumers) reduces to **the `S`-seed kwargs seam
and its test coverage**, not to a data-flow that must be migrated to the DB. The Python snapshot
facade modules (`v3_snapshot.py` / `v4_snapshot.py` / `snapshot_readers.py`) survive purely as the
**DB-backed parity oracle** for the Rust `degenbot-db` reader.

---

## 1. Production DB path vs injected path boundary (precise)

### Production DB path (the only live production snapshot source)

| Step | Site |
|---|---|
| Guard on DB file presence | `src/degenbot/bot/_bot.py:199` (`if Path(self.database_path).exists():`) |
| Eager construction-time load | `src/degenbot/bot/_bot.py:200` — `self._py_bot.load_snapshot_from_db(str(self.database_path), self._chain_id)` |
| Rust entry point (single authoritative loader) | `rust/crates/shells/degenbot-python/src/bot/mod.rs:383` — `Bot::load_snapshot_from_db` (retains `Arc<SnapshotDb>`, records `S`) |
| Stub/doc for the same entry | `src/degenbot/_ffi/__init__.pyi:1056-1070` |
| `start()` reads `S` from the shared `BotState` (no kwargs) | `src/degenbot/arbitrage/engine_registry.py:200` — `snapshot_block = self.engine.snapshot_seed_block` |
| Snapshot→WS backfill anchor | core `BlockPump::resume_from_subscribe` (via `S`; `bot_runner.py:588-591`) |

### Injected (non-DB) path — test seam only

| Step | Site |
|---|---|
| Test/DI carrier of the snapshot pair | `src/degenbot/runner/bot_runner.py:399` — `self._injected_snapshots = injected.snapshots` |
| Field definition | `src/degenbot/runner/bot_runner.py:298` — `snapshots: tuple[Any, Any, Any, Any] | None = None` |
| Resolver | `src/degenbot/runner/bot_runner.py:596` → `_start_snapshots()` at `:798-818` (returns the injected pair, else `(None, None)`) |
| Kwargs handed to the engine | `src/degenbot/runner/bot_runner.py:609-610` — `v3_snapshot=start_v3, v4_snapshot=start_v4` |
| `start()` injected arm — `S` only, **no data ingest** | `src/degenbot/arbitrage/engine_registry.py:189-195` — `snapshot_block = min(s.newest_block for s in (v3_snapshot, v4_snapshot) if s is not None)` |
| Seed `S` recorded on `BotState` | `src/degenbot/arbitrage/engine_registry.py:210` — `self.engine.snapshot_seed_block = snapshot_block` |
| Rust setter | `rust/crates/shells/degenbot-python/src/bot/engine/mod.rs:242-244` — `set_snapshot_seed_block` |

**The boundary line:** `src/degenbot/bot/_bot.py:200` (DB: real snapshot data loaded into the core
`SnapshotStore` at construction) vs `src/degenbot/arbitrage/engine_registry.py:189-210` (injected:
only a seed block `S` is transferred; tick data is never carried). There is no production caller of
the second line.

---

## 2. Consumer table

Classification: **production** / **test-tier** / **capture-tooling** / **dead**.
Recommendation: **retire** / **migrate-to-DB** / **keep-as-parity-oracle** / **keep**.

### 2a. Production source (`src/`)

| Site (file:line) | What it is | Class | Recommendation |
|---|---|---|---|
| `src/degenbot/arbitrage/engine_registry.py:138-139` | `start(..., v3_snapshot=, v4_snapshot=)` kwargs | production (injected seam) | **retire** the kwargs once tests migrate (S comes from `BotState` on both real paths) |
| `src/degenbot/arbitrage/engine_registry.py:189-210` | injected arm: `S = min(newest_block)`; `snapshot_seed_block` setter | production (S-only) | **retire** the injected arm; DB arm (`:200`) is the live one |
| `src/degenbot/arbitrage/engine_registry.py:51-52, 164-166, 270, 310` | docstrings still describing `load_*_from_py` ingestion | production (stale doc) | **retire** (doc debt) |
| `src/degenbot/runner/bot_runner.py:298, 399` | `InjectedActors.snapshots` field + wiring | production (DI seam) | **retire** with the kwargs (or reduce to a seed-block int) |
| `src/degenbot/runner/bot_runner.py:592-605, 609-610` | comments + `start()` kwargs at call site | production (orchestration) | **retire** |
| `src/degenbot/runner/bot_runner.py:798-818` | `_start_snapshots()` | production (orchestration, dead in production) | **retire** |
| `src/degenbot/runner/bot_runner.py:962` | `self._injected_bot = None` (release path) | production | keep |
| `src/degenbot/uniswap/v3_snapshot.py:29, 55, 88, 93` | `UniswapV3LiquiditySnapshotSource`, `DatabaseSnapshot` (DB facade), `UniswapV3LiquiditySnapshot` | production lib (no prod constructor) | **keep-as-parity-oracle** |
| `src/degenbot/uniswap/v4_snapshot.py:34-35, 53, 94, 99` | V4 equivalents | production lib (no prod constructor) | **keep-as-parity-oracle** |
| `src/degenbot/uniswap/concentrated/snapshot_readers.py:12, 82-` | `DatabaseSnapshotBase` + facade; imports Rust `degenbot.db.DatabaseSnapshot as _EngineSnapshot` | production (DB source) | keep |
| `src/degenbot/db/__init__.py:31, 63` | re-export of the **Rust** `DatabaseSnapshot` pyclass | production (DB seam) | keep |
| `src/degenbot/_ffi/db.pyi:104-115, 735` | stub for the Rust `degenbot._ffi.db.DatabaseSnapshot` pyclass | production (DB seam) | keep |
| `src/degenbot/__init__.py:70, 75, 107, 112` | public re-exports of the two facade classes | production API | keep (public surface) |
| `src/degenbot/uniswap/__init__.py:35, 42, 55, 60` | re-exports | production API | keep |
| `src/degenbot/bot/_bot.py:189-200` | DB eager load; comment claims "file/memory snapshot path … loaded at `engine_registry.start` via `load_*_from_py`" | production DB path (+ stale doc) | keep the load; **retire** the stale comment |
| `src/degenbot/runner/_run_ritual.py:331, 339` | comments on `Arc<SnapshotDb>` clone lifetime | production (DB path) | keep |
| `src/degenbot/runner/build_paths.py:967` | doc: `Arc<SnapshotDb>` handles dropped before return | production (DB path) | keep |
| `src/degenbot/uniswap/snapshot_binary.py` | empty tombstone module | dead | keep tombstone (pinned absent by test) |
| `src/degenbot/_ffi/__init__.pyi:485-505, 1497` | `snapshot_seed_block` getter/setter docs referencing `load_*_from_py` | production (stale doc) | **retire** (doc debt) |

### 2b. Rust core / shell

| Site | What it is | Class | Recommendation |
|---|---|---|---|
| `rust/crates/shells/degenbot-python/src/bot/mod.rs:367-440, 1057, 1292, 3183-3480` | `Bot::load_snapshot_from_db` + `SnapshotDb` handle lifecycle | production (DB path) | keep |
| `rust/crates/shells/degenbot-python/src/bot/engine/mod.rs:216-244` | `snapshot_seed_block` getter/setter (+ docstrings naming `load_*_from_py`) | production (S setter) + stale doc | keep setter; **retire** doc |
| `rust/crates/shells/degenbot-python/src/bot/engine/snapshot.rs:12-17` | doc: non-DB path sets `S` via setter | production (doc) | keep |
| `rust/crates/shells/degenbot-python/src/db/snapshot.rs:37` | Rust `DatabaseSnapshot` pyclass (mirrors Python facade) | production (DB seam) | keep |
| `rust/crates/foundation/degenbot-db/src/snapshot.rs`, `read.rs` | Rust snapshot read fns (mirror Python `DatabaseSnapshot`) | production (core) | keep |
| `rust/crates/foundation/degenbot-db/tests/parity.rs` + `tests/fixtures/generate_parity.py:22-23` | parity gate + fixture generator that imports the Python `DatabaseSnapshot` oracle | capture-tooling / parity | **keep-as-parity-oracle** |

### 2c. Tests

| Site | What it is | Class | Recommendation |
|---|---|---|---|
| `tests/uniswap/test_snapshot_facade_semantics.py:15, 17, 79-191` | facade drain-on-read / merge / `pending_updates` semantics via in-memory fake sources | test-tier | **keep-as-parity-oracle** (facade contract) |
| `tests/uniswap/v3/test_uniswap_v3_snapshot.py:31, 81-100` | DB-source read surface (`DatabaseSnapshot` → Rust) | test-tier | keep (DB) |
| `tests/uniswap/v3/test_v3_tracker_snapshot_events_golden.py:57, 244-260` | golden; builds a **memory** snapshot (`_MinimalSnapshotSource`) and pins `pending_updates → update_liquidity_map` | test-tier (golden) | **keep-as-parity-oracle** (the surviving snapshot-apply contract) |
| `tests/database/test_snapshot_seam_parity.py` | Rust-backed `DatabaseSnapshot` vs frozen legacy reader oracle | test-tier (parity) | **keep-as-parity-oracle** |
| `tests/database/test_snapshot_explicit_deps.py:12-13, 33-68` | path-explicit Rust-backed snapshots | test-tier | keep |
| `tests/ffi/test_ffi_registration_surface.py:170-180` | tombstone guard for `snapshot_binary` retired names | test-tier | keep |
| `tests/ffi/test_pyclass_module_annotation.py:125` | asserts `degenbot._ffi.db.DatabaseSnapshot` reports the `db` module | test-tier | keep |
| `tests/fakes/engine.py:46, 62-63, 78, 130, 240-247, 350-381` | fake engine + `RETIRED_ENGINE_MEMBERS` guard + `start()` recorder carrying `v3_snapshot`/`v4_snapshot` | test-tier | keep (guards retirement); migrate the recorder when kwargs retire |
| `tests/helpers/boot_actors.py:22, 153, 157` | default `snapshots=(None, None, None, None)` | test-tier | migrate when kwargs retire |
| `tests/arbitrage/test_arbitrage_session.py` (many: `:264, 279-280, 302, 329, 358, …`) | runner DI snapshots + assertions that `start_calls` carry `v3_snapshot`/`v4_snapshot` | test-tier | migrate-to-seed-block when kwargs retire |
| `tests/arbitrage/test_engine_registry_start.py:71-72, 188-189` (+ assertions) | injected `v3_snapshot`/`v4_snapshot` → `S` derivation | test-tier | migrate |
| `tests/arbitrage/test_engine_registry_two_step_verify.py:69-70` | `_FakeSnapshot(newest_block=…)` for the verify seed | test-tier | keep (S-only) |
| `tests/arbitrage/test_run_ritual.py:170-171` | fake host `v3_snapshot`/`v4_snapshot = None` attributes | test-tier (dead) | **retire** with the kwargs |
| `tests/arbitrage/test_cockpit_phase.py:83`, `test_cockpit_session.py:169` | `snapshots=(object(), object(), None, None)` | test-tier | migrate |
| `tests/arbitrage/test_solvers/test_arbitrage_engine.py:24-36` | `_make_v3_snapshot`/`_make_v4_snapshot` helpers whose docstrings reference retired `load_*_from_py`; **unused** | dead | **retire** (delete) |
| `tests/pathfinding/fixtures/generate_mainnet_snapshot.py:393, 403` | DB path via `PathfindingRequest(database_path=…)` | capture-tooling (DB) | keep |

### 2d. Scripts / examples

| Site | What it is | Class | Recommendation |
|---|---|---|---|
| `scripts/record_v3_golden.py` | records a full-range V3 golden via `bot.build_pool` (RPC/DB), not the injected path | capture-tooling | keep (unaffected) |
| `examples/eth_settlement_arbitrage_v2_v3_v4_rust.py:13-14` | doc: "Load DB snapshots (V3+V4) … Backfill snapshot→WS gap" | production example | keep (DB path) |

---

## 3. Assertions that depend on the Python snapshot modules (parity-oracle question)

The Python snapshot read modules are **not** consumed by any production data flow, but they are the
frozen oracle for the Rust `degenbot-db` reader. The assertions that depend on them:

1. **Rust↔Python DB-reader parity** — `rust/crates/foundation/degenbot-db/tests/parity.rs` asserts the
   Rust read fns (`fetch_liquidity_map`, `fetch_all_liquidity_maps`, `fetch_pools`,
   `fetch_newest_update_block`) match the Python `DatabaseSnapshot` oracle recorded in
   `fixtures/parity_expected.json`, generated by
   `rust/crates/foundation/degenbot-db/tests/fixtures/generate_parity.py` (which imports
   `degenbot.uniswap.{v3,v4}_snapshot.DatabaseSnapshot`). Includes the `VARCHAR(78)` ↔ `U256`
   boundary (>`i64::MAX`, `u128::MAX` gross). **Hard gate (binding #4).**
2. **Seam parity** — `tests/database/test_snapshot_seam_parity.py` compares the Rust-backed
   `DatabaseSnapshot` facade against the frozen legacy reader oracle.
3. **Facade semantics** — `tests/uniswap/test_snapshot_facade_semantics.py` pins lazy per-pool load,
   drain-on-read for `tick_bitmap`/`tick_data`/`pending_updates`, and the `update()` merge.
4. **Snapshot-apply contract** — `tests/uniswap/v3/test_v3_tracker_snapshot_events_golden.py` pins
   `snapshot.pending_updates(...) → pool.update_liquidity_map(...)`. **No DB-based flow covers this
   assertion** (the DB golden/parity suites cover pool *build*; this covers event *application* to a
   built pool).

None of these four depend on the `engine_registry.start(v3_snapshot=/v4_snapshot=)` kwargs or on
`InjectedActors.snapshots`. They survive independently of the injected-seam retirement.

---

## 4. Recommendation for P2b (retire the injected snapshot path)

1. **Remove the seed-block kwargs seam** — delete `v3_snapshot`/`v4_snapshot` from
   `engine_registry.start()` (`engine_registry.py:138-139`) and its injected arm
   (`:189-210`), delete `_start_snapshots()` (`bot_runner.py:798-818`) and the kwargs at
   `bot_runner.py:609-610`, and drop `InjectedActors.snapshots` (`bot_runner.py:298, 399`). `S` is
   already carried on the shared `BotState` by the DB path (`:200`); the injected arm's only effect
   was to set that same `S`.
2. **Migrate the runner DI tests** to inject a `snapshot_seed_block` value directly
   (`tests/arbitrage/test_arbitrage_session.py`, `test_engine_registry_start.py`,
   `test_cockpit_phase.py`, `test_cockpit_session.py`, `helpers/boot_actors.py`) instead of fake
   snapshot objects.
3. **Purge stale documentation** — the `load_*_from_py` / `_*_to_py_dict` / `backfill_from_snapshot`
   narrative in `engine_registry.py`, `bot_runner.py`, `_bot.py`, `_ffi/__init__.pyi`,
   `_ffi` engine stub, and `rust/.../bot/engine/mod.rs`. Delete the dead
   `tests/arbitrage/test_solvers/test_arbitrage_engine.py:24-36` helpers.
4. **Keep** the Python snapshot facade modules + DB parity oracles (§3) and the
   `snapshot_binary.py` / `RETIRED_ENGINE_MEMBERS` tombstones.
5. **Do not** touch the DB path (`_bot.py:200` → `bot/mod.rs:383`) — it is the live production
   snapshot source.

---

## 5. Acceptance check

- Every `rg` hit for `UniswapV3LiquiditySnapshot` / `UniswapV4LiquiditySnapshot` / `DatabaseSnapshot`
  across `src/ tests/ scripts/ examples/ rust/` is accounted for in §2 (facade defs, re-exports,
  DB seam, tests, parity fixtures).
- The production DB path vs injected path boundary is stated with file:line in §1.
- No code changed; this report is the only tree effect.
- **Foreign activity observed (left untouched):** an unrelated Rust `degenbot-solvers` / `degenbot-aave`
  stream is mid-flight in the working tree (modified `degenbot-solvers/src/mixed/*`,
  `profit_envelope/*`, `degenbot-aave/*`, `degenbot-cli*`, `aave_updater.rs`, `_ffi/aave.pyi`,
  `docs/cli/aave.md`, `docs/rust-cli.md`, plus untracked
  `rust/crates/engine/degenbot-solvers/src/mixed/solidly_t.rs`).
