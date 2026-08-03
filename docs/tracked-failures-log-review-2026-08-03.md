# Tracked-failure review of the 2026-08-03 bot run

Source: `logs/bot_run.log` — a single ~10.5 hour live mainnet run
(2026-08-03T04:56Z → 15:26Z), build `6ba3e59a` (production trade-through,
solver-state desync + historical-replay fix landed). The log is 10.4 GB and
was mined with grep/ripgrep; this is the catalog of tracked failures worth
investigating and the exact reproduction data for building tests.

## Headline

| signal | count | verdict |
|---|---|---|
| `SOLVER-STATE] ABORT` / `verified desync` | **0** | fix held over the full run |
| `panicked` / `Traceback` | **0** | no crashes |
| `[sim-revert-swap] ... matched=false` | 90 | 1-wei rounding edges + a few real ones (below) |
| `[sim-fail]` (all) | 121 | mostly benign filtering; 6 are the V4-hop overdrafts (below) |
| `ERC20/Uni:: transfer amount exceeds balance` | **6** | V4-hop overdrafts — see the oracle-verified reframe below |

The two big concurrent soundness fixes (in-range active-liquidity adjust +
the historical-replay guard, ADR-021 fail-fast tripwire) did **not** recur —
0 desync aborts and 0 panics across ~25,670,300 → 25,672,300+ (approximate
2000+ blocks). The remaining tracked failures are the **UO3JM4 V4-hop solver
over-prediction** and a few rarer sim buckets, all non-fatal under
trade-through (the unprofitable path reverts; the pump keeps mining).

## The genuine tracked failures: V4-hop over-prediction (UO3JM4 class)

Six `V3-V4-V3` paths over one ~10.5 h run overdrew their output token because
the solver's V4-hop `hop_outputs[i]` **over-predicted** the on-chain swap by a
small margin → `V4_TAKE(predicted)` withdraws more than the pool actually
settles → `ERC20: transfer amount exceeds balance` (or `Uni::_transferTokens`)
in sim. This is the same class as the original fee-1 (`0x76f75965`) capture
and the ADR-020 Tier-3 oracle target — here we now have it on **three distinct
V4 pools** and three blocks.

Machine-readable record: `tests/fixtures/v4_overprediction_recurrences.json`.

| block | V4 pool_id | path(s) | V4 hop predicted | V4 hop actual | over |
|---|---|---|---|---|---|
| 25672140 | `2a6d5b75…` (tick −288661, liq 1.09e14) | 9354 / 9356 | 631,737,964,053,287 | 628,304,616,211,100 | +3.43e12 (~0.55%) |
| 25672332 | `0x76f75965…` (fee-1 USDC/USDT, same as existing harness) | 35397 / 35396 | 3,182 / 9,649 | 3,179 / 9,640 | +3 / +9 wei |
| 25673381 | `9a5c1d2f…` (tick −262345, liq 1.43e18) | 57150 / 43047 | 772,833,263,957,077 / 14,228,641,859,916,626 | 772,076,574,181,336 / 14,147,240,238,759,279 | +0.1% / +0.57% |

Notes for building fixtures:
- Block **25672332** reuses the **exact** V4 fee-1 pool (`0x76f75965…`) the
  existing `fee1_v3v4v3_solver_fixture` harness targets — the cheapest
  recurrence to fixture (extend `RECORDED_*` in `capture_fee1_v3v4v3_fixture.py`
  and re-run against `TARGET=25672332`).
- Blocks 25672140 / 25673381 need the V3 hop-0 / hop-2 pool addresses resolved
  by their token pairs (see the `[solver-st]` sq/liq/fee table below) before
  the general `V3-V4-V3` harness can reconstruct them.
- Per recurrence the `[debug-v4-solve]` line carries the **live** V4 scalars
  (tick / liquidity / sqrt / protocol_fee / n_ranges) that the static DB does
  not hold; the `[sim-diag]` line carries `solve_block`, `optimal_input` and
  `hop_outputs`. Together they are the full input to `v4_simulate_swap`
  (the tier-3-proven on-chain oracle) — the assertion target is
  `solver V4-hop == v4_simulate_swap == recorded actual`, current state RED.

### What is proven (2026-08-03 session)

I reconstructed each V4 pool, drove `v4_simulate_swap` (the tier-3 byte-exact
on-chain oracle) at the recorded V4-hop input, and probed nearby inputs. The
robust, committed facts:

| pool (block) | solver V4 input | oracle @ input | solver predicted | recorded actual |
|---|---|---|---|---|
| `0x2a6d5b75…` (25672140) | 185 | 631737964053287 = solver | 631737964053287 | 628304616211100 |
| `0x76f75965…` (25672332/35396) | 9652 | 9649 = solver | 9649 | 9640 |
| `0x9a5c1d2f…` (25673381/57150) | 3135 | 772076574181336 = **actual** | 772833263957077 | 772076574181336 |

1. **V4 crossing math is byte-exact.** The production per-hop CL simulator
   (`simulate_walk_path` → `landed_ending_range_index` + `int_simulate_v3_swap`)
   is per-field identical to `v4_simulate_swap` for the ending range, and the
   existing `v4_crossing_solver_vs_sim_parity` sweep proves solver == oracle
   across liquidity × amount on identical states. Reconstructing the UNI pool
   and running the solver's own crossing at the delivered input 3135 returns
   `772076574181336` == the oracle == the recorded actual byte-exactly (asserted
   in the new parity test). So the over-prediction is NOT in the crossing/CL
   math, and protocol fee is irrelevant (2048500 vs 0 identical).
2. On the big pool + fee-1, the oracle at the recorded input equals the solver's
   prediction, and the recorded actual reproduces exactly at a SMALLER input
   (185→184, 9652→9643) — a few-unit forward-amount discrepancy.
3. On the UNI pool, the recorded actual equals the oracle at input 3135, but the
   solver's predicted 772833263957077 is NOT reproduced at input 3135 on the
   on-chain-at-block state NOR on the solver's own snapshot scalars (both give
   lower values). The live predicted maps to the oracle at ~3 units MORE input
   — a solve-vs-sim divergence that single-block reconstruction cannot pin.

**Root cause — discovered via the revm-powered V4 parity harness.**
The new `[sim-revert-swap]` instrumentation captured a genuine fee-1 overdraft
live (path 10338, pool `0x76f75965`, block ~25675755) and the on-chain
pre-state reproduces it. Probing the oracle at the derived inputs settles it
unambiguously:

| input | v4_simulate_swap |
|---|---|
| 4728 | **4726** = the recorded actual
| 4729 | **4727** = the solver's predicted

The int-solve CL crossing path (`build_int_v4_sequence` → `compute_crossing` /
`int_simulate_v3_swap`) evaluates each tick range as a **single floored step**, missing the zero-amount **current-tick interior flooring** the on-chain PoolManager (and `v4_simulate_swap`) apply at the current tick's word boundary — so it over-predicts output by a few wei on **zero-for-one** CL hops. Byte-exact proof on the real fee-1 pool (zfo=true, input 4728): on-chain = **4724**, single-step collapse = **4727** (+3), two-step floored (current→tick-0→tick−2) = **4724** (= on-chain). Pinned by the RED test `v4_fee1_solver_path_matches_v4_simulate_swap` (all divergences at zfo=true: `4728→ sim 4724, solver 4727`) and the passing guard `fee1_zfo_true_two_step_floored_equivalence`.

The bug fires at **zfo=true only**, and only when the current tick sits exactly on a word boundary (the fee-1 pool's tick 0). The live path-10338 V4 hop is **zfo=false and byte-exact** (its `+1` — `predicted=4727` vs `actual=4726` at delivered input 4728, mapping to `v4@4729` — is a forward-amount gap, NOT the V4 crossing). The V4 crossing itself is exonerated for the actual path direction.

**Fix (committed):** `tick_bitmap.rs::compute_tick_ranges` now re-inserts sqrt(current_tick) as the first interior boundary of range 0 for zfo=true when current_tick is a word boundary, so the int-solve path (`compute_crossing`/`int_simulate_v3_swap`) floors at the current tick exactly like the on-chain PoolManager. This makes the solver byte-exact to `v4_simulate_swap` on the fee-1 pool in BOTH directions — the former RED pin `v4_fee1_solver_path_matches_v4_simulate_swap` is now GREEN, plus the mechanism guard `fee1_zfo_true_two_step_floored_equivalence`. Fix is shared V3/V4 (same `compute_tick_ranges` + `int_simulate_v3_swap`) and validated against the full pools/solvers/bot/tier-3 suites (no regressions).

**Remaining / NOT resolved by this fix:** the live path-10338 `+1` sits upstream on the V3-30 hop0 (zfo=true) whose current_tick is **-201001 — NOT a word boundary** (verified via `cast slot0`), so this word-boundary fix does not apply there. The V3-30 hop0 `+1` (solver 4729 vs on-chain 4728) is a distinct, still-unpinned rounding source (possibly the same zfo=true partial-step class at a non-word-boundary tick, or an inter-hop amount composition effect), pending reconstruction of the real V3-30 pool (0x4e68ccd3) at block 25675755. See `tests/fixtures/fee1_v3v4v3_block25675755.json` and the `fee1_76f75965_*` parity tests.

### Solver-side solve states (from `[solver-st]`, for pool reconstruction)

Block 25672140, path 9354 (V3-V4-V3):
```
hop0 V3 sq=1839110616740431430508143283575753 liq=1519195195733115112 fee=30 zfo=false
hop1 V4 sq=42758191622663401234955 liq=109231844006814 fee=35 zfo=false   (pool 2a6d5b75…)
hop2 V3 sq=1035259069928946174436102479  liq=2827273958750186631 fee=100 zfo=true
```
Block 25672332, path 35397:
```
hop0 V3 sq=3415829185344042634043168 liq=114436806370291191 fee=1 zfo=true
hop1 V4 sq=79231869042278935382727675145 liq=94294142 fee=1 zfo=false   (pool 0x76f75965…)
hop2 V3 sq=1841909906765577915933244850345376 liq=67068011904 fee=25 zfo=true
```
Block 25673381, path 57150:
```
hop0 V3 sq=1841909906765577915933244850345376 liq=67068011904 fee=25 zfo=false
hop1 V4 sq=159379589444074970543365 liq=1432650976603835442 fee=35 zfo=false  (pool 9a5c1d2f…)
hop2 V3 sq=3719938978503056409784328824 liq=10985102101222257902639 fee=5 zfo=true
```

## Other failure buckets logged (mostly benign, worth a look)

| bucket | n | reading |
|---|---|---|
| `IIA` (Insufficient Input Amount) | 39 | solver filters near-zero output after input adjustment; routine |
| `no-profit` | 34 | thin-margin reverts; traded through (non-fatal) |
| `empty` | 34 | empty-revert no-profit guard; benign |
| `CurrencyNotSettled` (V2-V4-V3) | 3 | revert at V4 PoolManager (0x…4444c); token not settled — worth a look |
| `Pancake: K` | 2 | Pancake V2 K-invariant revert (solver V2 math edge) |
| `UniswapV2: K` | 1 | V2 K-invariant revert (solver V2 math edge) |

The `Pancake/UniswapV2: K` and the 0.5–1.3% V4 over-predictions are the most
interesting non-benign items besides the 6 overdrafts; each is a candidate
tier-3 oracle slice per ADR-020.
