# Sim root cause: V4 multi-tick solver approximation (NOT stale state, NOT swap-step rounding)

> Decisive root-cause finding for the `CurrencyNotSettled` failures on V4-V3-V3.
> This **overturns two prior hypotheses**: (1) "stale engine state" (epic
> `TR6GWT`, path A — refuted by V3 hops matching exactly) and (2) "V4
> `compute_swap_step_v4` rounding divergence" (the prior version of THIS doc —
> refuted by tracing the solver path: the solver does NOT use
> `v4_simulate_swap`/`compute_swap_step_v4` for `hop_outputs`; it uses the
> 2-range `int_simulate_v3_swap` approximation).

## TL;DR

The `CurrencyNotSettled` revert is a **solver approximation error**, not stale
state and not a swap-step rounding bug:

- **The solver** computes `hop_outputs` via `int_simulate_v3_swap`
  (`degenbot_solvers/src/mobius_v3_int.rs`) — a single-tick-range CL swap model
  chained through an optional `IntTickRangeCrossing` (base_range → one crossing
  → ending_range). This 2-range model is EXACT for swaps that stay within one
  range or cross exactly ONE tick boundary, but **approximates** swaps that
  cross 2+ boundaries (it cannot walk the full tick bitmap like
  `compute_swap_step`).
- **The V4 hop** (USDC/WETH 0.3% pool, thinner liquidity) crosses 2+ tick
  boundaries for the arb input → the 2-range model **over-estimates** the
  output (always `predicted > actual`). The composer's
  `V4_TAKE(predicted)` then exceeds the real V4 delta → `CurrencyNotSettled`.
- **The V3 hops** (deep USDC/USDT + WETH/USDT pools) stay within one range
  (or cross exactly one boundary the 2-range model handles) → `predicted ==
  actual` exactly.

## How the finding was reached

### Step 1 — the revert-tolerant swap capture (refutes stale state)

The `SwapEventCaptureInspector` preserves reverted-frame swaps into a separate
`reverted_swaps` buffer. For each failed path, the simulator logs each
reverted swap's ACTUAL output vs the solver's predicted `hop_outputs[i]`
(env-gated `DEGENBOT_SIM_LOG_REVERTED_SWAPS=1`).

The decisive fixture (block 25635461, `DEGENBOT_SIM_EXIT_ON_FAIL=1` trap):

| path | V4 hop 0: actual vs predicted | V3 hop 1 | V3 hop 2 |
|------|------------------------------|---------|---------|
| 97 | 25885 vs 25898 (off by 13) ❌ | 25920 vs 25920 ✅ exact | 13596432288007 vs 13596432288007 ✅ exact |
| 200 | 715 vs 716 (off by 1) ❌ | 188620083055122 ✅ exact | 378427095065 ✅ exact |
| 98 | 29372 vs 29387 (off by 15) ❌ | 29413 ✅ exact | (knock-on) |
| 173 | 11998 vs 12004 (off by 6) ❌ | 11992416144107937 ✅ exact | 6291521325144 ✅ exact |

Every V4 hop mismatched (1-15 units, always `actual < predicted`); V3 hops
that completed within their own range matched exactly. This refutes stale
state (V3 would diverge too) AND refutes the sim's `v4_simulate_swap` (the sim
captures the CORRECT on-chain amount — it's the SOLVER that's wrong).

### Step 2 — tracing the solver path (refutes swap-step rounding)

The prior version of this doc localized the bug to `v4_simulate_swap` /
`compute_swap_step_v4`. **That localization was wrong**: the solver does NOT
call `v4_simulate_swap` to compute `hop_outputs`. The call chain is:

```
solver: exact_mobius_solve / int_simulate_path
  → IntHopState::swap (V2 getAmountOut) for V2 hops
  → int_simulate_v3_swap (single-range CL) for V3/V4 hops
    — computes ONE sqrtPriceNext within ONE tick range's liquidity
    — the optional IntTickRangeCrossing chains base_range → ending_range (ONE crossing only)
```

`int_simulate_v3_swap` (`mobius_v3_int.rs:596`) computes the swap within a
single `IntV3TickRangeHop` (one `liquidity`, one `sqrt_price_x96`). The
multi-hop simulator (`mobius_v3_int.rs:72`) chains an optional
`IntTickRangeCrossing` per hop — but this handles at most ONE boundary
crossing (base → ending). A V4 swap crossing 2+ tick boundaries (changing
liquidity each time) is approximated by the 2-range model → over-estimates the
output.

V3 hops in these paths cross 0 or 1 boundaries → the 2-range model is exact →
they match. The V4 hop (USDC/WETH 0.3%, thinner liquidity) crosses 2+ →
over-estimated.

## Implications

1. **Path A (extend engine state) is NOT the fix** — refuted (V3 hops match exactly).
2. **`compute_swap_step_v4` / `v4_simulate_swap` is NOT the bug** — the sim runs
   those and captures the correct on-chain amount. The per-step math is
   faithful (V3 uses the same `get_amount_delta` round-down and matches exactly).
3. **The fix is in the solver**: extend `int_simulate_v3_swap` / the
   `IntTickRangeCrossing` model from 2 ranges to N ranges (walk the full tick
   bitmap like `compute_swap_step` / `v4_simulate_swap`), OR round the V4
   hop-output DOWN to the nearest whole-tick-boundary output before the
   composer's `V4_TAKE`.

## Fix direction (ergo `W2UWZO`)

- Record a mainnet V4 swap fixture that crosses 2+ ticks (path=97, block
  25635461, the USDC/WETH V4 pool).
- Pin a RED test: `int_simulate_v3_swap(fixture_input, fixture_2range_hop)`
  returns 25898 (the over-prediction), while the on-chain actual is 25885.
- Extend the crossing model to walk N ranges (via `compute_tick_ranges` or by
  delegating the V4 hop to `v4_simulate_swap` for hop-output computation) until
  the test passes GREEN + the `CurrencyNotSettled` rate drops to ~0.

See `logs/debug/sim_trap.log` + `logs/debug/v4_fixture_block_25635461.md`
(gitignored) for the captured data.

---

## Addendum — refined localization (follow-up session)

A follow-up run with the aggressive defaults (0-bps filter + `SIM_EXIT_ON_FAIL`)
+ `DEGENBOT_SIM_LOG_REVERTED_SWAPS=1` captured a **fresh decisive fixture**
that REFUTES W2UWZO's "Localization so far" residual suspect (the V4 tick walk
in `v4_simulate_swap`) and pins the divergence purely on the SOLVER side:

```
path=1402 V2-V4-V3  CurrencyNotSettled  block 25635886
hop=0 V2  actual_out=160363438980  predicted=160363438980  matched=true   ✓
hop=1 V4  actual_out=100            predicted=101           matched=false ✗  (+1)
hop_outputs=[160363438980, 101, 52688562059]
```

Reproduced across 3 paths (1401/1402/1404) every block — V2 hop matches
**exactly**, V4 hop is the **sole diverger**, always `actual < predicted`
(off by 1 here; 1–15 in the prior corpus).

### What this establishes

1. **The sim's `actual` IS ground truth.** The in-process sim runs the REAL
   V4 PoolManager bytecode in revm against RPC storage at the solve block
   (`sim/evm/mod.rs` — EVM transact → CacheDB → `BotStateDb` →
   `WrapDatabaseAsync<AlloyDB>`). The inspector captures the `Swap` event
   the real bytecode emitted. So `actual=100` is what mainnet would produce.
   W2UWZO's hypothesis that `v4_simulate_swap`'s `gen_ticks` tick walk is the
   bug is therefore a RED HERRING for `CurrencyNotSettled`: `v4_simulate_swap`
   is NOT in the execution path that produces `actual` — only the solver's
   `int_simulate_v3_swap` / `compute_crossing` (the 2-range-to-N-range CL
   model) produces the over-predicted `predicted=101`.
2. **Not a sparse-miss.** V4 pools load from a DB snapshot (bot logs
   `Loaded Uniswap V4 LP snapshot from db source`) → `coverage == Tracked`
   (complete `tick_data`). `gen_ticks` sees all initialized ticks. So the
   `known_bitmap_words` sparse-miss guard (which `v4_simulate_swap` has but
   `compute_tick_ranges` lacks) is NOT the cause for Tracked pools.
3. **The composer hardcodes `hop_outputs`.** `degenbot_executor::composers`
   bakes the solver's predicted `hop_outputs[i]` into the chained
   `V4_TAKE` / next-hop `amountSpecified` as a fixed bytecode amount — it
   cannot read the runtime `BalanceDelta`. So a +1 over-prediction on the V4
   hop directly overdrafts settlement → `CurrencyNotSettled`.

### Residual suspect (narrowed)

With sparse-miss ruled out (Tracked) and `v4_simulate_swap` exonerated, the V4
hop over-prediction is one of:

- **Stale active state** — the V4 pool's `sqrtPriceX96`/`liquidity`/`tick` at
  solve time lags the solve-block RPC state the revm sim reads. Thin-liquidity
  0.8%/1% V4 pools are price-sensitive to 1-block drift; deep V3 pools are not
  → explains why V3 matches exactly while V4 diverges. (V3/V4 swaps +
  Mint/Burn ARE pumped via `on_v4_swap`/`on_v4_liquidity_update` in
  `log_dispatcher`, so this would be a pump-ORDERING gap: solve running before
  block-N's events are applied, or the snapshot lagging.)
- **A residual in `compute_crossing` / `int_simulate_v3_swap`'s hand-rolled
  per-range formula** diverging from `compute_swap_step_v4`'s steppiwise
  amount-accumulation on a multi-tick crossing. (The single-range partial-step
  formula was verified field-by-field to match on-chain `get_amount1_delta`
  floor + `get_next_sqrt_price_from_amount0_rounding_up` ceil, so a SINGLE-range
  single-step cannot diverge — the +1 REQUIRES a multi-range crossing or stale
  state.)

### Decisive next experiment

Dump, for ONE failing V4 hop, BOTH (a) the engine's `V4PoolState`
(`sqrtPriceX96`, `liquidity`, `tick`, `tick_data` keys, `coverage`) at solve
firing AND (b) the on-chain V4 state at the solve block via `cast` (archive RPC
`ETHEREUM_ARCHIVE_NODE_HTTP_URI` is available). If they DIFFER → stale active
state (fix: pump ordering / refresh V4 active state to solve block before
solving). If they're IDENTICAL but `int_simulate_v3_swap` still returns 101
while `v4_simulate_swap` returns 100 → the residual is in
`compute_crossing`/`int_simulate_v3_swap` (fix: per CR #2 — delegate the V4
hop-output to `v4_simulate_swap` in the solver path).

---

## RESOLUTION — hypothesis 2 REFUTED; residual is stale active state (ergo `W2UWZO`)

The "Decisive next experiment" above proposed dumping the engine's
`V4PoolState` vs `cast`-ing the on-chain state at the solve block. A
**strictly stronger** experiment was run instead — one that isolates the
solver math from state freshness entirely: feed the **identical** synthetic
`V4PoolState` to BOTH `v4_simulate_swap` (the byte-exact full-tick-walk the
revm sim's `actual` mirrors) AND the solver's crossing path
(`IntV3TickRangeSequence::compute_crossing` + `int_simulate_v3_swap` for the
ending partial step — exactly what `int_simulate_mixed_path_n` assembles for a
CL hop's `hop_outputs[i]`). Because the input state is byte-identical, ANY
divergence is pure solver math; conversely, byte-exact parity proves the
observed `+1` CANNOT originate in the solver math and must be stale state.

**Result: byte-exact parity across the sweep.** The test
(`rust/crates/degenbot-solvers/tests/v4_crossing_solver_vs_sim_parity.rs`)
sweeps:

- liquidity `L` from 1e13 to 1e21 (the regime where the zfo partial-step
  round-up previously surfaced);
- both swap directions (zfo + ofz);
- multi-tick crossings (5 initialized ticks → up to 4 boundary crossings);
- amounts landing in every range interior AND boundary-adjacent / dust amounts
  (`gin(k) ± δ` for δ ∈ {1, 2, 3, 7, 13}) — the rounding edge regime where a
  `+1` residual would live.

`v4_simulate_swap == solver_crossing_output` for every case. This REFUTES
hypothesis 2: there is no residual rounding bug in `compute_crossing` /
`int_simulate_v3_swap` for identical state — the round-up fixes (commits
`1cb8c929` + `d2de7ab5`) closed the multi-range rounding gap completely.

**Conclusion: the residual `+1` V4 hop over-prediction is stale active
state (hypothesis 1).** The engine's `V4PoolState` scalars/tick_data at solve
firing differ from the solve-block RPC state the revm sim reads — a
pump-ORDERING gap (solve running before block-N's V4 swap/Mint/Burn events are
applied, or the snapshot seed lagging). This is consistent with every
empirical signature: V2 + V3 hops match exactly (deep pools, price-insensitive
to 1-block drift); only the thin-liquidity V4 hop diverges by 1 wei. The fix is
NOT in the solver math — it is in V4 active-state freshness at solve time.

The remaining dump-vs-`cast` experiment is now only a *confirmation* step
(capture the exact scalar/tick delta for one failing hop to pin the pump-order
root cause), not a fork-resolver — the fork is already resolved.
