# ADR-037: Engine Mutex sharding (RAYPAR)

**Status: accepted.** Implemented August 2026 (epic C42WKO).

## Context

The `ArbitrageEngine` lived behind a single `parking_lot::Mutex`. Every
operation — solve, resolve, register path, mark dirty, read results — took
this one lock. The lock ordering was:

```
drain_lock (SolveCoordinator) → engine Mutex → BotState RwLock
```

The drain phase held the engine Mutex for the resolve phase (seconds on
heavy drains, walking 2,500+ affected paths), blocking:

- `on_pool_state_updated` (called on **every WS log event** — dozens per block)
- `latest_results` (Python reads solved results)
- `register_path` (Python registers new arb paths)

The RAYPAR lab (`docs/rayon-parallelism-lab.md`) identified the solve
parallelism problem (LPT partition, 4.91→7.80 efficiency). This ADR covers
the complementary lock-contention work: sharding the engine's mutable fields
so non-drain operations don't park behind a drain.

## Decision

Shard the engine's mutable state into independently-lockable partitions:

### T1: `results` → DashMap (commit `eddf219e2`)

`results: HashMap<u64, SolvePathResult>` → `DashMap<u64, SolvePathResult>`.

Python `latest_results` reads snapshot the DashMap shards into an owned
`HashMap` (O(n_results), typically <50 entries) without taking the engine
lock. Writes during `clamp_merge` use the same `insert`/`remove` API.

### T2: `path_resolved` + `path_status` → evaluated and **skipped**

These fields are only accessed during the drain (resolve writes, solve
reads) and `register_path` (writes). `register_path` also writes to
`path_pools` + `pool_to_paths` — still behind the engine lock. Sharding
just `path_resolved` + `path_status` doesn't unblock `register_path`.
Registrations are rare (startup); the ROI didn't justify the risk (resolve
phase has complex borrow patterns with `core.read()` + DashMap refs).

### T3: `dirty_v2/v3/v4` → shared `Arc<DirtySets>` (commit `c5cde784a`)

The dirty sets were plain `HashSet<u64>` behind the engine Mutex. The
`EngineSubscriber::on_pool_state_updated` (called on every WS log event)
locked the engine just to insert one `u64` into a `HashSet`.

Extracted into a shared `Arc<DirtySets>` with per-set `Mutex<HashSet>`.
The subscriber holds a strong `Arc<DirtySets>` + `Arc<StateLock<BotState>>`
and writes the dirty marker under a short per-set lock — zero engine
contention with the drain path. The drain reads (`take_all`) swaps all
three sets out atomically.

## Lock ordering after sharding

```
drain_lock → engine Mutex → [DashMap shard locks | DirtySets per-set locks] → BotState RwLock
```

Each shard lock is independent — no shard acquires another shard while
holding `core.read()`. The `drain_lock` still serializes drains (correct —
two drains on the same engine would corrupt state). The goal is to let
**non-drain** operations (Python reads, dirty marking) not park behind a
drain, not to parallelize drains.

## What was NOT sharded

- `path_pools` / `pool_to_paths` / `path_resolved` / `path_status` (T2
  skipped — would need full sharding to unblock `register_path`)
- `hop_projection_cache` (only mutated during resolve — no cross-thread
  access)
- `delivery` / `phase` (rarely contended)
