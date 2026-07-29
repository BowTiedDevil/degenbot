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
