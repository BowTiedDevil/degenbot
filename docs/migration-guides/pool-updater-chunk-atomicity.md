# Migration Guide: Pool-Updater Chunk Atomicity Invariant + Rust-Owned Chunk Updater

> **Purpose.** This guide defines the exact atomicity invariant the
> `degenbot pool update` chunk loop must hold, documents how the current tree
> breaks it (a half-finished migration that left the chunk-loop orchestration
> + RPC fetching + transaction ownership in Python while moving only the row
> writes into Rust), and specifies the AGENTS.md-mandated fix: move the whole
> update loop — chunk management, RPC event fetching, pool upserts, liquidity
> updates, the `last_update_block` stamp, single-transaction-per-chunk — into
> the Rust core. It is the **contract artifact gating epic `2SFL6I`**
> ("DB updater atomicity hardening: restore single-transaction chunk
> invariant"). `main`'s chunk loop (`git show main:src/degenbot/cli/pool.py`)
> is the reference; the current tree is the regression.
>
> The implementation tasks (§4) are filed as children of `2SFL6I` from this
> decomposition.

## 0. The migration-path framing (AGENTS.md, verbatim)

AGENTS.md §Architectural Vision states the long-term goal: a set of
first-class standalone Rust crates forming a complete, functional MEV bot —
no Python required — where "that core must eventually own **everything** a
functional MEV bot needs: pool/token state, swap math, event decoding,
solvers, the pump loop, swap encoding, *and* the infrastructure currently
still Python-only — **the database (persistence, not just ORM calls), RPC
interaction, pub-sub, price oracles, the DB-aware pool and lending-market
updaters**, simulation, and transaction submission. There is no piece of bot
functionality that lives in Python indefinitely. ... **Rust is the engine;
Python is a driver shell, not a co-implementation.**"

The pool updater's chunk loop sits at the intersection of **three** items
AGENTS.md names verbatim as on the migration path: **the database
(persistence)**, **RPC interaction**, and **the DB-aware pool ... updater**.
Keeping the chunk loop — or its RPC fetching, or its transaction — in
Python therefore strands standalone-usable logic on the Python side, which
the standalone-Rust-core constraint forbids: "anything a standalone Rust
consumer would need to build an MEV bot must live in a core crate from day
one — never 'move it later,' which strands it across the future crate
boundary."

The fix below (Approach A) is the canonical migration AGENTS.md endorses,
not a pragmatic alternative. It is grounded in this language.

## 1. The invariant (the contract)

Reference: `git show main:src/degenbot/cli/pool.py` — the chunk loop
(`pool_update`, ~lines 1640–1830) held the invariant by construction.

### 1.1 Definitions

- A **chunk** is a batch of blocks `[working_start_block, working_end_block]`
  processed for a set of active exchanges `exchanges_to_update` (those whose
  `last_update_block + 1 == working_start_block`, i.e. the exchanges whose
  turn it is in this chunk).
- A **chunk's writes** are: every `PoolCreated`-derived pool row (V2/V3/V4),
  every `Mint`/`Burn`-derived liquidity update (V3/V4 tick bitmap + positions
  + liquidity), the V3/V4 `liquidity_update_block`/`log_index` per-pool
  marker, and the per-exchange `last_update_block = working_end_block` stamp.
- `last_update_block` is the **restart cursor**: the strict upper bound on
  the block range whose pool + liquidity data is durably committed.

### 1.2 The three invariants

1. **Atomicity invariant.** At chunk commit, EITHER (a) every write in the
   chunk is durably persisted in a **single transaction commit**, OR (b)
   NONE of them are (single transaction rollback). No intermediate state is
   observable — including across an interrupt or crash. Pool rows, liquidity
   updates, the per-pool liquidity markers, and the per-exchange
   `last_update_block` stamp all land in the SAME transaction.

2. **Restart invariant.** `last_update_block` is a strict upper bound on the
   block range whose pool + liquidity data is durably committed. Restarting
   from `last_update_block + 1` re-processes only work that was NOT
   committed — never re-applies committed work. (Equivalently: the chunk
  's pool rows + liquidity state + stamp are a single all-or-nothing unit.)

3. **No-duplicate-writer invariant.** At most one connection holds a write
   transaction on the database during a chunk. (The current code violates
   this — see §2.)

### 1.3 Where `main` held it by construction

- `pool_update` opens ONE SQLAlchemy `db_session()` context for the whole
  run (`with db_session() as session`). The shared `session` is threaded
  into every updater function: `pool_updater(w3, ..., session)`,
  `apply_v3_liquidity_updates(..., session=session)`,
  `apply_v4_liquidity_updates(..., session=...)`.
- The stamp is an ORM-dirty attribute on the existing ORM object —
  `exchange.last_update_block = working_end_block` (in the same session,
  no separate connection).
- At chunk end (~lines 1820–1824): `exchanges_to_update.clear();
  session.commit()`. The `session.commit()` flushes every ORM-dirty write
  from the chunk — pool rows, liquidity state, the per-pool markers, AND
  the `last_update_block` stamp — in ONE transaction. An interrupt before
  that line rolls back the whole chunk (no partial state). An interrupt
  after it has the full chunk + the advanced stamp (restart continues from
  `working_end_block + 1`).

This is the contract. The current tree (§2) breaks all three invariants.

## 2. The current regression (cite the code)

The current tree (`src/degenbot/cli/pool.py`) moved the DB writes into the
Rust core (good, per AGENTS.md) but **left the chunk-loop orchestration +
RPC fetching + transaction ownership in Python**. The chunk loop (~lines
448–575) now dispatches each write as a separate `db_*` PyO3 call:

- `pool_updater(provider, ..., database_path=str(bot.config.database.path))`
  → the per-exchange V2/V3/V4 pool-discovery shell, which calls
  `db_upsert_v2_pools` / `db_upsert_v3_pools` / `db_upsert_v4_pools`.
- `db_apply_v3_liquidity_updates(database_path=..., ...)` — one call per pool
  per chunk.
- `db_apply_v4_liquidity_updates(database_path=..., ...)`.
- `db_set_exchange_last_update_block(database_path=..., block=working_end_block)`
  — one call per exchange per chunk (the stamp).

The `session.commit()` at chunk end (line 568) now commits ONLY
SQLAlchemy-owned state — but no writes went through SQLAlchemy. The actual
writes all went through Rust. So `session.commit()` is a no-op for the
chunk's data. (The `session` survives only for SQLAlchemy READS:
`session.scalar(select(PoolManagerTable)...)` + the active-exchange query.)

### 2.1 Each `db_*` PyO3 seam opens its own connection

Every `db_*` seam follows the same pattern (`rust/crates/degenbot-python/
src/db/discovery.rs` + `liquidity_updater.rs`):

```rust
py.detach(|| {
    let (db, _state) =
        degenbot_db::DegenbotDb::open_for_writes(&path)   // ← fresh connection per call
        .map_err(|e| db_err_to_py(&e))?;
    db.upsert_v3_pools(...)                                // ← writes + drops `db`
        .map_err(|e| db_err_to_py(&e))
})
```

A fresh `DegenbotDb::open_for_writes(&path)` per call → a dedicated SQLite
connection per `db_*` invocation → closed when `db` drops at closure end
(`rust/crates/degenbot-db/src/connection.rs:132`). Across one chunk this is
**N connections, N transactions** (one per `db_upsert_*`, one per
`db_apply_*_liquidity_updates`, one per `db_set_exchange_last_update_block`).
No connection and no transaction spans the chunk.

### 2.2 Each core upsert does per-row autocommit `execute()`

`rust/crates/degenbot-db/src/discovery.rs::upsert_v3_pools` (and v2/v4)
loops the input rows and does `conn.execute("INSERT INTO pools ...")` +
`conn.execute("INSERT INTO {subclass} ...")` per row, with **no
`transaction()` wrapper**. In rusqlite's default autocommit mode each
`execute()` is its own implicit transaction → each row is durable
immediately. `apply_v3_liquidity_updates` (`liquidity_updater.rs:365`)
likewise fetches state on `self.lock()` + `persist_v3(...)` writes
atomically on the one connection the call opened — but only for THAT call,
not for the chunk.

### 2.3 The write/stamp desync → the observed bug

Because pool rows (connection #1) + the per-exchange `last_update_block`
stamp (connection #3, via `db_set_exchange_last_update_block`) are separate
connections/transactions, an interrupt between them desyncs:

- Pool rows for `[working_start, working_end]` are durable (connection #1
  committed + closed).
- The exchange `last_update_block` is NOT advanced (the stamp call didn't
  run, or ran for only some exchanges in the `for exchange in
  exchanges_to_update` loop at line 560–565).
- Restart: `working_start_block` = `last_update_block + 1` (unchanged —
  stamp didn't advance) → re-fetch the SAME chunk's `PoolCreated` events
  → `db_upsert_v*_pools` re-inserts pool rows that already exist →
  `UNIQUE constraint failed: pools.address, pools.chain`. **This is the
  user's observed bug.**

### 2.4 The stale-ORM-cache workaround (symptom, not fix)

The current tree reads `last_update_block` afresh each chunk via
`db_fetch_exchange(...)` into `fresh_last_update_block[exchange.id]`
(lines 456–463) rather than trusting the SQLAlchemy session's
`exchange.last_update_block` ORM attribute. This is a workaround for the
desync: because the stamp is written by Rust on a separate connection, the
ORM object is stale. On `main` the ORM-dirty `exchange.last_update_block =
working_end_block; session.commit()` was the atomic pairing — write + stamp
on one transaction. The workaround papers over one symptom (stale read) but
leaves the root cause (no chunk atomicity) untouched.

### 2.5 Framing: a half-finished migration

This is not a design choice gone wrong — it is a **half-finished
migration**. The DB writes were moved to Rust core (correct, per AGENTS.md),
but the chunk-loop orchestration + RPC fetching + transaction ownership
stayed in Python. That stranded orchestration is standalone-usable logic
(the exact thing a pure-Rust MEV bot consumer would need), which AGENTS.md
forbids: "do not strand standalone-usable logic on the Python side."
Restoring the invariant therefore requires finishing the migration — moving
the loop itself into Rust — not patching the Python loop.

## 3. THE approach (A) + the rejected alternatives

### 3.1 Approach A — Move the whole update loop into Rust

A Rust function — e.g.
`run_pool_update(database_path, chain_id, exchange_specs, to_block,
chunk_size, rpc_url, progress_callback) -> UpdateReport` — owns the whole
run: chunk management, RPC event fetching (via `degenbot-rpc`, §3.2), pool
upserts, liquidity updates, the `last_update_block` stamp, single-connection
+ single-transaction-per-chunk commit/rollback, interrupt polling between
chunks. The PyO3 seam is ONE call to start the run; Python boots the
process + reports progress via the callback. **ADR-005 three-layer
placement:**

| Layer | Where | Holds |
|-------|-------|-------|
| **Rust core** | `degenbot-db` (existing writes) + `degenbot-rpc` (existing RPC) + a new `pool-updater` module or crate | the chunk loop, RPC fetches, single connection + transaction per chunk, writes, stamp, commit/rollback, cancel polling. **Zero `pyo3`** (enforced by `just check-no-pyo3-in-cores`). |
| **PyO3 wrapper** | `rust/crates/degenbot-python/src/pool/` | `#[pyfunction] run_pool_update(...)` + a `ProgressCallback` type. Thin: arg extraction → GIL release → core call → result wrap. |
| **Python companion** | `src/degenbot/cli/pool.py` | boot config → build/omit exchange specs → call `run_pool_update` → report progress (tqdm ticks on the callback). No SQLAlchemy session, no per-call `db_*` dispatch, no per-event Python iteration. |

The existing core write fns (`upsert_v*_pools`,
`apply_v*_liquidity_updates`, `set_exchange_last_update_block`) are **reused,
not rewritten** — but bound to the chunk's own single connection + wrapped
in one `transaction()` per chunk. The `open_for_writes`-per-call pattern is
retired for the chunk path (the chunk owns the connection); the standalone
`db_*` seams can remain for ad-hoc/test uses.

### 3.2 RPC-layer decision: REUSE `degenbot-rpc` (no port needed)

**Inventory result.** `rust/crates/degenbot-rpc/` is a real, mature core
crate (NOT a stub). `lib.rs` exports three modules: `provider`, `contract`,
`subscription`. `provider::AlloyProvider` (built on `alloy` 2.0 with
`transport-throttle`) exposes exactly the RPC primitives the chunk loop
needs:

- `get_block_number()` — `eth_blockNumber` (chain tip; the
  `to_block` resolution + the "is `to_block` ahead of chain tip" guard).
- `get_logs(&LogFilter)` — `eth_getLogs` (the `PoolCreated` / `Mint` /
  `Burn` event fetch).
- `get_block(block_number)` — `eth_getBlock` (block-tag resolution for
  `latest`/`safe`/`finalized`).
- `get_code(...)`, `eth_call(...)`, `estimate_gas(...)`,
  `get_transaction(...)`, `get_transaction_receipt(...)` — present but not
  needed by the chunk loop.
- `LogFilter` (`provider.rs:142`) + `LogFetcher` (`provider.rs:965`) — the
  big-block-range **chunked log fetching** the loop needs (a V3
  `Mint`/`Burn` scan over millions of pools can span huge block ranges;
  `LogFetcher::new(provider, max_blocks_per_request)` already paginates a
  range into per-request chunks with retry logic). IPC endpoint support is
  in (the local-node case).

**Decision: REUSE.** The chunk loop's RPC needs are fully met by
`degenbot-rpc`. **No RPC porting is in scope for this epic.** The §4 task #1
reduces from "port eth_getLogs/etc." to "wire `degenbot-rpc::AlloyProvider`
+ `LogFilter`/`LogFetcher` into the new `run_pool_update` core fn, decoding
the returned `alloy::Log`s into the existing `LiquidityUpdateEvent` /
`PoolCreated`-row inputs the core writers already consume." The
decode-from-`alloy::Log` step may need a small pure-Rust leaf (event
signature + topic decode → `V3PoolRowInput`/`LiquidityUpdateEvent`), which
belongs in `degenbot-decoders` (a core crate) — that is the only genuinely
new Rust surface, and it is decode-only (no state, no FFI).

This is consistent with AGENTS.md listing `degenbot-rpc` among the core
crates: the infrastructure was pre-positioned for exactly this kind of
consumer.

### 3.3 Sub-questions (resolved)

- **Progress reporting.** The current loop uses `tqdm` progress bars in
  Python. The Rust run surfaces progress back via a **callback**: a
  `#[pyfunction]`-callable `ProgressCallback` (Rust trait, PyO3 bridge) the
  core invokes at chunk boundaries with `(blocks_processed,
  current_chunk_start, current_chunk_end, pools_added, liquidity_updated)`.
  Python's tqdm ticks on the callback. The UX is preserved; only the loop
  moves.
- **Exchange specs.** The per-chain exchange configs (factory addresses,
  fee denominators, kind discriminators) currently dispatch in Python via
  `POOL_UPDATER[chain_id, exchange.name]`. **Preferred: Rust reads the
  `exchanges` table itself** at run start (the DB is the source of truth —
  AGENTS.md "Rust owns the state") and builds a typed `ExchangeSpec`. What
  is NOT in the DB (event-topic hashes, the V2/V3/V4 family discriminator
  string) moves to `degenbot-abi` / `degenbot-decoders` as constants. The
  PyO3 seam takes only `(database_path, chain_id, to_block, chunk_size,
  rpc_url, progress_callback)` — no per-exchange Python list. (If a
  temporary "explicit exchange list" override is needed for a migration
  seam, it can thread a `Vec<ExchangeSpec>` through FFI, but the DB-read
  path is the target.)
- **Interrupt handling.** SIGINT during the Rust run must abort cleanly,
  rolling back the current chunk's transaction. Because `py.detach`
  GIL-releases the run, a Python-side `KeyboardInterrupt` won't pre-empt
  mid-chunk cleanly — the Rust run must **poll a cancel flag between
  chunks** (set by a signal handler the PyO3 wrapper installs). Contract:
  **SIGINT between chunks → honored immediately (rollback the not-yet-
  started chunk, return a partial `UpdateReport`). SIGINT mid-chunk → the
  current chunk completes atomically (commit OR rollback) before the run
  returns.** Either way, no partial-chunk state is observable.
- **GIL discipline** (per `rust/AGENTS.md`). The chunk's RPC fetches + DB
  writes run GIL-released (`py.detach`); only the progress-callback
  invocations re-acquire the GIL (brief, infrequent — once per chunk). Long
  RPC polls (`eth_getLogs` over a big range) hold NO GIL.

### 3.4 Rejected alternatives (for the record)

- **Approach B — Thread a single `DegenbotDb` connection through the
  existing `db_*` seams (don't rewrite the loop).** REJECTED. Leaves the
  chunk-loop orchestration + RPC fetches in Python (the bottleneck the
  orchestrator flagged), strands standalone-usable logic on the Python side
  (violates AGENTS.md's standalone-Rust-core constraint), and keeps Python
  as a co-implementation rather than a driver shell. It restores the
  no-duplicate-writer invariant but not the "Rust is the engine" framing.
- **Approach C — `INSERT OR IGNORE` / `ON CONFLICT DO NOTHING` on pool
  inserts, without fixing the transaction structure.** REJECTED. Papers
  over the duplicate-pool-insert symptom but leaves the write/stamp desync
  (the exchange `last_update_block` still doesn't advance atomically with
  the writes). It also violates the **restart invariant**: on every restart
  the loop would re-fetch the same chunk's events + silently no-op the
  inserts, paying the RPC + decode cost again for committed work. Note:
  `apply_v3_liquidity_updates` DOES have a per-pool idempotency guard
  (`liquidity_update_block`/`liquidity_update_log_index` marker — events at
  or before the marker are skipped, `liquidity_updater.rs`), so the
  liquidity re-apply is mostly protected; but the pool INSERT has no such
  guard (the duplicate-INSERT failure surfaces before any marker logic),
  and the silent-swallow of constraint violations would mask genuine
  duplicates from distinct exchanges. Approach C papers over a symptom;
  Approach A restores the invariant.
- **Approach D — Keep the loop in Python, wrap the chunk in one DB-level
  `BEGIN`..`COMMIT`.** REJECTED. Leaks transaction handles across the FFI
  boundary (fragile rollback + murky cancel semantics on a transaction
  owned by Rust but orchestrated by Python), and still strands the loop
  logic on the Python side (AGENTS.md violation).

## 4. Implementation task decomposition (file as children of `2SFL6I`)

Each task is ~1 commit with clear gates. The §3.2 inventory result (REUSE
`degenbot-rpc`) collapses what was potentially task #1 (a port) into a
smaller wiring task. Dependency edges below.

### Task 1 — `wire degenbot-rpc into the chunk loop's RPC needs`
**Summary.** No port — `degenbot-rpc::AlloyProvider` + `LogFilter` +
`LogFetcher` already provide `eth_getLogs`/`eth_blockNumber`/`eth_getBlock`
+ big-range pagination. Wire them into the new `run_pool_update` core fn;
add the one genuinely new Rust surface: a pure-Rust decode leaf
(`alloy::Log` → `PoolCreated`-row input / `LiquidityUpdateEvent`), living in
`degenbot-decoders` (decode-only, no state, no FFI).
**Depends on:** nothing (the crate already exists; this is the
first-consumer wiring).
**Gates.** `cargo test -p degenbot-decoders` + `cargo test -p degenbot-rpc`
green; `just check-no-pyo3-in-cores` green.

### Task 2 — `exchange-spec + event-config types in Rust`
**Summary.** The typed `ExchangeSpec` (factory, fee_denominator, kind
discriminator, event-topic hashes) the chunk loop dispatches on. Rust
reads the `exchanges` table itself at run start (DB is source of truth);
constants like event hashes move to `degenbot-abi` / `degenbot-decoders`.
**Depends on:** Task 1 (event-topic hashes are decoded there).
**Gates.** round-trip test: build `ExchangeSpec` from a populated `exchanges`
table; assert kind/factory/fee resolution.

### Task 3 — `run_pool_update Rust core function`
**Summary.** The chunk loop: progress-report root, chunk-range computation
(`working_end_block` = min of `last_block`, `working_start + chunk_size -
1`, per-exchange `last_update_block`), single `DegenbotDb::open_for_writes`
conn held for the whole run, a `transaction()` per chunk wrapping every
write (reused `upsert_v*_pools` / `apply_v*_liquidity_updates` /
`set_exchange_last_update_block` bound to that conn), cancel-flag poll at
chunk boundaries. `UpdateReport` returned. Zero `pyo3`.
**Depends on:** Tasks 1 + 2.
**Gates.** §1's three invariants hold by construction (asserted in Tasks
6 + 7); `cargo clippy -p <new-crate> -- -D warnings` clean; no-pyo3 OK.

### Task 4 — `PyO3 seam + progress callback`
**Summary.** `#[pyfunction] run_pool_update(database_path, chain_id,
to_block, chunk_size, rpc_url, progress_callback) -> UpdateReport`. GIL
released across the run (`py.detach`); progress reported via a
`ProgressCallback` PyO3 bridge (Rust trait, invoked at chunk boundaries).
Thin PyO3 wrapper — no business logic.
**Depends on:** Task 3.
**Gates.** `cargo clippy -p degenbot_rs -- -D warnings` clean; `.pyi` stub
+ `__all__` entry; round-trip seam test.

### Task 5 — `Python CLI refactor — pool_update becomes boot + hand-off`
**Summary.** `cli/pool.py::pool_update` becomes: read config → build
exchange specs (or let Rust read the DB) → call `run_pool_update` → report
progress via the callback (tqdm ticks on callback). DROP the SQLAlchemy
session-for-writes, the per-call `db_*` dispatch, the `fresh_last_update_block`
workaround, the per-event Python tqdm iteration, the stale
`session.commit()`. Delete the now-dead `db_apply_v*_liquidity_updates`-
for-the-chunk-path usage (the standalone `db_*` seams can remain for
ad-hoc/test uses).
**Depends on:** Task 4.
**Gates.** `just test-python` green; existing pool-update CLI tests still
pass (or are updated to the new hand-off shape); ruff clean.

### Task 6 — `regression test (contract artifact): chunk interrupt → full rollback`
**Summary.** Reproduce the bug: chunk loop interrupts mid-chunk → assert
the chunk is fully rolled back (no pool rows, no liquidity state, no
`last_update_block` advance) → restart re-processes cleanly with NO
`UNIQUE constraint failed: pools.address, pools.chain`. This is the
contract for the §1 atomicity + restart invariants.
**Depends on:** Task 5 (needs the CLI/C end-to-end path).
**Gates.** the test fails on `main`-style broken code (regression guard) +
passes on the new path.

### Task 7 — `no-duplicate-writer test`
**Summary.** The chunk fn holds a write transaction for the chunk's
duration; a concurrent `DegenbotDb::open_for_writes` during the chunk
blocks/refuses (SQLite `BUSY` → retry-with-timeout per the existing
`busy_timeout=5000` pragma) rather than producing a second concurrent
writer. Asserts the §1 no-duplicate-writer invariant.
**Depends on:** Task 3 (core fn) — can land alongside Task 6.
**Gates.** test passes; no concurrent-writer corruption under interrupt.

### Dependency graph

```
1 (wire rpc) ─┬─► 2 (exchange specs) ──► 3 (run_pool_update core) ──► 4 (pyo3 seam) ──► 5 (cli refactor) ──┬─► 6 (regression test)
              │                                                                                          └─► 7 (no-dup-writer test)
              └─ (event hashes feed Task 2)
```

Tasks 1 + 2 before 3; 3 before 4; 4 before 5; 5 before 6 + 7.

## 5. References

- **AGENTS.md §Architectural Vision** — the long-term goal + standalone-
  Rust-core constraint + the explicit "database (persistence), RPC
  interaction, ... DB-aware pool ... updater" migration-path list. Grounds
  Approach A.
- **ADR-005** (`docs/adr/ADR-005-polars-inspired-three-layer-architecture.md`)
  — the three-layer placement (Rust core / PyO3 wrapper / Python
  companion) + the standalone-Rust-core-as-first-class-concern framing.
- **ADR-003** (`docs/adr/ADR-003-botcore-state-layer.md`) — `Bot` as the
  single Rust state owner (the chunk loop's single-connection +
  single-transaction mirrors the single-state-owner discipline).
- **`docs/migration-guides/three-layer-transition.md`** — the triage
  rubric. The pool-updater chunk loop's disposition is `port-now`: the
  writes ported (`partial`), the loop + RPC + transaction did not (this
  epic finishes the port).
- **`main` chunk loop** (`git show main:src/degenbot/cli/pool.py`) — the
  contract source of truth (§1.3).
- **`rust/crates/degenbot-rpc/`** — the existing RPC core (§3.2 inventory).
- **`rust/crates/degenbot-db/src/discovery.rs` + `liquidity_updater.rs` +
  `connection.rs`** — the existing write primitives + the
  `open_for_writes`-per-call pattern being retired for the chunk path
  (§2.1–2.2).
- Epic `2SFL6I`; substrate chain FCA2HP `ca782237` → MZ55NP `67691f7c` →
  WEDVGE `dbf591d1` → XXAMGS `2b3fff8d` (the heal/retirement track this
  epic runs alongside).