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

### Oracle verification of the recorded recurrences (2026-08-03 session)

I reconstructed both the fee-1 pool (block 25672332, `0x76f75965…`) and the
0.55% pool (block 25672140, `0x2a6d5b75…`) from the DB tick_data + on-chain
scalars at the solve block and ran `v4_simulate_swap` at each recorded V4-hop
input. Result — **the tier-3 on-chain oracle matches the solver's PREDICTED
output byte-exactly, not the recorded `actual_out`**:

| block | pool | V4 input | v4_simulate_swap (=solver predicted) | recorded actual_out |
|---|---|---|---|---|
| 25672332 | `0x76f75965…` | 9652 / 3184 | 9649 / 3182 (== solver) | 9640 / 3179 |
| 25672140 | `0x2a6d5b75…` | 185 | 631737964053287 (== solver) | 628304616211100 |

So `predicted == v4_simulate_swap` in every case: the solver's V4-hop crossing
math is **byte-exact against the frozen on-chain state**. The recorded
`actual_out` is consistently *lower* (by a few wei on fee-1, by 0.55% on the
big pool). This **reframes UO3JM4**: the logged `matched=false` overdrafts are
NOT a solver-crossing over-prediction on a fixed state. The output the live
sim observed is less than both the solver and the frozen-state oracle — a
solve-vs-sim **state / fee-at-sim-time** effect (protocol fee 2,048,500 on
the big pool; pool-state drift between solve and sim), not V4 crossing math.
Any fix / tier-3 slice for these particular recurrences must target that
state-consistency seam rather than the crossing calculation.

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
