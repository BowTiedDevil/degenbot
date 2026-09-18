# StateView mechanism feasibility — block-epoch pipeline

**Spike:** ergo `KWKEVV` (epic `MROOY7` block-epoch pipeline / ADR-041)
**Worker:** spike-worker-3@worktree-stateview, 2026-09-07. Adopted the working harness left uncommitted by the failed `spike-worker` attempt (`rust/crates/degenbot-solvers/examples/stateview_feasibility_probe.rs`); re-ran it twice for stability and completed the live-bot scrape the predecessor did not finish.

## Decision (per the pre-committed rule in the task body)

**Cheap-read (mechanism (a)) for every pool family: V2-family scalars, V3 tickmaps (Uniswap / PancakeSwap / SushiSwap), and V4 tickmaps.** Materialization and COW pass the view-construction leg of the rule with three orders of magnitude of headroom, but the second leg fails: measured solve-gate holds under cheap-read are ≤ 5 ms p99 per path (≤ 1 ms for the gate itself), far below the 100 ms p99 threshold, and once writes are structurally confined to the Streaming stage, the existing `StateLock<RwLock<BotState>>` reads during Resolved..Solved are uncontended by construction. No family qualifies for an exception; no mechanism exception is needed.

| Family | Mechanism | Rule leg 1 (view p99 ≤ 2 ms @ p90 CL density) | Rule leg 2 (cheap-read > 100 ms p99 solve-gate?) |
|---|---|---|---|
| V2 (Uniswap/Pancake/Sushi) | **cheap-read** | n/a (scalars; clone-able in ~76–124 µs full 100k-pool set) | No — 1 ms p99 |
| V3 (Uniswap) | **cheap-read** | full-clone 40–50 ns @ p90=2 ticks (16 µs p99 even at registry max 1536) | No |
| V3 (Pancake/Sushi) | **cheap-read** | same order (p90=4/2 ticks) | No |
| V4 (Uniswap) | **cheap-read** | clone 40–50 ns @ p90=2 ticks | No |

## 1. Corpus and live evidence

### 1.1 Corpus locations (confirmed on disk)

- Heavy-CL solver-replay corpus: `rust/crates/degenbot-solvers/tests/fixtures/heavy_cl_solve_captures.jsonl.zst` (420 paths / 87 MB decoded; packaged 80 KB), referenced by `docs/rayon-parallelism-lab.md` and read transparently via `rust/crates/degenbot-solvers/src/capture_fixture.rs` (`read_fixture`, `DEGENBOT_SOLVER_CAPTURE_*` producer knobs in `rust/crates/degenbot-bot/src/arb_engine/solver_capture.rs`).
- Other committed captures: `heavy_mixed_solve_captures.jsonl.zst`, `live_capture_loop13/17.jsonl.zst`, `live_gatebursts_mixed.jsonl`, `cl_capture_offline.jsonl` (same fixture dir).
- **Tickmap size distribution has no producer-side capture:** the solver captures record precomputed tick-*range* views, not raw map cardinalities. The authoritative size/mix source is therefore the live registry the captures are drawn from: the running bot's pool database (below), read `-readonly` beside the live WAL.

### 1.2 Live pool-family mix and tickmap size distribution (500K-path bot, `~/.local/state/degenbot/db/degenbot.db`, 2026-09-07)

| Family | Pools | Total persisted ticks | Per-pool tick p50 | p90 | p99 | avg | max |
|---|---|---|---|---|---|---|---|
| uniswap_v2 | 521,137 | — (scalar reserves) | | | | | |
| pancakeswap_v2 | 7,973 | — | | | | | |
| sushiswap_v2 | 4,771 | — | | | | | |
| uniswap_v3 | 72,783 | 149,279 | 2 | 2 | 16 | 2 | **1536** |
| pancakeswap_v3 | 1,303 | 4,122 | 2 | 4 | 22 | 3 | 429 |
| sushiswap_v3 | 885 | 1,219 | 0 | 2 | 10 | 1 | 78 |
| uniswap_v4 | 131,428 | 125,824 | 0 | 2 | 4 | 0 | 545 |

Takeaways:

- **90th-percentile CL density is 2–4 ticks/pool.** The p99 is ≤ 22 (Pancake V3); the registry maximum is 1536 ticks (one Uniswap V3 pool).
- V2-family (533,881 pools) state is scalars; V4 maps are sparser than V3 (median empty, avg < 1 tick).
- The predecessor's 682/1536-entry probe sizes bracket the real distribution's tail: 682 ≈ dense-region working set, 1536 = the exact registry maximum.

## 2. Harness measurements

Reproduce from repo root (release build):

```bash
cargo build --release --manifest-path rust/Cargo.toml -p degenbot-solvers --example stateview_feasibility_probe
rust/target/release/examples/stateview_feasibility_probe
```

Harness: `rust/crates/degenbot-solvers/examples/stateview_feasibility_probe.rs` (throwaway; synthetic `HashMap<i32, TickInfo>` of the exact production entry type, 1001 timed reps per size, percentile = ceil-index). `TickInfo = { U128, i128, u64 }` = 48 B; `V3BlockDelta = 144 B`.

### 2.1 M1 — tickmap clone cost sweep vs N (mechanism (c) worst case: a full per-epoch map clone)

Two consecutive runs; stable within ~2× everywhere.

| N | run1 p50 | run1 p90 | run1 p99 | | run2 p50 | run2 p90 | run2 p99 | bytes/entry |
|---:|---:|---:|---:|---|---:|---:|---:|---:|
| 2 | 40 ns | 50 ns | 50 ns | | 40 ns | 40 ns | 40 ns | 80 |
| 4 | 60 ns | 70 ns | 90 ns | | 50 ns | 50 ns | 50 ns | 72 |
| 16 | 150 ns | 160 ns | 160 ns | | 160 ns | 180 ns | 220 ns | 106 |
| 64 | 480 ns | 480 ns | 490 ns | | 480 ns | 490 ns | 500 ns | 114 |
| 256 | 1.49 µs | 1.79 µs | 1.87 µs | | 1.57 µs | 1.59 µs | 1.83 µs | 127 |
| **682** | **3.92 µs** | **4.90 µs** | **5.63 µs** | | 4.15 µs | 4.62 µs | 5.67 µs | 97 |
| **1536** | **8.85 µs** | **11.1 µs** | **16.2 µs** | | 9.44 µs | 11.3 µs | 18.7 µs | 86 |
| 7,394 | 44.2 µs | 53.7 µs | 65.9 µs | | 49.0 µs | 58.9 µs | 78.3 µs | 143 |
| 32,768 | 196 µs | 222 µs | 336 µs | | 218 µs | 242 µs | 314 µs | 129 |
| 65,536 | 419 µs | 468 µs | 618 µs | | 432 µs | 487 µs | 565 µs | 129 |
| 131,072 | 1.48 ms | 2.09 ms | 3.40 ms | | 1.25 ms | 1.41 ms | 1.73 ms | 129 |
| **262,144** | **18.7 ms** | **23.1 ms** | **46.9 ms** | | 17.7 ms | 19.3 ms | 21.9 ms | 130 |

- At the corpus's 90th-percentile density (2–4 entries) a full clone is **40–50 ns**; at the registry maximum (1,536) it is **16–19 µs p99**. Both are ~10⁵× under the 2 ms rule leg.
- **Reproduced predecessor anomaly:** clone cost goes superlinear above ~131k entries (262,144 entries ≈ 33 MB map): 18.7 ms p50 at 262k entries ≈ 4,800× the linear bytes/entry trend (130 B ≈ 395 ns of memcpy-equivalent). Consistent across both runs and with the predecessor's fragment. Hypothesis: allocator chunk-RSS growth + TLB misses at ≥ 16 MB working sets (sweep runs inside one process whose arena has already fragmented). **Out of corpus relevance** — the largest live tickmap is 1,536 entries (125 kB); no live pool approaches this regime. Flagged for any future work that clones whole-registry aggregated maps.

### 2.2 M2 — journal-replay materialization mechanism (b) cost

Restore-before-block over a full 32-block `ReorgJournal<V3BlockDelta>` window + reverse-apply onto a live map, 1001 reps. This is the whole-view construction cost if we materialized views from the delta journals.

| k priors/block | run1 p99 (32 blocks) | run2 p99 | per-block |
|---|---:|---:|---:|
| 0 | 1.67 µs | 0.97 µs | ~0.02 µs |
| 1 | 5.10 µs | 4.61 µs | 0.09 µs |
| 2 | 6.40 µs | 4.33 µs | 0.11 µs |
| 4 | 9.56 µs | 6.10 µs | 0.15 µs |
| 8 | 22.7 µs | 15.4 µs | 0.38 µs |

Production journals carry typically 0–4 priors/block/pool (task body); even the synthetic 8 prior/block stress is **23 µs p99 for a full 32-block rewind** — 100× under the 2 ms leg. The journal-replay mechanism is viable; it simply is not needed (see rule).

### 2.3 M3 — V2-family costs

- `restore_before_block` on a full 32-block V2 (degenerate-full) window: p50 20 ns, p99 30 ns.
- Cloning the scalar reserve state of a 100k-pool set: p50 76.8 µs, p99 124 µs (run1) / 76.6 µs, 109 µs (run2). Even a full-registry (533,881-pool) V2 snapshot is ~0.4–0.7 ms — under 2 ms, but it buys nothing over cheap-read since the state is already under the StateLock.

### 2.4 M4 — COW (Arc) view construction

- `Arc<HashMap>` clone: p50 20 ns, p99 30 ns for both 682- and 1,536-tick maps (O(1) bump). COW's view-construction leg passes trivially; it was rejected on rule leg 2, and because per-epoch snapshots cost real memory (below).

### 2.5 M6 — memory anchors

- `TickInfo` 48 B; `V3BlockDelta` 144 B; `HashMap` clone RSS ≈ **125.8 bytes/entry** (measured 65,536-entry map × 32 clones); ≈ 130 B/entry steady across sizes.
- Per mechanism, whole-registry memory delta at current corpus (280,444 CL tick entries; 533,881 V2 scalar pools):
  - **cheap-read: zero** (state and journals already resident).
  - **COW:** + ~36 MB per retained CL-tickmap snapshot (280,444 × 130 B) + ~21 MB per V2 scalar snapshot (~40 B/pool), per retained epoch.
  - **journal-replay:** + `V3BlockDelta` 144 B per dirty pool-block (+ ~40 B per tick prior) at journal depth 32, only for pools with in-window activity.

## 3. Cheap-read solve-gate hold analysis (rule leg 2)

Live bot, Prometheus scrape `http://127.0.0.1:9464/metrics` (414–437 drain cycles, 19.6 M lock events, 971,791 per-path gate evaluations at scrape time; `degenbot.engine_registered_paths = 500000`):

| Series (s) | p50 | p90 | p99 |
|---|---:|---:|---:|
| `block_header_to_solved` | 0.5 | 0.5 | 2.5 |
| `block_log_burst` | 0.05 | 0.1 | 0.1 |
| `solve_duration` (dirty solve cycle) | 0.25 | 0.5 | 2.5 |
| `solve_path_duration` (per path) | 0.0005 | 0.001 | **0.005** |
| `solve_gate_duration` (per path) | 0.00025 | 0.0005 | **0.001** |
| `state_lock_hold` (19.6 M events) | 0.0001 | 0.0001 | 0.0001 |
| `state_lock_wait` (19.6 M events) | 0.0001 | 0.0001 | 0.0001 |

Jaeger corroboration (`degenbot.arb.solve`, 15 spans / 2 h lookback): p50 369 ms, max 1,122 ms — consistent with the `solve_duration` histogram.

- Per-path solve-gate holds under cheap-read: **p99 5 ms** (path) / **1 ms** (gate alone) — 20× under the 100 ms threshold.
- Whole-drain read phase compounds to the `solve_duration` p99 (2.5 s), but this hold blocks only Streaming-stage writes, which under the stage machine are structurally serialized behind Quiesced (the A+D hybrid's write- confinement is precisely what makes the hold uncontended). The strongest *observed* contention datum is `state_lock_wait` p99 ≤ 0.1 ms across 19.6 M acquisitions **on the current, not-yet-structurally-separated architecture**; stage confinement only removes contenders.
- Clean-read consensus with live replay: the log-burst window (`log_burst` p99 ≤ 100 ms) + settle wait overlaps the solve phase; a read-held map for the drain duration is never re-entered by a writer of the same epoch. Therefore **cheap-read does not exceed 100 ms p99 of consequential gate-hold contention**, and the rule's exception does not trigger.

## 4. Decision rule evaluation, per family

1. **Leg 1** (view-construction p99 ≤ 2 ms at 90th-pctile CL density — i.e. 2–4 ticks/pool, max 1,536): *all three non-default mechanisms pass with ~10⁵ headroom* (clone 40–50 ns; journal-replay ≤ 23 µs; Arc 30 ns).
2. **Leg 2** (cheap-read would exceed 100 ms p99 solve-gate hold): *fails* for every family — measured per-path gate/solve p99 of 1/5 ms, zero measurable lock wait.

Since cheap-read only loses if **both** legs pass, **cheap-read wins for all families**. The journal-replay and COW results remain in §2 as sizing input for the Rewind path (2UVG3E) and any future out-of-corpus growth of the tickmap distribution.

## 5. Sizing consequences for the data-plane task (`2UVG3E`)

- No new view representation: `StateLock<RwLock<BotState>>` stays the data plane; writers confined to Streaming; Resolved..Solved..Simulated take read guards whose measured hold profile is §3.
- StateLock diagnostics (hold/wait histograms) are retained — they are the budget verifier for the "Spike-derived p99 latency budget met on the capture corpus replay" gate.
- The ReorgJournal `restore_before_block` costs (§2.2/§2.3, all ≤ 23 µs p99) bound the Rewind path; no materialization machinery is needed to hit the budgets.

## 5.1 Landed (task `2UVG3E`): the data plane + which locks remain on the solve path

Implementation of the cheap-read branch, as landed in this worktree
(pre-stage-machine; the machine itself is task `7NFYQW`):

- **Data plane unchanged, writers confined by role.** `StateLock<RwLock<BotState>>`
  (parking_lot + the Z4Z6VO diagnostics wrapper) remains the only pool-state
  store. Pool-state writes happen in exactly the Streaming-role paths
  (`Bot::dispatch_log` apply, reorg `ReorgJournal` restore, gap backfill,
  registration); the solve cycle (Solve/Simulate) consumes read guards — the
  resolve window holds one consistent read snapshot, the per-path solves take
  short reads. The one conditional write that used to sit inside the Solved row
  (buffered-event lazy expiry) is default-off and documented below as the
  residual pre-machine exception to retire with the stage machine.
- **Engine `Mutex<ArbitrageEngine>` off the solve path (seam #4).**
  The detached cycle is the ONLY solve arm (WFF6MM hard cutover; the old
  `DEGENBOT_DETACHED_SOLVES` stance and its in-cycle opt-out are retired): the
  drain-driven solve cycle returns at enqueue end, so the
  `EngineHandle::solve_dirty` engine-Mutex hold collapses to µs (probe +
  enqueue + bookkeeping), and results merge on the `arb-detached-merge`
  sidecar under short per-item acquisitions guarded by the Q1a staleness
  oracle. Backpressure is the admission draw (shed+carry) — the old
  `DETACHED_INFLIGHT_CAP` degrade no longer exists.
- **StateLock diagnostics retained for registration/FFI** (the slow operator
  paths the 2026-08-21 incident implicated); the solve path's reads are the
  cheap, `#[track_caller]`-diagnosed bare reads.

**Lock inventory on the solve path after this task** (the contention story):

| Lock | Stage phase | Why it remains |
|---|---|---|
| `StateLock<RwLock<BotState>>` (read) | Resolved..Simulated | THE data plane — cheap-read guards; uncontended by Streaming confinement (I4); diagnostics stay for registration/FFI |
| `StateLock<RwLock<BotState>>` (write) | Streaming only | dispatch_log apply, `ReorgJournal` restore, backfill, registration — the writer side |
| `Mutex<ArbitrageEngine>` | enqueue end (µs); per-item sidecar merges | Cycle hand-off + registration/FFI serialization. Not held during solving (detached default) |
| `SolveCoordinator::drain_lock` | drain fan-out bookkeeping | Coordinator cursor consistency; µs; retired with the seam (task `SZJUKL`) |
| `EpochDelta` internals (`RwLock<Epoch>` + `Mutex<HashSet>`) | Streaming (writes), drain (take) | The touched-pool ledger; `take_keys` is a single brief swap |

## 6. Reproduction commands

```bash
# 1. Harness (map clone sweep, journal replay, scalar + COW costs)
cargo build --release --manifest-path rust/Cargo.toml -p degenbot-solvers --example stateview_feasibility_probe
rust/target/release/examples/stateview_feasibility_probe

# 2. Live pool-family mix + tickmap size distribution (read-only beside live WAL)
sqlite3 -readonly ~/.local/state/degenbot/db/degenbot.db "SELECT 'uniswap_v3', COUNT(*) FROM uniswap_v3_pools;"
WITH n AS (SELECT p.pool_id pid, COUNT(l.id) cnt FROM uniswap_v3_pools p
           LEFT JOIN liquidity_positions l ON l.pool_id=p.pool_id GROUP BY p.pool_id),
     r AS (SELECT cnt, ROW_NUMBER() OVER (ORDER BY cnt) rn, COUNT(*) OVER () total
           FROM n)
SELECT MAX(CASE WHEN rn=CAST(0.5*total AS INT) THEN cnt END)  p50,
       MAX(CASE WHEN rn=CAST(0.9*total-1 AS INT) THEN cnt END) p90,
       MAX(CASE WHEN rn=CAST(0.99*total-1 AS INT) THEN cnt END) p99, MAX(cnt) mx FROM r;
# (repeat with pancakeswap_v3_pools / sushiswap_v3_pools on liquidity_positions;
#  uniswap_v4_pools p JOIN managed_pool_liquidity_positions l
#  ON l.managed_pool_id=p.managed_pool_id)

# 3. Latency + lock series (bot live, ~500K registered paths)
curl -s http://127.0.0.1:9464/metrics | grep -E 'degenbot_(block_header_to_solved|block_log_burst|solve_duration|solve_gate_duration|solve_path_duration|state_lock_(hold|wait))_seconds'
curl -s 'http://host.docker.internal:16686/api/traces?service=degenbot-bot&operation=degenbot.arb.solve&limit=15&lookback=2h'
```

## 7. Caveats for reviewer

1. **M1's clone sweep is synthetic-sized, not drawn from capture files.** The committed solver captures encode precomputed tick-range views, not map cardinalities; the size distribution used to place the sweep is the live registry's (§1.2), which is the corpus these captures are drawn from. The sweep's entry type is the exact production `TickInfo`.
2. **262,144-entry (≈33 MB) superlinear clone anomaly** is real but reproduces only far outside the live distribution (max 1,536 entries). Should the registry ever host >10⁵-tick pools, revisit COW for that pool before cloning.
3. DB read was taken read-only beside a live WAL; counts are point-in-time (2026-09-07) but the shape (p99 ≤ 22 ticks) has been stable across this epic's span.
4. Rule leg-2 reasoning relies on structural write confinement (Streaming ↔ Resolved..Solved separation), which is the A+D hybrid's settled design (Q1/Q3), not a measurement — the measured `state_lock_wait` p99 ≤ 0.1 ms on 19.6 M events is the empirical bound available *above* it (i.e. before that confinement exists).
5. The harness (`stateview_feasibility_probe.rs`) is committed and lint-exempted with a dated throwaway header; supervisor decision at 2UVG3E sign-off: KEEP it as the reusable budget-regression probe (deletion is no longer planned).
