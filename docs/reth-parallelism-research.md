# Reth Parallelism Research — Threads, Pools, and Tokio Workers

> Study of how [paradigmxyz/reth](https://github.com/paradigmxyz/reth) organizes parallelism,
> as design input for degenbot's worker fleet (ADR-042) and runtime sizing.
> Investigated against reth at commit `4bf2ff2` (2026-09-09), shallow clone of `main`.
> All file paths below are relative to `crates/` in the reth repo.

## TL;DR — the ten transferable lessons

1. **One tokio runtime for all async work; a taxonomy of dedicated rayon pools for CPU work.**
   CPU-bound tasks never touch tokio's blocking pool — a documented design rule with a
   citation to [ryhl.io/blog/async-what-is-blocking](https://ryhl.io/blog/async-what-is-blocking/)
   (tasks/src/pool.rs, `BlockingTaskPool` doc). Tokio blocking threads and rayon workers are
   sized and *named* independently, per workload class.
2. **Pools are role-named, lazily spawned, and self-documenting**: `cpu-NN`, `rpc-NN`,
   `storage-NN`, `proof-strg-NN`, `proof-acct-NN`, `prewarm-NN`, `bal-stream-NN`,
   `state-ovly-NN`, plus a dedicated `drop` thread. Every pool's thread count is
   independently configurable; sizes come from measured workload classes, not one global
   number (tasks/src/runtime.rs `RayonConfig`).
3. **Every pool is lazy** (`WorkerPool` wraps `OnceLock<rayon::ThreadPool>`): threads exist
   only after first use, so idle subsystems cost zero threads (pool.rs `WorkerPool`).
4. **Sizing follows workload kind, not just core count**: CPU pools default to
   `available_parallelism`; the storage pool defaults to a fixed **16** threads (I/O-bound —
   more threads than cores); the two trie proof pools default to **2x** available parallelism
   (I/O-stalled proof work); the state-trie overlay pool is a small fixed **4**.
   (runtime.rs L39–49, default_thread_count). There is also an explicit
   `DEFAULT_RESERVED_CPU_CORES = 2` constant — with a candid `TODO` that reservation is
   *currently ignored* because "subtracting from thread pool sizes doesn't actually reserve
   CPU cores" (runtime.rs L211–217). Reth names the aspiration; it does not implement it.
5. **Concurrency limiting is a first-class primitive separate from pool sizing**:
   `BlockingTaskGuard` — a Tokio `Semaphore` wrapped around a pool — rate-limits expensive
   operations (RPC tracing, `eth_getProof`) with a default of 512 concurrent tasks, and
   callers can acquire *weighted* permits via `acquire_many_owned`. The debug RPC uses a
   guard of **1** (rpc/src/debug.rs L1531). Sizing says how many threads exist; the guard says
   how many heavy jobs may run at once.
6. **Shutdown and panic discipline are built into every spawn.** Every task is spawned
   wrapped in `select(shutdown_signal, task)`; "critical" tasks are additionally
   `catch_unwind`-wrapped and report `TaskEvent::Panic` to a `TaskManager` that triggers —
   and waits for — graceful shutdown, with a timeout variant (tasks/src/runtime.rs,
   `spawn_critical_task`, `graceful_shutdown_with_timeout`). OS-thread variants exist for
   tasks that must have their own thread (`spawn_critical_os_thread` enters the tokio
   context manually).
7. **Stable named threads as an optimization tool**:
   `spawn_blocking_named(name, f)` routes work to a persistent dedicated OS thread
   (thread-local state reuse, no spawn overhead on hot paths);
   `try_spawn_blocking_named` returns `None` instead of queueing if that thread is busy;
   `spawn_blocking_named_or_tokio` falls back to the tokio blocking pool (runtime.rs).
   Expensive drops get their own `drop` thread (`spawn_drop`).
8. **Strategy selection is an enum, with a cheap-escape hatch and a gate**:
   `PrewarmMode::{Transactions, BlockAccessList, Skipped}` (engine/tree/.../prewarm.rs);
   blocks under `SMALL_BLOCK_TX_THRESHOLD = 5` txs skip prewarming entirely because the
   fixed worker-spawn overhead exceeds the benefit (payload_processor/mod.rs L51);
   the whole parallel pipeline is behind `has_enough_parallelism()` =
   `available_parallelism() >= 5`, with a comment that **enumerates the five required
   lanes** (engine, state-root task, multiproof task, sparse-trie task, storage-root
   workers) (engine/primitives/src/config.rs L84–94). Parallelism is *budgeted lane count*,
   not a vibe.
9. **Static assignment beats work stealing under skew — reth learned this too.**
   The BAL prewarm pool uses **per-worker crossbeam FIFO queues with round-robin dispatch**
   and chunks large read-sets (`WARM_BATCH_SIZE`) "so a single account with a large
   read-set does not serialize onto one worker" (bal_prewarm_pool.rs L93–97). No rayon
   work-stealing in this hot path at all. This is the same failure mode our
   `rayon-parallelism-lab.md` measured (4.91/8 rayon vs 7.80/8 LPT) — reth's answer is
   round-robin + chunking + an explicit fast-path-capable queue-per-worker, plus a
   `end_block()` oneshot barrier that waits for the slowest worker.
10. **Every parallel tail is bounded by a fallback**: the sparse-trie state root task has a
    `DEFAULT_STATE_ROOT_TASK_TIMEOUT = 1s` after which a **sequential fallback is spawned**
    (engine/primitives/src/config.rs L40); blocks execute in bounded batches
    (`max_execute_block_batch_size = 4`); persistence has explicit memory-return and
    backpressure thresholds. Parallelism is always the fast path, never the only path.

## 1. The spine: `reth-tasks` and its `Runtime`

`reth_tasks::Runtime` (the former `TaskExecutor` is now a type alias:
`pub type TaskExecutor = Runtime;`, tasks/src/lib.rs) is a single cheaply-cloneable
`Arc` handle owning:

- **One tokio multi-thread runtime** (`TokioConfig::Owned` with `worker_threads: None` →
  tokio default = `available_parallelism`; keep-alive 15s; thread-name prefix; or
  `TokioConfig::ExistingHandle` to attach to an existing runtime — used by libraries and
  `Runtime::test()` so tests can run inside `#[tokio::test]`).
- **Seven dedicated rayon pools** (the `rayon` cargo feature), each independently sized:
  general CPU, RPC blocking, storage I/O, proof-storage workers, proof-account workers,
  prewarming, BAL streaming, and the state-trie overlay pool — plus a `WorkerMap` of named
  single-thread workers.
- A `BlockingTaskGuard` semaphore for rate-limiting expensive RPC work on the rpc pool.
- Shutdown machinery (below) and `TaskExecutorMetrics` (regular/critical, blocking/non-blocking
  task counters, `IncCounterOnDrop` finished-counters per class).

There is **no per-subsystem tokio runtime** anywhere in the node: each subsystem receives the
same `Runtime`/executor handle and gets *its parallelism capability* (which pool to `install`
on) injected. Backfill, network, RPC, engine tree, ExEx all share one runtime.

Compare degenbot: our `degenbot-core::runtime` sizes the one ambient runtime from a
**cgroup-aware CPU budget** and names workers `degenbot-io-rt-N` (SMTH6M/PE4FPM), and
ADR-042's `degenbot-workers` builds the role-switching fleet. Reth solves the same problem
with the opposite emphasis: it does **not** do cgroup-aware sizing (plain
`available_parallelism`, reservation TODO'd away) and instead invests in *pool taxonomy and
role separation*. Degenbot is ahead on quota-awareness (tokio's default, like reth's, reads
the same host signal that under-reads cgroup quotas in this devcontainer); reth is ahead on
per-role pool hygiene: laziness, naming, independent sizing, and metrics per pool.

### Pool default sizes (runtime.rs `RuntimeBuilder::build`)

| Pool (thread name)                 | Default size                                   | Class       |
|------------------------------------|------------------------------------------------|-------------|
| `cpu-NN` (general rayon)           | `available_parallelism`                        | CPU         |
| `rpc-NN` (+ `BlockingTaskGuard`)  | same as cpu pool; guard max blocking = **512**  | CPU         |
| `storage-NN`                      | fixed **16**                                    | I/O         |
| `proof-strg-NN`                   | **2x** cpu pool                                 | mixed I/O   |
| `proof-acct-NN`                   | **2x** cpu pool                                 | mixed I/O   |
| `prewarm-NN`                      | cpu pool                                        | CPU         |
| `bal-stream-NN`                   | cpu pool                                        | I/O/CPU     |
| `state-ovly-NN`                   | fixed **4**                                     | CPU         |
| tokio workers (`tokio-rt`)        | tokio default (`available_parallelism`)        | async I/O   |
| `drop` (WorkerMap named thread)    | 1, shared                                       | misc        |

All rayon pools are built with
`builder.panic_handler(|_| {})` (pool.rs `build_pool_with_panic_handler`) so a panicking
worker job cannot abort the process — panics are handled at the *call site* via the returned
`thread::Result` / `catch_unwind`, and at the *task* level by the critical-task machinery.

## 2. Workload-routing primitives (pool.rs)

- **`BlockingTaskPool`** — rayon pool wrapped to be awaitable: `spawn` returns an
  `async` `BlockingTaskHandle` (oneshot + `catch_unwind`), so async code can `await`
  CPU/blocking work without ever blocking a tokio worker. A `spawn_fifo` variant preserves
  FIFO submission order within the pool.
- **`WorkerPool`** — rayon pool with **per-thread typed state** (`Worker`: a thread-local
  `Box<dyn Any>` container with `init/get/get_or_init/clear`). Initialize once per thread
  via `broadcast`, then `install` closures that hit thread-local state (no locks). This is
  exactly the "one EVM per worker thread, reused across jobs" pattern degenbot's sim layer
  could exploit. The docs of `with_worker` warn precisely about re-entrancy/yield hazards —
  a `RefCell` thread-local borrow cannot survive rayon running another job on the thread.
- **`spawn_and_wait`** — if already on the pool, run inline; if on another pool, always
  queue to avoid rayon's cross-pool `install` stealing the caller's thread.
- **`in_place_scope`** — converts the calling thread into a pool worker for the duration.
- Queue-wait and job-duration **metrics per pool** ( histogram on jobs: wait-before-start
  and run time) — per-pool saturation observability for free.

## 3. Shutdown, panic, and hot-path thread strategies (runtime.rs)

- `spawn_task`/`spawn_blocking_task` — regular tasks: wrapped in
  `select(shutdown, fut)`; finishing counts drop-increment a metric.
- `spawn_critical_task`/`spawn_critical_blocking_task` — name + `catch_unwind` +
  `TaskEvent::Panic(name, payload)` to the `TaskManager`, whose join handle is found via
  `take_task_manager_handle` (CLI runner resolves it: a panicked critical task fails the
  process).
- `spawn_critical_os_thread` — OS thread + manual `handle.enter()` so the async task
  keeps tokio context; same panic reporting. Used for subsystems that must own a thread.
- `graceful_shutdown`/`_with_timeout` — `GracefulShutdown` counters (`AtomicUsize` of
  inflight graceful tasks) and `initiate_graceful_shutdown` callable from anywhere.
- `spawn_blocking_named`/`try_spawn_blocking_named`/`spawn_blocking_named_or_tokio` —
  the stable-thread pattern with a graceful fallback (worker_map.rs backs it).
- `spawn_drop` — move expensive-to-drop values to the dedicated `drop` thread.

## 4. Execution-layer parallelism (engine tree)

Reth's block pipeline is where the interesting *arrangement* lives:

- **Two-stream split** (prewarm.rs module doc): each incoming payload's transactions are
  split into a **parallel prewarm stream** (executed in parallel into shared caches to warm
  state) and a **sequential execution stream** (the real, order-dependent execution). The
  prewarm side is best-effort and produces *hints* (`StateRootHintStream`) consumed by the
  state root task.
- **`PrewarmMode` enum** selects tx-based prewarm, BAL-based prefetch (EIP-7928 block access
  lists give *authoritative* read-sets), or `Skipped`; the mode enum carries the stream
  capabilities so "the capability dies with the workers instead of outliving them"
  (prewarm.rs L70–80) — RAII ownership of parallel resources, encoded in the FSM.
- **BAL prewarm pool**: long-lived dedicated OS threads (names `bal-prewarm-NNN`), one
  unbounded crossbeam queue each, round-robin dispatch of `Warm` targets, slot batches of
  `WARM_BATCH_SIZE`, broadcast `BeginBlock`/`EndBlock` with a oneshot barrier; each
  worker opens its **own read txn** over parent state. Deterministic, skew-immune
  dispatch (lesson 9 above).
- **Parallel state root** (`reth-trie-parallel`): the state root is computed by a *task* fed
  by either the sequential execution hook or the BAL `hashed_update_stream` — one exclusive
  slot per job (state_root_strategy/mod.rs L869–876) — multiproof chunks of
  `DEFAULT_MULTIPROOF_TASK_CHUNK_SIZE = 5`, sparse-trie pruning at depth 4, and a 1s
  timeout spawning a sequential fallback (lesson 10).
- **Memory/backpressure config** (engine/primitives/src/config.rs): in-memory block buffer
  target 5, persistence threshold 7, backpressure threshold 16 (engine stalls rather than
  growing unbounded), block buffer limit 32, invalid-header cache 256, cross-block cache
  4 GB. All consts with invariant-asserting `const fn`s (e.g.
  `assert_backpressure_threshold_invariant`) — config invariants checked at compile-expansion
  time.

For an MEV bot, the transferable *shape* is: a parallel "warm/speculate" stream feeding
caches for a real order-dependent fast path; authoritative read-sets (when available) turning
speculation into prefetch; round-robin static dispatch for the fan-out; and a serial fallback
path that keeps correctness independent of parallelism health.

## 5. Backpressure discipline (ExEx, downloaders)

The ExEx manager (exex/exex/src/manager.rs) documents its own responsibilities as
"Receiving events, **Backpressure**, Error handling, Monitoring" (L213). Mechanism: command
channels to ExExes are unbounded, but the manager holds a **bounded internal buffer**
(`max_capacity`), tracks a live `current_capacity: Arc<AtomicUsize>` that *tells the
producer (the execution stage) how big a batch it may process*, a `watch` `is_ready` flag,
monotonic notification IDs, and a WAL so overflow is durability, not data loss. Notification
channels are drained cooperatively to avoid stalls (notifications.rs L506). Bounded memory +
producer-visible capacity + WAL = backpressure that slows producers instead of dropping or
deadlocking.

The p2p downloader stack is *generic over* Linear vs Parallel downloader strategies
(net/p2p/src/lib.rs L33), with batch/request concurrency as knobs on the strategy, again a
strategy-object choice rather than a hardcoded scheduler.

## 6. `for_each_ordered` — parallel compute, in-order consume

tasks/src/for_each_ordered.rs implements `ForEachOrdered` for any `IndexedParallelIterator`:
compute in parallel, deliver **in index order** to a sequential consumer on the calling
thread (cache-padded per-index slots + condvar; the blocked consumer does *not* participate
in rayon work-stealing — callers are told to invoke it from a blocking thread). Directly
relevant to a solve→submit pipeline that must preserve ordering (e.g. nonce/commit
constraints) while parallelizing the expensive part. Our rayon-lab's LPT-binned
`std::thread::scope` partition would slot in as the producer of this pattern with zero
solver changes.

## 7. Why rayon, not tokio, for CPU-bound work

Reth answers this explicitly in `tasks/src/pool.rs` (`BlockingTaskPool` doc) — the choice is
deliberate and documented:

1. **Tokio's blocking pool is the wrong home for CPU work.** Reth routes blocking *I/O*
   (disk lookups) to tokio's blocking pool, but runs CPU-bound tasks on rayon, "which
   performs poorly with CPU bound tasks (see <https://ryhl.io/blog/async-what-is-blocking/>).
   Once the tokio blocking pool is saturated it is converted into a queue; blocking tasks
   could then interfere with the queue and block other RPC calls." In other words, the tokio
   blocking pool is a shared resource serving every `spawn_blocking` caller; saturating it
   with CPU work starves all other blocking I/O (RPC handlers included) in the node. Rayon
   gives CPU work a **separate, independently sized lane** (lesson 1).
2. **Dedicated pools get independent sizing, naming, and metrics.** `cpu-NN`, `rpc-NN`,
   `storage-NN`, … are each sized for their workload class and observable per pool
   (section 2). A monolithic blocking pool can only be sized once, for the average workload.
3. **Rayon's scopes permit borrowed state.** `spawn_blocking` requires `'static`; trie and
   tree recursions want to loan shared `&mut` slices / `RefCell`-free structures across
   worker threads. `rayon::scope` / `in_place_scope` (section 2) makes that safe and is the
   tool reth reaches for on these paths.
4. **Work stealing suits irregular, recursive parallelism.** Trie recursion forks into
   unbalanced subtasks; rayon's work-stealing deque balances them automatically. (Where
   skew is measured and harmful — BAL prewarm — reth *drops* rayon for round-robin FIFOs,
   lesson 9; the choice is per-workload, not dogma.)
5. **Async machinery only at the edges.** Rayon results are surfaced to the runtime via a
   thin async wrapper (`spawn` returns a future; panic propagation built in, pool.rs L86+),
   so callers keep a tokio-native API while the compute itself never touches async workers.

Net rule, worth copying into ADR-042: *one tokio runtime for async I/O; rayon lanes for
CPU and scoped-borrow parallelism; tokio's blocking pool reserved for blocking I/O only.*

### First-party receipts for the section-7 rule

- **Tokio defaults are worse than unbounded.** `rpc/rpc-server-types/src/constants.rs` L29–31:
  "tokio's blocking pool, has a default of 512 and could grow unbounded, since requests like
  `eth_call` also require a lot of cpu which will occupy the thread, we can set this to a lower
  value." CPU-bound work wants ~`cores` threads; tokio's blocking pool wants up to 512+, and
  every extra CPU-hogging thread is context-switching overhead with no throughput gain.
- **Reth's concurrency limits are derived from cores, not from the pool default.** Tracing
  requests are capped at `max(available_parallelism - 2, 2)` (constants.rs L36–44); blocking IO
  gets a separate 256-permit semaphore. Limits are workload-specific semaphores layered on pools,
  not inherited pool semantics.
- **Tokio rejects `spawn_blocking` during shutdown.** `stages/stages/src/stages/sender_recovery.rs`
  L340–347 documents a real bug: during the shutdown grace period, tokio refuses new blocking
  tasks, which surfaced as a spurious `RecoveredSendersMismatch` error at stage end. That work
  moved to a raw OS thread. Rayon pools (and std threads) accept work until explicit teardown.
- **Tokio's own docs tell you to do this.** "CPU-bound tasks and blocking code"
  (docs.rs/tokio) gives both options: *"you should use a separate thread pool dedicated to CPU
  bound tasks. For example, you could consider using the rayon library for CPU-bound tasks. It is
  also possible to create an extra Tokio runtime dedicated to CPU-bound tasks, but if you do
  this, you should be careful that the extra runtime runs only CPU-bound tasks, as IO-bound
  tasks on that runtime will behave poorly."* Reth cites this doc via the Ryhl blog link —
  and took the rayon branch, not the second-runtime branch; see the comparison below.

### Why not a second tokio runtime for CPU work

Tokio offers the second option explicitly, and reth declined it. The second-runtime option
would have given reth **one** extra monolithic CPU executor;
reth's design needs **eight** lanes with independent sizes, names and guards (`cpu`, `rpc`,
`proof-strg`, `proof-acct`, `state-ovly`, `prewarm`, `bal-stream`, `storage`). The second tokio
runtime would still (a) inherit `spawn_blocking`'s 512-thread default growth and its
shutdown-rejection behavior (the sender-recovery bug above), (b) still require `'static`
tasks — so trie borrow-around-worker patterns would keep their Arc overhead, and (c) carry the
same mis-fit warning tokio prints: the runtime must run *only* CPU work, which reth's RPC/
proof classes don't satisfy unambiguously (many are IO-dominated). Where reth *does* keep
CPU-touching work on tokio (`eth_call`), it has to strap semaphore guards (`BlockingTaskGuard`)
over the pool to stop that mis-fit from eating the machine. Rayon's fork-join+scopes model is
the natural fit; the "extra runtime" is the concession fallback for people who can't use it.

### How reth actually decides what runs where

| Workload                                        | Executor                                        | Citation |
|-------------------------------------------------|-------------------------------------------------|----------|
| Tracing, `eth_getProof` (CPU-heavy + memory)    | rayon `rpc` pool + `BlockingTaskGuard`          | rpc-eth-api/helpers/blocking_task.rs; constants.rs |
| `eth_call`, gas estimate, `getLogs` (IO-heavy)  | tokio blocking pool + 256-permit IO semaphore   | blocking_task.rs L155+; constants.rs L29–34 |
| Tx signature recovery / tx conversion           | rayon `cpu` pool (`par_iter`, ordered consume)  | engine/.../payload_processor/mod.rs L222–330 |
| Trie proofs (mixed CPU+IO)                      | rayon `proof-strg`/`proof-acct` WorkerPools (2x cores) | tasks/src/runtime.rs |
| Hashed post-state, receipt roots (pure CPU hot path) | dedicated named OS threads (`hash-post-state`, `tx-iterator`) | engine/.../payload_validator.rs L757; mod.rs L270 |
| MDBX read fan-out (BAL prewarm)                 | **128 threads, deliberately oversubscribed** — NVMe needs queue depth 64–128 for peak throughput; reth notes page-cache hits can flip the workload to CPU-bound, where oversubscription "is counterproductive due to context switching, core migration, contention" — and over-provisions anyway | bal_prewarm_pool.rs L135–152 |
| Long IO that must outlive shutdown grace        | raw `std::thread`                                | sender_recovery.rs L340 |

And reth does not treat rayon as free either: the tx-conversion path skips rayon entirely
below `SMALL_BLOCK_TX_THRESHOLD = 30` ("the rayon parallel iterator overhead (work-stealing
setup, channel-based reorder) exceeds the cost of sequential conversion"), converts the first
4 transactions sequentially to dodge rayon's ~1ms stall before index-0 scheduling
(`PARALLEL_PREFETCH_COUNT`, mod.rs L227–241, 322), and streams results in 64-tx windows.

*(Footnote: CLI help for `engine.storage-worker-count` still says "Tokio blocking pool" while
the implementation uses rayon WorkerPools — a stale-doc fingerprint of the migration in
`node/core/src/args/engine.rs` L461–470.)*

## 8. Comparison with degenbot (what to import, what not)

| Concern                          | reth (4bf2ff2)                                        | degenbot today                                    | Verdict |
|----------------------------------|-------------------------------------------------------|---------------------------------------------------|---------|
| Tokio runtime                    | one multi-thread rt, default-sized                    | one multi-thread rt, cgroup-budget-sized          | degenbot ahead (SMTH6M) |
| CPU pools                        | named lazy rayon pools per workload class             | rayon pools in rayon-lab + worker fleet (F1/F2)   | adopt: **lazy** pools + per-pool names/metrics |
| I/O pool sizing                  | fixed 16 / 2x cores for I/O-stalled classes           | single ambient rt                                 | size lanes to workload kind, not cores |
| Concurrency limiting             | `BlockingTaskGuard` semaphore + weighted permits      | ad-hoc                                            | adopt for inline-sim / heavy eth_call fan-out |
| Thread-locals on workers         | `Worker` typed state per thread (`WorkerPool`)     | —                                                 | adopt for per-thread EVM/env reuse |
| Panic policy                     | pool `panic_handler` no-op + per-task `catch_unwind` + critical-task manager     | workspace lints forbid `unwrap/panic`; bot failure_policy | adopt the *critical task report → graceful shutdown* loop |
| Skewed fan-out                   | round-robin static queues + chunking (BAL prewarm)    | rayon lab proved LPT static partition wins        | convergent evidence; ship RAYPAR-T3 |
| Cheap-work bypass                | `SMALL_BLOCK_TX_THRESHOLD` skip + small-block const in payload processor (30) | — | adopt explicit thresholds where fixed overhead can dominate |
| Parallel-tail bounding           | 1s timeout → sequential fallback                      | —                                                 | adopt as a pattern for solve-phase upgrades |
| cgroup quota                     | not handled (reserved-cores TODO)                     | `cpu_budget` + fleet `quota` module            | degenbot ahead |
| Role/permission model            | pools are the roles; guard is the brake               | `degenbot-workers` FSM (T-table, posture)       | degenbot more demanding; reth validates role-splitting |
| Ordered consume                  | `ForEachOrdered`                                      | —                                                 | adopt for post-solve ordering |

### Not worth importing

- Reth's *absolute* sizes (16 storage threads, 2x proof pools, 4 GB caches) are tuned for a
  full node hammering the DB and trie; an 8-core-quota MEV bot should size its own lanes from
  its own budget — the transferable part is "one size constant per named lane, measured per
  class", not the numbers.
- The single giant `Runtime` covering a whole node is heavier than degenbot's need; our
  fleet budget (ADR-042) is the better authority — but reth shows the primitive surface each
  *role* wants (lazy spawn, named threads, guard, metrics) exactly matches what the fleet's
  dispatcher should offer per role.


---

## Follow-up (2026-09): is degenbot's second-runtime solve executor the wrong choice? [RESOLVED 2026-09-10 — see the addendum at the end of this section]

Question re-opened per request: degenbot-bot's `solve_executor.rs` hosts CPU-bound
LPT solve bins on a *private second tokio runtime* (`degenbot-solve-tokio`) — exactly
the "extra Tokio runtime" option tokio's docs concede but warned against in §7, while
reth picks rayon lanes for this slot. Assessment:

**Why the tokio runtime is (mostly) fine here.** All of the section-7 counterarguments
apply to tokio as a *generic* executor; this runtime is a degenerate one:
- No `spawn_blocking` is involved — workers are sized exactly (`cpu_budget::solve_worker_count`),
  never the 512-thread default; the constants.rs/ryhl arguments about pool growth don't bite.
- No scheduler workload, by construction: the drain enqueues exactly `n` one-bin closures
  per cycle (RAYPAR T3: no splitting, no stealing), each a *synchronous* `run_bin()` pinned
  for the whole bin. Tokio's work-stealing, per-task cost, and fairness machinery are all
  bypassed in effect.
- No fork-join-with-borrows: granularity is bin-level; the solve never needs rayon scopes.
- The runtime is bought, not for async machinery, but for *ambient-handle ambience*: a bin
  that needs cloud RPC (sim escalations, affect-cache misses) finds `Handle::try_current()`
  and can `block_in_place + handle.block_on` (inline_hook.rs join_sim_task) — a plain OS
  thread would fail that path (inline_hook module doc).
- The reserved-cores problem, reth's "TODO", is solved by the budget layer regardless:
  worker count is cgroup-budget-derived, not `available_parallelism`.

**Real (latent) costs that argue for the fleet cutover anyway:**
1. **Pinning is emergent, not contracted.** The "sync-inside-async-by-design" invariant
   is a comment. If an `await` ever sneaks into a bin closure, tokio may migrate the
   bin across workers with no compile-time signal — quietly smearing the warm caches /
   allocator arenas that motivate pinning. Fleet seats pin structurally (T6 keyed pin).
2. **Role conflation.** The runtime is simultaneously the CPU-seat host AND the ambient
   runtime escalation RPC nodes reach for. Under burst that couples the bot's two most
   latency-sensitive resources on one lane.
3. **Runtime mis-fit tax.** `enable_all()` pulls time + IO drivers onto a CPU-only lane;
   all 8+ threads share one thread name (`degenbot-solve-tokio`, no atomic-index suffix —
   unlike `degenbot-io-rt-N`), which hurts dumps/census attribution.
4. **It is already declared legacy.** solver_dispatch.rs L2377 marks the tokio stance as
   the migration legacy arm; `fleet_solve_executor.rs` (ADR-042 F3) is the fleet-native
   replacement with explicit T-table state, keyed pins, loud overflow discipline, and the
   same per-path streaming contract.

**Verdict:** keep the historical red line intact — SOMETHING dedicated, sized from the
cgroup budget; don't go back to the shared-ambient-runtime solve. But direct new effort
at `fleet_solve_executor` and retire `solve_executor`'s tokio stance behind the
`fleet.stance` flag → hard cutover, per the repo's no-backwards-compat policy. That is
section 7's reth-shaped rule: *CPU lanes are pinned seats with one-job quantity semantics,
not a runtime.* While the tokio stance still ships, two hygiene items are cheap:
drop `enable_all()` (nothing in the lane legitimately takes the timer driver) and give
workers per-index names.

### Follow-up RESOLVED (2026-09-10, ergo CQLMM2 / LW-T9 — addendum by impl-b)

The §Follow-up verdict is **executed**: the tokio stance is retired and the
follow-up section above now describes history, not the shipped tree.

- `solve_executor.rs` (the private second tokio runtime) and its
  `global_solve_executor` OnceLock are **deleted**; the role-switching fleet
  (`fleet_solve_executor`, ADR-042 F3) is the **only** solve-bin executor.
  `fleet.stance` / `DEGENBOT_FLEET` retired with the stance: the config key
  fails the load loudly for one release (P6YXA6 mirror). The three "real
  (latent) costs" the follow-up listed are closed: fleet seats pin
  structurally (T6 keyed pin, LW-T2 wedge test), Solver seats and the ambient
  escalation lane are separate (sim escalations ride the injected
  `EscalationPort` — never an ambient `Handle`; the `join_sim_task`
  `block_in_place` arm died with the stance), and the mis-fit tax is moot
  (no runtime left to mis-fit).
- The two "cheap hygiene items" are carried verbatim into the deleted module:
  `enable_all()` no longer exists anywhere in the solve lane, and per-index
  seat names (`work-fleet-<role>-<n>`) replace the shared thread name.
- The "sync-inside-async BY DESIGN" invariant comment is retired with the
  stance: seat runtime-freeness is now enforced STRUCTURALLY by the LW-T2
  wedge test (fleet seats observe `Handle::try_current() == Err` from inside a
  live multi-thread runtime), not by prose.

## 10. Method & sources

- Primary source reth repo, commit `4bf2ff2` (2026-09-09), shallow clone inspected locally.
- Files read in full or in part: `tasks/src/runtime.rs`, `tasks/src/pool.rs`,
  `tasks/src/lib.rs`, `tasks/src/for_each_ordered.rs`,
  `engine/primitives/src/config.rs`, `engine/tree/src/tree/payload_processor/mod.rs`,
  `.../bal_prewarm_pool.rs`, `.../prewarm.rs`,
  `.../state_root_strategy/mod.rs` (parallel-BAL gate region),
  `exex/exex/src/manager.rs`, `exex/exex/src/notifications.rs` (grep),
  `cli/runner/src/lib.rs`, `net/p2p/src/lib.rs`, `net/downloaders/src/*` (listing).
- All claims carry their file citation inline; constants carry their line numbers.
- Local cross-references: `docs/rayon-parallelism-lab.md` (RAYPAR), ADR-042 /
  `rust/crates/engine/degenbot-workers/src/lib.rs`, `rust/crates/foundation/degenbot-core/src/runtime.rs`.
