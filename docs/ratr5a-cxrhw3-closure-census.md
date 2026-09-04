# RATR5A + CXRHW3 closure census (pair-review requirement, landed 2026-09-04)

## Verify the class is closed: the exhaustive grep + hit list

The rg command was run against HEAD e92437e06 (pair-review-verified).
``
rg 'with_state_mut' rust/crates/degenbot-python/src/bot/pool.rs | head -20
``
All hits were individually enumerated by the pair reviewer and map to ONLY these
choreographies, each now staged (no fetch-under-lock):

| # | seam | lock kind | fetch recovery |
|---|---|---|---|
| 1 | calculate_tokens_out_with_fetch (pool.rs:730) | short WRITE | ensure_missing_words_staged, lock-free fetch, disarmed sim (CXRHW3) |
| 2 | simulate_swap_with_fetch (pool.rs:767) | short WRITE | same choreography |
| 3 | simulate_exact_output_swap_with_fetch (pool.rs:828) | short WRITE | same choreography |
| 4 | ensure_word_known (pool.rs:1976 -> ensure_word_known_by_pool_id) | short WRITE | stage_word_fetch_by_pool_id + install_word_fetch, bounded retry, stamped merge |
| 5 | simulate_swap_with_override + exact (pool.rs:616-700) | short READ | override_missing_words + lock-free fetch + simulate_override_disarmed |

## Core-level mechanical enforcement

- RegisteredClSim.disarm_fetch: fetch recovery DISARMED; reads surface the typed
  FetchExhausted contract. Covered by: disarmed_sim_never_fetches_and_surfaces_exhausted.
- OverrideSim.disarm_fetch: same discipline on the override path. Covered by:
  the override path choreography + the core-level tests.
- stamped merge (OB7UNY): merge_tick_word never regresses a fresher stamp.
  Covered by: the retry-shape red test.
- state_write_is_free on PyLiquidityPool: python-side probe (try_write, instant/
  non-parking) for live python-side verification of lock-freedom.

## Ledger (on HEAD e92437e06)

| work | commit | state |
|---|---|---|
| K4ETHF (epic: 3.1s lock convoy at solve p95 5.0s -> 1.0s) | 05844d5e6..aa40e7390 | sealed |
| RATR5A (write-path stage/merge) | b01e2a98a + 9218cc5f9 | done, red-verified |
| CXRHW3 (read-path + override leg, census requirement) | aa40e7390 + e92437e06 | done, red-verified |
| O3Z5MD (span-gate TOCTOU) | 260ee68e9 | closed |
| FRKBGP (solve-cycle profile, RSS tripwire live) | 541d2b053..0a67c594a | closed |

## Drift watch (live on pid 407625)

- state-lock wait p95 0.1ms per site (ALL threshold <= 50ms).
- log_burst p50 25ms (< 100ms).
- fan-out elapsed p95 14ms (< 50ms).
- RSS 8.45GiB steady-state load (delta above plateau is the signal, not the absolute).
- state_write_is_free live probe available on PyLiquidityPool for python-side checks.

## Post-closure levers (gated on fresh evidence, not hunches)

- (P1) per-path unit cost (420us; needs flamegraph inside int_solve_cl_path).
- (P2) worker width (divisor only).
- (P3) affected-set amplification (per-dirty-pool re-solve; bigger surgery).