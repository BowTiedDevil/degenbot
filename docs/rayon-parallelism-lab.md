# Rayon Solver-Parallelism Lab Report

> ergo epic **RAYPAR** (7MKIR7). Authors: offline harness + 80/40-path capture corpus.

## TL;DR

The production drain's 4.21/8 achieved parallelism is **not** caused by rayon
overhead, per-item diagnostics, or memory bandwidth. It is caused by **rayon's
work-stealing scheduler failing to load-balance under extreme path-cost skew**.
An LPT (longest-processing-time) static partition over the same 8 threads, same
solver code, same captures achieves **7.80/8 efficiency** (97%) vs rayon's
**4.91/8** (61%) — a **35% wall-time reduction** with zero solver changes.

The single highest-impact intervention (RAYPAR-T3) is to replace the per-item
rayon `par_iter` fan-out with an LPT-pre-balanced scoped-thread partition,
using a resolve-time cost proxy (e.g. total word-boundary-price count per hop)
to bin paths before the solve.

---

## 1. Method

### Harness

`degenbot-solvers/examples/rayon_scale_probe.rs` reconstructs production
`ResolvedMixedPath` inputs (V3 CL hops carrying the production
`build_cl_word_profiles` + `build_cl_crossing_table` precomputes) from the
committed heavy-CL capture fixture (`tests/fixtures/heavy_cl_solve_captures.jsonl`,
420 paths / 87 MB) and re-runs the production solver entry
`mixed::solve_path_with_min_profit` under controlled thread counts.

The harness sweeps:

- **Thread counts:** {1, 2, 4, 8} via per-pool `ThreadPoolBuilder::num_threads`.
- **Closure variants:** bare solve vs production-diagnostics (span enter + 8
  relaxed atomics + K-slowest heap + walk/gate stat reset-take) — to isolate
  per-item closure overhead.
- **Scheduling variants:** rayon `par_iter` (work-stealing) vs
  `std::thread::scope` static partition (contiguous vs LPT-balanced bins).
- **Control experiment:** pure-CPU spin workload through the same rayon pools —
  maps the machine's scheduling baseline (no solver, no memory traffic).

### Golden-lite gate

Each reconstructed path is solved once serially and its profit compared to the
recorded capture golden within `PROFIT_EPS = 100,000 wei`. On the 40-path
validation subset: 14/14 golden-bearing paths matched, 0 excluded.

---

## 2. Results

### 2.1 80-path corpus (bare solve, release build)

| label        | threads | wall_s | sum_item_s | efficiency | item_med_ms |
|--------------|---------|--------|------------|------------|-------------|
| serial-median | 1       | 147.5  | 147.5      | 1.00       | 91.6        |
| rayon-bare    | 4       | 41.0   | 154.8      | **3.77/4** | 95.6        |
| rayon-bare    | 8       | 32.6   | 159.9      | **4.91/8** | 98.9        |
| std-lpt       | 4       | 39.1   | 156.0      | **3.99/4** | 100.4       |
| std-lpt       | 8       | **21.1**| 164.7      | **7.80/8** | 99.3        |
| std-contig    | 8       | 45.4   | 157.3      | 3.46/8     | 102.8       |

**Top-10 per-path medians (us, path_id):** four paths at ~18s (all path 501
across different blocks), three at ~7.3s (path 500), three at ~6.6s (path 400).
**Top-8 share = 60% of total serial work.** max_item = 18.1s.

### 2.2 40-path corpus (full variant sweep)

| label        | threads | wall_s | efficiency |
|--------------|---------|--------|------------|
| rayon-bare   | 1       | 54.6   | 1.00       |
| rayon-bare   | 4       | 17.7   | 3.25/4     |
| rayon-bare   | 8       | 17.6   | 3.31/8     |
| rayon-diag   | 8       | 17.7   | 3.32/8     |
| std-lpt      | 8       | 17.4   | 3.34/8     |
| control-spin | 8      | 0.011  | 7.31/8     |

### 2.3 Production cross-check (KGXFT7 gate-on run, 50 drains)

- Achieved parallelism: 4.21/8 (solve.cpu_us = 1,380s vs phase_us = 328s).
- Per-drain efficiency spread: 2.08–7.46, median 4.74.
- Heaviest drain: 71s CPU in 16.3s wall on 2,529 paths.

---

## 3. Attribution

### Hypotheses tested:

| Hypothesis | Verdict | Evidence |
|---|---|---|
| Rayon scheduling overhead | **Falsified** | Control spin scales to 7.31/8 (near-linear). Rayon provides no friction for uniform work. |
| Per-item diagnostics overhead | **Falsified** | diag vs bare: efficiency 3.32 vs 3.31 (statistical zero). Atomics, span enter, heap = negligible. |
| Memory-bandwidth saturation | **Partially true (minor)** | Per-item median rises 91.6ms→98.9ms (1→8 threads) = +8%. Cannot explain 3.3 vs 8.0 gap. |
| Work-stealing load imbalance under skew | **CONFIRMED — dominant cause** | LPT static partition at 8 threads: 7.80/8 vs rayon 4.91/8. Same threads, same solver, same bandwidth. 35% wall reduction. |

### Root cause

The heavy-CL capture workload has extreme cost skew: the top 8 of 80 paths
(10%) account for **60% of total CPU**. Path 501 alone costs ~18 seconds —
longer than the ideal 8-thread makespan for 72 of the 80 paths.

Rayon's work-stealing splits work in half lazily; when a worker steals, it
takes the **easy** half (shallow on its work queue). The **heavy** items stay on
the original worker's queue and, being unsplittable (one path = one rayon task),
they serialize the tail: 7 threads sit idle while one grinds through an 18s
path.

LPT pre-balances: sort paths by descending cost, greedily assign each to the
least-loaded bin. With 8 bins, the four 18s paths go to 4 different threads; the
remaining 76 paths fill the gaps. No thread is left idle with an unsplittable
giant while others wait.

### Why production shows 4.21 (not 4.91)

Production drains average ~284 paths (vs the 80-path harness), and the closure
adds span enter + atomics + heap + capture check + profit-envelope gate. The
larger item count provides more stealing opportunities (partially compensating
for the scheduling deficiency), but the fundamental skew — a handful of
multi-second paths per drain — remains. The production K-slowest heap names
these paths every drain; they are the same shapes as path 501 in the harness.

---

## 4. Intervention ranking

| # | Intervention | Predicted wall impact | Complexity | Risk |
|---|---|---|---|---|
| 1 | **LPT static partition** (replace rayon `par_iter` with scoped threads + cost-proxied bin packing) | **−35%** solve wall at 8 threads | Medium (new dispatch path + cost proxy) | Low (cutover gate, A/B against rayon) |
| 2 | Solver-side CPU reduction on path 501-class paths (the 18s giants) | Multiplies through all threads at any efficiency | High (solver math) | Medium |
| 3 | Thread-count tuning vs 8-CPU cgroup quota | Marginal | Trivial | Low |
| 4 | Thread-local accumulators replacing per-item atomics | <1% (diag overhead is noise) | Low | Low |

**Recommended:** T3 implements #1. Cost proxy for LPT binning at resolve time:
`∑ word_boundary_prices.len()` across the path's CL hops (available without
solving, correlates with walk combinatorics). The per-drain solve then uses
`std::thread::scope` with 8 LPT bins instead of `par_iter`.

---

## 5. Environment

- Host: AMD Ryzen 9 5900X 12-core / 24-thread.
- cgroup: `cpu.max = 800000 100000` → 8 CPUs quota.
- Rayon pool (production): 8 threads (named `degenbot-solve-{i}`).
- `std::thread::available_parallelism()` = 8 (cgroup-aware).

---

## 6. Artifacts

- Harness: `rust/crates/degenbot-solvers/examples/rayon_scale_probe.rs`
- Fixture: `rust/crates/degenbot-solvers/tests/fixtures/heavy_cl_solve_captures.jsonl`
- Raw CSV: `/tmp/raypar80.csv` (80-path), smoke run inline (40-path).
- ergo: epic 7MKIR7, task 5A3C2P (harness), task QXEVGN (this report).
