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
---

## CENSUS CORRECTION (pair-review Finding, 2026-09-04)

The initial census had TWO errors: (1) a `head -20` pipe truncated the enumeration — 25
sites exist in pool.rs, not 5; (2) the plain calculate_tokens_out AND calculate_tokens_in
sems were misclassified as "no fetcher by contract — N/A" when sparse V3/V4 pools REGISTER
with the web3 fetcher by construction, making them active fetch-under-write paths.

### Honest enumeration (25 sites, every hit classified)

| # | pool.rs line | fn context | recovery choreography |
|---|---|---|---|
| 1 | 481 | ensure_missing_words_staged (stage) | staged choreography |
| 2 | 501 | ensure_missing_words_staged (install) | staged choreography |
| 3 | 539 | with_state_mut definition | N/A (accessor definition) |
| 4 | 743 | calculate_tokens_out_with_fetch | staged choreography + disarmed sim |
| 5 | 799 | simulate_swap_with_fetch | staged choreography + disarmed sim |
| 6 | 856 | simulate_exact_output_swap_with_fetch | staged choreography + disarmed sim |
| 7 | 900 | simulate_swap_with_override (read) | override_missing_words + lock-free fetch + disarmed sim |
| 8 | 972 | simulate_exact_output_swap_with_override (read) | same as 7 (read path) |
| 9 | 1700 | update_tick_data / write-back | tick install (short write, no fetch) |
| 10 | 1835 | token write-back | N/A (V2/registration) |
| 11 | 1859 | token write-back | N/A (V2/registration) |
| 12 | 1885 | token write-back | N/A (V2/registration) |
| 13 | 1930 | token write-back | N/A (V2/registration) |
| 14 | 1956 | token write-back | N/A (V2/registration) |
| 15 | 2024 | swap apply | N/A (event application, no fetch) |
| 16 | 2037 | swap apply | N/A (event application) |
| 17 | 2074 | liquidity update | N/A (event application) |
| 18 | 2111 | ensure_word_known (stage) | stage choreography (RATR5A) |
| 19 | 2120 | ensure_word_known (install) | stage choreography (RATR5A) |
| 20 | 2233 | liquidity update V4 | N/A (event application) |
| 21 | 2288 | write-back | N/A (V2/registration) |
| 22 | 2314 | tick data snapshot | N/A (V2/registration) |
| 23 | 3203 | swap math (V2-only) | N/A (V2 family, no fetcher) |
| 24 | 3299 | swap math disarmed | disarmed (this census correction) |
| 25 | 3566 | swap math disarmed | disarmed (this census correction) |

### Corrections landed

- pool.rs:743 (calculate_tokens_out) and pool.rs:799 (calculate_tokens_in):
  converted to swap_simulation_disarmed — the documented no-raise-on-miss
  contract is now mechanically enforced (miss recovery cannot run; miss => 0).
- The legacy SwapRead::NotComputable => raise path on line ~766-773 was removed
  for the plain seams: the disarmed contract maps ALL non-computed misses to
  U256::ZERO (the V2 overflow raise is impossible through the disarmed path).
- The census command was corrected: no head pipe, all hits enumerated.

### cdbc03bb correction (post-census, pair-review finding on ae2c4124f)

The correction above conflated two DISTINCT error classes. Restored in
bc3a1c708:

- **Miss class** (FetchExhausted/Failed — sparse-map miss recovery):
  disarmed, maps to U256::ZERO. The no-raise-on-miss contract holds
  (calculate_tokens_out pool.rs:743, calculate_tokens_in pool.rs:799).
- **Math-overflow class** (SwapRead::NotComputable — constant-product mul
  >= 2^256, on-chain getAmountOut SafeMath revert): raised as Python
  ValueError on plain calculate_tokens_out (on-chain parity; the V2
  companion translates to domain LiquidityPoolError). calculate_tokens_in
  keeps the documented silent-0 legacy for this class.

Guard: tests/uniswap/v2/test_uniswap_v2_liquidity_pool.py::test_swap_for_all
gained a banded 2**250 case pair — fits I256 input conversion (2**250 <
2**255) but overflows the mul, exercising the NotComputable raise. The
existing 2**256-1 pair exercises the input-conversion site instead, so the
two classes are separately pinned: re-collapsing the match now goes red.
