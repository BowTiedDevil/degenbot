# Sim root cause: V4 swap-step rounding (NOT stale state)

> Decisive root-cause finding for the `CurrencyNotSettled` failures on V4-V3-V3
> (and V2-V4-V3 / V2-V4-V2). This **overturns the premise of epic `TR6GWT**
> ("stale-state elimination") and the path-A decision** (`2BUNU2`): the failures
> are NOT caused by stale engine state, and the fix is NOT extending the engine
> to carry `feeGrowthGlobal`/`tickBitmap`/observations.

## TL;DR

The `CurrencyNotSettled` revert is a **V4 swap-step rounding divergence in the
solver's calc**, not stale state:

- **V3 hops match the solver's `hop_outputs` EXACTLY** (byte-for-identical on
  every failing candidate). This decisively refutes stale engine state — if
  state were stale or the tick walk diverged, the V3 hops would diverge too.
  The engine state is correct AND the V3 swap math (`compute_swap_step_v3`) is
  a faithful port.
- **Only the V4 hop (hop 0) diverges, by a tiny rounding amount (1–8 units),
  ALWAYS `actual < predicted`**. The composer does
  `V4_TAKE(out_a = predicted)`, but the V4 swap produced `actual < predicted`
  → the take exceeds the available PoolManager delta → `CurrencyNotSettled`.

The fix: align the solver's `compute_swap_step_v4` / `v4_simulate_swap` rounding
with the on-chain V4 `SwapMath` (the solver over-predicts the V4 output by a
small amount). This is a **solver calc** task, NOT a sim-serving / engine-state
task.

## How the finding was reached — the revert-tolerant capture

The prior divergence probe (`4C33DP`, `sim_divergence_probe.md`) compared the
engine's *tracked scalar slots* vs the RPC and found 0 divergence — but that
proved nothing about the *swap amounts*, because `feeGrowthGlobal` (the main
untracked slot) does NOT affect swap amounts (the on-chain swap callback
WRITES fee-growth, doesn't read it for the amount calc). And the inspector's
`captured_swaps` was empty for every `CurrencyNotSettled` failure (the whole
`unlock` reverts → revm pops the inner swaps from the journal → the
`SwapEventCaptureInspector` dropped them) — the "captured_swaps blind spot".

The decisive instrument: a **revert-tolerant swap capture**. The
`SwapEventCaptureInspector` now preserves reverted-frame swaps into a SEPARATE
`reverted_swaps` buffer (committed set unchanged → no regression), drained via
`take_reverted_swaps()`. The simulator, for a failed path, logs each reverted
swap's ACTUAL output vs the solver's predicted `hop_outputs[i]` (env-gated
`DEGENBOT_SIM_LOG_REVERTED_SWAPS=1`).

A subtle sign-convention fix was required to read the actual outputs: the V3
`Swap` event's `amount0`/`amount1` are **pool-perspective** (positive = pool
received = INPUT; negative = pool paid = OUTPUT), while V2/V4 are
**swapper-perspective** (positive = received = OUTPUT). The output selector
picks the negative side for V3, positive for V2/V4.

## The data (V4-V3-V3 mainnet, `DEGENBOT_SIM_LOG_REVERTED_SWAPS=1`)

8 candidates across two runs, all `CurrencyNotSettled`, all `age:0` (no block
skew). Representative:

| path | V4 hop 0: actual vs predicted | V3 hop 1 | V3 hop 2 |
|------|------------------------------|---------|---------|
| 168 | 16742 vs 16750 (off by 8) ❌ | 1994520424333918 ✅ exact | 8785548137144 ✅ exact |
| 200 | 1032 vs 1033 (off by 1) ❌ | 270979048790253 ✅ exact | 548887430495 ✅ exact |
| 195 | 997 vs 998 (off by 1) ❌ | 262099028747223 ✅ exact | 530344466395 ✅ exact |
| 198 | 1042 vs 1043 (off by 1) ❌ | 273589079534020 ✅ exact | 554344285954 ✅ exact |

Every V4 hop mismatched (by 1–8 units, always `actual < predicted`); no V3 hop
EVER mismatched (`hop=[12] matched=false` grep returns empty across all
candidates).

## Implications

1. **Path A (extend engine state for feeGrowth/bitmap/observations) is the
   WRONG fix.** The V3 hops matching exactly proves the engine state IS the
   on-chain state at sim time; serving more state would change nothing for the
   V3 hops. And `feeGrowth` doesn't affect swap amounts anyway. The serving
   seam (`NQ3FPV`) is still a valuable mechanism, but enabling it would NOT fix
   these failures.

2. **The V3 `compute_swap_step_v3` port is byte-faithful** (the V3 hops match
   the on-chain V3 swap exactly on every candidate). The solver's CL math is
   correct for V3.

3. **The V4 `compute_swap_step_v4` / `v4_simulate_swap` has a rounding
   divergence** vs the on-chain V4 PoolManager `SwapMath`. The solver
   OVER-predicts the V4 output by a small amount (1 unit per step; 8 units for
   path 168 suggests ~8 swap steps / tick crossings). The direction (always
   `actual < predicted`) points at a specific rounding-direction mismatch —
   likely the on-chain V4 math floors the output down while the solver rounds
   up (or doesn't apply the same `ceil`/`floor` on a per-step boundary). This
   is the precise defect to locate + align.

## Localization — the V4 loop accounting MATCHES on-chain (divergence is in the walk)

A line-level diff of the solver's `v4_simulate_swap`
(`rust/crates/degenbot-pools/src/v4_state.rs`) against the on-chain V4
`PoolManager.sol` `swap()` shows the loop accounting + final delta assembly are
**byte-faithful** (the self-documenting comment's "~1-wei over-count" fix did
land correctly):

- exact-in: `amountSpecifiedRemaining += amountIn + feeAmount; amountCalculated += amountOut` — matches `PoolManager.sol:895-897`.
- exact-out: `amountSpecifiedRemaining -= amountOut; amountCalculated -= (amountIn + feeAmount)` — matches `PoolManager.sol:889-891`.
- final `(amount0, amount1)` assembly via `zeroForOne == exactInput` matches `PoolManager.sol:971-975`.

And `compute_swap_step_v4`'s `amount_out` is computed by the SAME
`get_amount*_delta(..., Some(false))` (round-down) call as `compute_swap_step_v3`
— which matches on-chain V3 EXACTLY on every V3 hop. So the per-step math is
faithful.

This narrows the residual to:
1. **The V4 tick walk** — the solver's `gen_ticks` (`state.tick_data` +
   `known_bitmap_words`, sparse) vs the on-chain `tickBitmap` walk. A sparse
   miss the `MissingTickWord` detection doesn't catch would cross a different
   tick set → wrong `liquidity` for part of the swap → off-by-small. The
   "Slice-4 fix" `else`-branch comment in `v4_simulate_swap` records that this
   gap HAS existed before.
2. **A V4-pool-specific unprobed state slot** — the divergence probe covered
   V4 `slot0`/`liquidity`/per-tick gross+net (all matched), but NOT the V4
   `tickBitmap` word VALUES (the engine carries only the `known_bitmap_words`
   key-presence set, NOT the bitmap values) NOR `feeGrowthGlobal`. Since
   `feeGrowth` doesn't affect amounts, the bitmap/walk is the prime suspect.

The off-by-1-per-step + off-by-8-for-multi-step signature is consistent with
a tiny per-step liquidity divergence (a near-zero-`liquidityNet` tick missed,
OR a boundary rounding in `gen_ticks`'s `<=`/`<` tick selection) rather than a
structural math break. The definitive localization (a recorded mainnet V4
swap fixture: pool state + tick data + resulting amounts, asserted byte-exact
by the solver) is the next TDD task.


## Fix direction (a NEW epic / task, not TR6GWT path A)


See `logs/debug/revert_swap_c.log` for the captured `[sim-revert-swap]` lines
that established this finding.
