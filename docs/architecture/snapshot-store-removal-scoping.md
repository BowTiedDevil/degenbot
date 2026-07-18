# Scoping: Remove `SnapshotStore`, collapse tick-map precedence to `Db → Chain`

> Spike output for ergo task `HKJ7VR` (epic `XEANMB`).
> Investigation-only — no code changes in this task.

## 1. Candidate-1 dependency confirmation — LANDED ✅

The `Db` arm of Candidate 1 landed in epic `5NT2OC` (tasks `U4KLPV`, `NOD4PS`, `XH5ID5` + the earlier `UHPXSD`), and is exercised on the production path:

- `assemble_v3_tick_map` / `assemble_v4_tick_map` in
  `rust/crates/degenbot-bot/src/bot_core/tick_assembly.rs` implement the
  `Store → Db → Chain` precedence. The **Db arm** calls
  `DegenbotDb::fetch_liquidity_map` (V3) / `fetch_liquidity_map_v4` (V4) and
  converts a non-empty `LiquidityMap` into `Tracked` tick coverage
  (`liquidity_map_to_tick_info`).
- The **PyO3 wrapper** (`rust/crates/degenbot-python/src/bot/mod.rs`) wires
  `AlloyTickBootstrapRpc` into the Chain arm via `make_tick_bootstrap_rpc`
  (task `NOD4PS`, Option B — pure-Rust, no GIL re-entry).
- The **Python builders** (`src/degenbot/builders/v3_pool_builder.py`,
  `v4_pool_builder.py`) were cut over to call `assemble_*_tick_map` (task
  `XH5ID5`) and pass the returned `(rust_rows, coverage)` inline as
  `tick_data` to `register_v3_pool` / `register_v4_pool`.

**This epic is NOT blocked.** Candidate 1's `Db` arm is on the production path.

### 1a. A load-bearing discovery: the `seed_from_store=true` path is already vestigial

The Db arm being on the production path changed how `seed_from_store` is
computed. The PyO3 wrapper at `rust/crates/degenbot-python/src/bot/mod.rs:963`
(V3) and `:1085` (V4) sets:

```rust
let seed_from_store = cov == PoolTickCoverage::Tracked && rust_tick_data.is_empty();
```

After the `XH5ID5` cutover, the builder ALWAYS passes `rust_rows` (the
helper's returned ticks) inline as `tick_data`. Tracing the four outcomes:

| Arm hit | `rust_rows` | `coverage` | `seed_from_store` | `register_*` seed source |
|---|---|---|---|---|
| Store hit (non-empty ticks) | store's ticks (non-empty) | `Tracked` | `false` | inline `tick_data` |
| Store hit (empty ticks — edge case) | `{}` | `Tracked` | **`true`** | `take()` AGAIN (see §1b) |
| Db hit | db's ticks (non-empty) | `Tracked` | `false` | inline `tick_data` |
| Chain hit | one word's ticks | `Sparse` | `false` | inline `tick_data` |
| Miss | `{}` | `Sparse` | `false` | inline `tick_data` (empty) |

**In practice, `seed_from_store` is `false` on every production registration.**
The Store's `take()` is still called (in the helper's Store-arm closure,
consuming the entry), but the result flows as **inline `tick_data`**, NOT
through `seed_from_store=true`. The `register_*`'s `seed_from_store` branch
(which calls `take()` itself) is reachable ONLY on the
Store-hit-with-empty-ticks edge case (a pool in the snapshot with zero
initialized ticks — a freshly-initialized pool).

### 1b. Latent quirk in the empty-ticks Store-hit edge case (NOT a regression)

On the rare Store-hit-with-empty-ticks path, the helper's Store arm already
called `take()` (consuming the entry). Then `seed_from_store=true` makes
`register_v3_pool` call `take(&addr)` a SECOND time — which returns
`({}, Sparse)` (key gone). So the pool registers as `Sparse` with empty
ticks, NOT `Tracked`-with-empty-ticks. This is a latent quirk, **not a
regression from the `5NT2OC` cutover** — the same `seed_from_store` formula
existed pre-cutover, so the same edge-case behavior held then too. It is
moot once the Store is removed (the Db arm returns `Tracked` for a pool with
rows; a pool with zero initialized ticks has no `liquidity_positions` rows →
Db arm returns `None` → falls to Chain arm → `Sparse` — the correct outcome).

## 2. The consistency question (load-bearing) — the decision

### 2a. What `SnapshotStore` actually pins

`load_snapshot_from_db` (`rust/crates/degenbot-bot/src/bot_core/mod.rs:4099`)
does TWO things:

1. Streams every V3+V4 pool's tick data into the Store via
   `load_v3_family` / `load_v4_family` → `db.stream_liquidity_maps` →
   `store.insert`.
2. Sets the global **`snapshot_seed_block` `S`** =
   `min(fetch_newest_update_block(V3), V4))` on `BotState`.

The Store's consistency guarantee is: every pool's seed is the DB's state
**at the boot instant**, and `S` is the newest exchange `last_update_block`
at that instant. `take()` at registration hands out that boot-frozen cut.

### 2b. Where `S` is consumed

`S` (`snapshot_seed_block`) is read by:

- **`block_pump.rs` auto-backfill** (`rust/crates/degenbot-bot/src/bot_core/
  block_pump.rs:409, 459, 1072`) — closes the `S+1..W-1` gap before resume.
  This is INDEPENDENT of the Store — it only needs `S`, not the tick data.
- **`verify_*_snapshot_seed`** (`rust/crates/degenbot-python/src/bot/pump.rs
  :464, 518`) — compares the pinned seed against on-chain at `block_number`.
  The Python caller (`src/degenbot/arbitrage/engine_registry.py:181, 313,
  320`) passes `self._verify_snapshot_block`, which is **set once at boot to
  `engine.snapshot_seed_block`** (the global `S`).
- **Each pool's `snapshot_seed`** (`V3PoolState.snapshot_seed` /
  `V4PoolState.snapshot_seed`) — `Option<HashMap<i32, TickInfo>>`, set in
  `from_params` when `coverage == Tracked` (the seed is the registration-time
  tick_data, pinned for later verify). `take_v3_snapshot_seed` consumes it.

### 2c. The race removing the Store exposes

With the Store gone, each pool's `fetch_liquidity_map` happens at
registration time (spread across `build_paths`), NOT as one bulk boot read.
The `pool_updater` is a **separate process** writing to the same SQLite DB.
If the updater advances a pool's rows to block `S+10` mid-`build_paths`:

- The global `S` was set once at boot (stale the moment the updater writes).
- A pool registered AFTER the write reads its rows at `S+10`, but verify
  uses the boot-time `S`. `verify_*_snapshot_seed` compares the `S+10`
  seed against on-chain at `S` → **false verify failure**.

The Store prevented this by freezing the entire DB cut at boot. Removing it
exposes per-pool reads to the updater's writes.

**Note the race is already partial pre-removal.** The Store only covers pools
present at boot. A pool added by the updater AFTER boot (new `liquidity_positions`
rows for a new pool) misses the Store (`take()` → `Sparse`) and the Db arm
reads it live — already subject to this race. So removing the Store extends an
existing, already-tolerated race to all pools (not a brand-new hazard).

### 2d. The decision: WAL held read transaction (Approach 1)

**Recommendation: Approach 1 — hold one deferred read transaction across
`build_paths`.** SQLite WAL already provides the snapshot isolation the Store
was hand-rolling; the bot just has to read inside one transaction.

In WAL mode, SQLite is MVCC. A **read transaction** (any `BEGIN … SELECT …
COMMIT` on one connection) sees the database **as of when the first read in
that transaction started**. Concurrent writer commits — from the updater
process, on its own connection — do *not* perturb the reader's view. Readers
dont block writers; writers don't block readers.

So the Store's "freeze at boot, hand out frozen cuts at registration"
guarantee is **already a primitive SQLite provides** — the bot just has to
read inside one transaction. Today, each `fetch_liquidity_map` call does its
own `BEGIN…SELECT…COMMIT` (atomic per-call, but each call opens a *new*
snapshot — possibly advanced by the updater). That's the only reason the
race exists.

**Mechanism:**

- Acquire a single `BEGIN DEFERRED` (snapshot at first read) before
  registration starts; run all N `fetch_liquidity_map` calls + the
  `fetch_newest_update_block` read inside it; `COMMIT` when `build_paths`
  finishes. Every per-pool read then sees the **same boot snapshot**, regardless
  of updater writes. The global `S` read inside that same tx matches the data
  — so verify-at-`S` is correct for *every* pool. **No per-pool verify block
  plumbing needed; no behavior change to verify.**
- The bot's connection is already `PRAGMA query_only=on`, so the held tx stays
  a read tx — it can't accidentally upgrade and take a write lock.
- Implemented as `DegenbotDb::open_snapshot_tx()` → an RAII handle owning the
  `MutexGuard` + `Transaction`, with the per-pool read taking `&SnapshotTx`.
  The `assemble_*` Db arm changes from `Option<&DegenbotDb>` to
  `Option<&SnapshotTx>` (or a trait), or the builder acquires the tx and
  drains through it.
- rusqlite exposes `Connection::transaction_with_behavior(
  TransactionBehavior::Deferred)` — no `sqlite3_snapshot` FFI needed.

**Behavior test (landed):**
`rust/crates/degenbot-db/tests/wal_snapshot_isolation.rs` confirms the model
end-to-end with two connections to one WAL file (reader + writer):
- `held_read_transaction_freezes_view_across_concurrent_writer_commits`:
  inside one held deferred tx, two `last_update_block` reads see the same
  pre-write block (100) despite the writer advancing to 110 between them;
  tick rows read inside the tx also reflect the pre-write snapshot; after
  commit, a fresh read sees the writer's 110. Plus a `PRAGMA journal_mode`
  self-assertion confirms WAL is active.
- `per_call_reads_see_concurrent_writer_advance` (negative control):
  mirroring today's per-call `lock()` shape, the second read sees the writer's
  advance — the race the held tx closes.

### 2e. Rejected alternatives

- **Per-pool verify block (Option A, the earlier draft recommendation).**
  The Db arm does NOT pin globally; each pool's verify uses its exchange
  `last_update_block`. Correct but requires plumbing the exchange block through
  the assembly module → registration params → `V3/V4PoolState` per-pool
  `snapshot_seed_block` → the Python verify seam. Approach 1 achieves the same
  correctness with **zero verify-path behavior change** (the global `S` stays
  the verify block because it's read inside the same snapshot as the data),
  so Option A is strictly more work for no gain. Documented below as a
  fallback if a future consumer (e.g. a long-running bot that wants live
  registrations after startup) needs the held tx released between pools.
- **Pin the Db arm to a frozen block (Option B).** Requires threading a
  "frozen block" through the assembly module + filtering `liquidity_positions`
  rows by it. But the DB has no per-row block — a pool's block IS its
  exchange's `last_update_block`, which the freeze would have to read first,
  then re-read the rows — same value, extra query, circular. Approach 1 gets
  the same freeze from SQLite directly.
- **Keep a non-Store bulk pre-warm (Option C).** Boot does
  `stream_liquidity_maps` into... what? If into a `HashMap` handed to the Db
  arm, that's the Store. If nowhere, it's a no-op. This is the Store with a
  different name and is rejected.
- **`sqlite3_snapshot` capture-and-reopen per query (Approach 2).**
  `sqlite3_snapshot_get()` captures a marker; `sqlite3_snapshot_open()` starts
  a *new* short read tx at that historical marker, freeing the connection
  between reads (preserving consistency without holding the lock across
  `build_paths`). **Not available in rusqlite 0.40** (no `snapshot.rs` module;
  libsqlite3-sys doesn't bind `sqlite3_snapshot_*`) — would need raw FFI or a
  rusqlite PR. Worth it only if holding the connection across `build_paths`
  turns out to actually block something we care about; no such consumer seen
  during startup (verify reads use `BotState`, not the DB; the async RPC stays
  lock-free). Approach 1 likely suffices.

### 2f. Implications for the operator-discipline canary (optional)

Approach 1 makes the held-tx snapshot **correctness-preserving**: even if the
operator runs the updater concurrently with startup, the bot's reads are
consistent. The operator discipline ("don't run updater during startup")
becomes a performance hint, not a correctness floor.

If the operator still wants to detect violations post-hoc, a cheap canary:
read `S = min(last_update_block)` at bot start inside the snapshot tx, re-read
it after `build_paths` in a **fresh** tx (sees the live, possibly-advanced DB),
and warn if `S_live > S_snapshot`. SQLite itself won't tell a reader "a write is
in progress" — that's the whole point of WAL MVCC (readers are isolated). So
"awareness" is a post-hoc canary, not an in-flight signal.

## 3. Consumer inventory

`rg -n 'SnapshotStore|seed_from_store|load_snapshot_from_db|load_v3_family|
load_v4_family' rust/crates/ src/` produces:

### Production-path (to be removed/modified)

| Symbol | Location | Role |
|---|---|---|
| `SnapshotStore<K>` struct | `rust/crates/degenbot-bot/src/bot_core/snapshot_verify.rs:233` | the type itself — REMOVE |
| `SnapshotStore::{new,is_loaded,load,begin_load,insert,take,clear}` | same file, `:237-289` | the methods — REMOVE |
| `BotState.v3_snapshot` / `v4_snapshot` fields | `rust/crates/degenbot-bot/src/bot_core/mod.rs:144, 147` | the Store holders on state — REMOVE |
| `BotState::v3_snapshot_store()` / `v4_snapshot_store()` getters | `mod.rs:1406, 1414` | exposed to PyO3 for the helper's Store-arm closure — REMOVE |
| `BotState::register_v3_pool` `seed_from_store` branch | `mod.rs:315-327` | the `take()` re-seed path — REMOVE branch (vestigial per §1a) |
| `BotState::register_v4_pool` `seed_from_store` branch | `mod.rs:3086-3096` | V4 twin — REMOVE branch |
| `load_v3_family` / `load_v4_family` | `mod.rs:4242, 4271` | the store-feeders — REMOVE (boot no longer streams) |
| `BotState::load_snapshot_from_db` | `mod.rs:4099` | SHRINK to "set `S` only" (keep `fetch_newest_update_block`, drop the stream calls) |
| `RegisterV3PoolParams.seed_from_store` / `RegisterV4PoolParams.seed_from_store` | `rust/crates/degenbot-pools/src/v3_state.rs:108`, `v4_state.rs:113` | the field — REMOVE (vestigial) |
| PyO3 `seed_from_store` computation | `rust/crates/degenbot-python/src/bot/mod.rs:963, 1085` | the formula — REMOVE (always `false`) |
| `assemble_v3_tick_map` / `assemble_v4_tick_map` Store arm | `tick_assembly.rs:148-176, 199-207` | the `store_probe` closure arm — REMOVE (collapse to `Db → Chain`) + drop the `store_probe` param |
| `DegenbotDb::stream_liquidity_maps` | `rust/crates/degenbot-db/src/snapshot.rs:274` | the store-feeder query — KEEP (still used by `fetch_all_liquidity_maps` + tests, see §4) OR retire if unused |
| `run_cl_verification` / `VerifyRpc` | `snapshot_verify.rs:196` | the verify orchestrator — KEEP (verify path stays; only its block input changes per §2d) |
| `take_v3_snapshot_seed` / `v4_snapshot_seed` getters | `mod.rs:1467, 3702` | the per-pool seed accessors — KEEP (verify path consumes the pinned seed) |
| `V3PoolState.snapshot_seed` / `V4PoolState.snapshot_seed` | (in `degenbot-pools`) | the per-pool pinned seed — KEEP (verify still needs it) |
| `BotState.snapshot_seed_block` (`S`) | `mod.rs:156` | the global `S` — KEEP (block_pump auto-backfill needs it) |
| Python `Bot.__init__` call | `src/degenbot/bot/_bot.py:151` | `py_bot.load_snapshot_from_db(...)` — KEEP (still sets `S`; just no longer streams) |
| Python `_verify_snapshot_block` | `engine_registry.py:181` | the block passed to verify — CHANGE to per-pool exchange block |
| Python `_verify_pool_seed_at_block` calls | `engine_registry.py:471, 545` | the call sites — pass per-pool block |
| `.pyi` stubs | `src/degenbot/_ffi/__init__.pyi:703, 709, 747` | docstrings mentioning the Store — UPDATE |
| standalone example | `rust/crates/degenbot/examples/standalone_consumer.rs:50, 75` | calls `load_snapshot_from_db` — KEEP (still sets `S`) |

### Test-only (to be updated)

| Symbol | Location | Role |
|---|---|---|
| `tick_assembly/tests.rs` Store-arm tests | `tick_assembly/tests.rs:19, 195, 291, 319` | construct `SnapshotStore` + probe — REWRITE as Db-arm tests (Store arm gone) |
| `snapshot_verify.rs` store-methods tests | `snapshot_verify.rs:414-470` (unit tests on `load`/`insert`/`take`/`is_loaded`) | REMOVE with the type |
| `uniswap_engine/tests.rs` `seed_from_store: false` lines | many (≈20 sites) | the field literal — REMOVE (field gone) |
| `uniswap_engine/diagnostic.rs`, `log_dispatcher.rs`, `reorg_coordinator.rs` `seed_from_store: false` | several sites | same — REMOVE |
| `mod.rs` tests `seed_from_store: true` cases | `mod.rs:6241, 6283` (`register_v3_pool_seed_from_store_*`) | REMOVE (the `true` path is gone) |
| `load_snapshot_from_db_*` tests | `mod.rs:7090, 7139` (+ `degenbot-python` `:2070, 2101, 2121, 2135`) | UPDATE to assert "sets `S` but does NOT populate a Store" |

### Docs (to be updated)

- `rust/crates/degenbot-bot/src/bot_core/tick_assembly.rs` module doc (the `Store → Db → Chain` framing → `Db → Chain`).
- `snapshot_verify.rs` module doc (the Store section).
- `standalone_consumer.rs` doc comments.

### Confirmation: `seed_from_store` field's only consumer

`rg -n 'seed_from_store'` shows the field is set by the PyO3 wrapper
(`bot/mod.rs:963, 1085`) and read ONLY by `BotState::register_v3_pool`
/ `register_v4_pool`'s `if params.seed_from_store { ... }` branch
(`mod.rs:320, 3088`). All other references are test fixtures setting it to
`false`. **Confirmed: the field's only production consumer is the `register_*`
path.** Removing it is safe once the Store arm is gone.

## 4. Boot-time behavior change

### 4a. Order of magnitude

A typical mainnet boot registers on the order of **10²–10³ V3+V4 pools**
(Uniswap V3 alone has ~10k pools deployed; the bot registers its tracked
subset, historically hundreds). Each `fetch_liquidity_map` is a single
indexed `SELECT ... FROM liquidity_positions WHERE pool_id = ?` — one
`Mutex<Connection>` lock acquisition + a prepared-statement query.

### 4b. Cost comparison

- **Today (Store):** one bulk `stream_liquidity_maps` query at boot
  (~all pools, streamed, no materialized `Vec` beyond the Store itself), then
  N `take()` calls at registration (in-memory `HashMap` removes — ~free).
- **After removal (Db arm per pool):** N `fetch_liquidity_map` queries at
  registration time (one indexed `SELECT` each), NO bulk boot read.

For a local SQLite (the deployment shape), N indexed queries are cheap (sub-ms
each). The bulk stream's only remaining justification was the consistency
freeze (§2) — which Approach 1 (the WAL held read tx) replaces. So the
bulk stream can be retired entirely.

**`stream_liquidity_maps` retirement:** check `fetch_all_liquidity_maps`
(`degenbot-db/src/snapshot.rs:258` — materializing variant) and the parity
tests (`rust/crates/degenbot-db/tests/parity.rs:337, 356, 375`). If those are
the only remaining callers, retire them too in a follow-on housekeeping task
(or keep `stream_liquidity_maps` as a DB-crate utility if other consumers
exist). The spike's removal epic should NOT delete `stream_liquidity_maps`
unconditionally — confirm no other (test/tooling) caller first.

## 5. Recommended removal task breakdown

The follow-on children of epic `XEANMB`. Revised for Approach 1 (WAL held
read tx) — this is a strictly *smaller* epic than the per-pool-verify-block
draft, with **zero verify-path behavior change** (the global `S` stays the
verify block because it's read inside the same snapshot as the data).

### Task 5.1 — `DegenbotDb::open_snapshot_tx()` + thread through `build_paths`
- Add `open_snapshot_tx()` on `DegenbotDb` → RAII handle owning the
  `MutexGuard` + `Transaction<'_>` (deferred behavior: snapshot established
  at first read inside). The handle derefs to `Connection` so the existing
  `fetch_*` read methods work unchanged.
- Acquire one tx at `Bot.__init__` (Python) / `load_snapshot_from_db` (Rust),
  hold it across `build_paths`, `COMMIT` when `build_paths` finishes.
- Change the `assemble_*` Db arm from `Option<&DegenbotDb>` to
  `Option<&SnapshotTx>` (or a trait the tx implements) so per-pool reads run
  inside the held snapshot; `fetch_newest_update_block` for `S` runs inside
  the same tx so `S` matches the data.
- Lock-protocol check: confirm holding the `MutexGuard` tx across `build_paths`
  doesn't block a consumer we care about (verify reads use `BotState`, not
  the DB; the async RPC stays lock-free — no conflict).
- WAL `-wal` growth note: the held snapshot prevents the WAL from being
  checkpointed past that point for the duration of `build_paths`; the
  updater's own checkpoint catches up once the bot drops the tx.

### Task 5.2 — Collapse `assemble_*` to `Db → Chain`, remove Store arm
- Drop the `store_probe` param + Store arm from `assemble_v3_tick_map` /
  `assemble_v4_tick_map` in `tick_assembly.rs` and their PyO3 wrappers.
- Update the builders to not build the Store-arm closure.
- Rewrite `tick_assembly/tests.rs` Store-arm tests as Db-arm tests.

### Task 5.3 — Remove `SnapshotStore` + `seed_from_store`
- Delete the `SnapshotStore` struct + impl + `BotState.v3_snapshot`/`v4_snapshot`
  fields + the `v3_snapshot_store()`/`v4_snapshot_store()` getters + the
  `load`/`insert`/`begin_load`/`is_loaded`/`take`/`clear` methods.
- Delete `RegisterV3/V4PoolParams.seed_from_store` + the `if params.seed_from_store`
  branches in `register_v3_pool`/`register_v4_pool`.
- Delete the `seed_from_store` computation in the PyO3 wrapper + all test
  fixtures' `seed_from_store: false`/`true` literals.
- Delete the `register_v3_pool_seed_from_store_*` tests (the `true` path is gone).

### Task 5.4 — Shrink `load_snapshot_from_db` to "set `S` only" (inside the held tx)
- Drop the `load_v3_family`/`load_v4_family` calls + the functions themselves
  (or mark dead if `standalone_consumer.rs`/tests still reference).
- Keep the `fetch_newest_update_block(V3/V4)` `min` → `snapshot_seed_block` set
  (the block_pump auto-backfill still needs `S`) — run inside the held tx from
  5.1 so `S` and the per-pool data share one snapshot.
- Update Python `Bot.__init__` docstring (`_bot.py:138-151`) — "streams V3+V4
  into the core Store" → "opens the snapshot tx + sets the global seed block `S`".
- Update the `load_snapshot_from_db_*` tests — assert `S` is set, the Store
  is NOT populated (it no longer exists).

### Task 5.5 — Docs + `.pyi` + standalone example
- Update `tick_assembly.rs` module doc (`Store → Db → Chain` → `Db → Chain`).
- Update `snapshot_verify.rs` module doc (remove the Store section).
- Update `_ffi/__init__.pyi:703-747` docstrings.
- Update `standalone_consumer.rs` doc comments (`load_snapshot_from_db`
  now opens the snapshot tx + sets `S`; the held-tx lifetime ends at the
  `build_paths` equivalent).
- Update ADR-006 / the relevant migration-guide entry if one references the
  Store as a consistency boundary — point at this findings doc for the
  WAL-tx replacement.

### Task 5.6 (optional) — Retire `stream_liquidity_maps` if unused
Gate on confirming no other (test/tooling) consumer of
`stream_liquidity_maps` / `fetch_all_liquidity_maps` remains after 5.4. If
clean, retire both from `degenbot-db/src/snapshot.rs` + their parity tests.

### Task 5.7 (optional) — Operator-discipline canary
Per §2f: read `S` at bot start inside the snapshot tx, re-read after
`build_paths` in a fresh tx, warn if `S_live > S_snapshot` (correctness was
preserved by the held tx; the canary just surfaces operator-discipline
violations).

## 6. Checkpoint

The spike is complete. The consistency question is **answered**: Approach 1
(WAL held read transaction) is the recommended approach — `DegenbotDb::
open_snapshot_tx()` acquires one deferred read tx at bot startup and holds
it across `build_paths`, so every per-pool `fetch_liquidity_map` + the
`fetch_newest_update_block` read share a single DB snapshot immune to
concurrent `pool_updater` commits. The global `S` stays the verify block
(read inside the same snapshot as the data) — **zero verify-path behavior
change**. The behavior test at `rust/crates/degenbot-db/tests/
wal_snapshot_isolation.rs` confirms the model end-to-end.

**Asking for approval:** Does Approach 1 + the six-task breakdown above look
right to proceed with the removal tasks?

## 7. Removal status (epic `XEANMB` complete)

The Store is gone. Final task dispositions:

- **5.1 (`QSV32Z`) done** — `SnapshotDb` held read-transaction lands the WAL
  consistency mechanism + folds in 5.4 (the `load_v3_family`/`load_v4_family`
  streaming into the Store is gone; `load_snapshot_from_db` reads `S` only).
- **5.2 (`DPRVIP`) done** — `assemble_v3/v4_tick_map` collapsed to
  `Db → Chain` (the `store_probe` closure param + Store arm removed).
- **5.3 (`KRWT2Q`) done** — `SnapshotStore<K>` struct, the `BotState.v3/v4`
  fields, `RegisterV3/V4PoolParams.seed_from_store`, the
  `if params.seed_from_store` branch + all test fixtures deleted. The non-DB
  ingestion surface (`load_*_from_py` / `clear_*_snapshot` PyO3 methods +
  `with_v3_store` / `with_v4_store` helpers) is retired; `start()` now sets
  `snapshot_seed_block` (S) before `subscribe()` so `after_subscribe`
  advances the phase to `SnapshotLoaded`.
- **5.5 (docs) done** — framing references updated (`Store → Db → Chain` →
  `Db → Chain`; lock-protocol section rewritten; standalone_consumer +
  register_v3_pool docstrings updated).
- **5.6 (`DJOWLB`) done — KEEP `stream_liquidity_maps` /
  `fetch_all_liquidity_maps`.** After 5.4 the only remaining callers are the
  parity tests (`rust/crates/degenbot-db/tests/parity.rs`). They are clean
  streaming APIs a future bulk-read consumer could use; retirement is not
  required for correctness.
- **5.7 (`ERZ5MS`) done** — operator-discipline canary landed.
  `SnapshotDb::close_with_canary(s_snapshot, chain)` commits the held tx,
  re-reads `S_live = min(fetch_newest_update_block(V3), V4)` in a fresh
  autocommit tx on the same connection (the held snapshot was released by
  `COMMIT`, so the next `SELECT` sees the live DB), and returns a
  `CanaryReport { s_snapshot, s_live, advanced }`. `PyBot::close_snapshot_tx`
  captures `s_snapshot` (read inside the held tx at bot startup) before
  committing, then calls `close_with_canary` + `log::warn!`s if
  `advanced == true` (the `pool_updater` committed concurrently with
  `build_paths`). Correctness was already preserved by the held tx; the
  canary only surfaces the discipline violation. Behavior tests:
  `rust/crates/degenbot-db/tests/wal_snapshot_isolation.rs` (positive case:
  writer advanced during startup → `advanced == true`; negative: no commit →
  `advanced == false`).
