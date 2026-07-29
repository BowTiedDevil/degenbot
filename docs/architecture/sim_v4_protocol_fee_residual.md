# V4 `CurrencyNotSettled` residual after the protocol-fee fix (BZBOLL)

**Status:** protocol-fee fix landed + unit-proven, but **insufficient** — a
~59% `CurrencyNotSettled` residual remains on mainnet V4-V3-V3.

## What BZBOLL fixed (verified)

- Ported V4 `calculateSwapFee(protocolFee, lpFee)` + the per-direction
  extractors (`protocol_fee_zero_for_one` = `packed & 0xFFF`,
  `protocol_fee_one_for_zero` = `packed >> 12`) into `degenbot-cl-math`.
- Threaded a packed `protocol_fee` uint24 onto `V4PoolState` +
  `RegisterV4PoolParams`; set from the on-chain `getSlot0` decode at
  registration (builder passes `protocol_fee=int(protocol_fee_raw)`).
- `v4_simulate_swap` computes `swap_fee = calculate_swap_fee(dir_fee, lp_fee)`
  internally; the solver's crossing path (`build_int_v4_sequence`) picks the
  same `swap_fee` up via `gamma_numer`.
- Unit test `calculate_swap_fee_path97_fixture_pinned` reproduces the on-chain
  actual (`25885`, was `25898` pre-fix) for the recorded path-97 fixture.
- The dev `.so` (built 2026-07-29T17:51Z) confirmed via `strings`/`nm` to
  contain `calculate_swap_fee`, `protocol_fee_zero_for_one`,
  `protocol_fee_one_for_zero`, `direction_protocol_fee` — i.e. the fix IS
  live in the running build (not a stale-build artifact).

## What mainnet validation (TR6GWT / 4ZIM72) revealed

Soak run `logs/debug/sim_divergence_V4-V3-V3_postfix_soak.log`
(permutation `V4-V3-V3`, `DEGENBOT_MIN_PROFIT_MARGIN_BPS=0`,
`DEGENBOT_SIM_EXIT_IGNORE_BUCKETS=CurrencyNotSettled,IIA`):

| metric | value |
|---|---|
| blocks covered | ~9 (25640145 → 25640163) |
| total candidates | 59 |
| ok (below threshold) | 14 |
| failed — `CurrencyNotSettled` | 35 (**59%**) |
| failed — `IIA` | 10 |

The pre-fix rate was ~85% (per epic context, ~237/246 ≈ 96% historical). The
fix reduced the rate but did **not** eliminate it.

## Residual fingerprint (all 35 `CurrencyNotSettled` failures)

```
bucket         = CurrencyNotSettled
revert selector = 0x5212cba1  (CurrencyNotSettled())
target          = 0x000000000004444c5dc75cB358380D2e3dE08A90  (V4 PoolManager)
swaps_before    = 0
captured_swaps  = []   (no Swap event captured — unlock reverted)
```

This is a **distinct subclass** from the one BZBOLL targeted. BZBOLL targeted
the case where the swap executes and the settlement `delta` check fails because
the solver's predicted amount (using `lp_fee` alone) exceeded the actual output
(those would have non-empty `captured_swaps`). This residual reverts at the
**PoolManager unlock settlement**, with `captured_swaps=[]` — the swap ran but
the unlock reverted, so no `Swap` event was emitted.

## What was ruled out

1. **Stale `protocol_fee` in the engine (no `setProtocolFee` pump handling).**
   Ruled out: the failing pool `0x4f88...24a7` (USDC/WETH fee=500) has on-chain
   `getSlot0` `protocolFee = 512125` (non-zero), and emitted **zero**
   `setProtocolFee` events in the backfill range (591 ModifyLiquidity + 10 Swap,
   no protocol-fee-change topic). The engine's value, set at registration from
   the current slot0, matches on-chain.
2. **Engine holds `protocol_fee = 0` (threading broken).** Ruled out: the
   builder passes `protocol_fee=int(protocol_fee_raw)` (line 312), the PyO3
   binding threads it into `RegisterV4PoolParams.protocol_fee`, and
   `BotState::register_v4_pool` constructs `V4PoolState::from_params(params, …)`
   which sets `state.protocol_fee = params.protocol_fee`. No production path
   clobbers it (only the `simulate_swap_with_override` verify-seam, reorg, and
   divergence-probe paths hardcode `protocol_fee: 0`).
3. **Decoding mismatch.** Ruled out: my `protocol_fee_zero_for_one`/`_one_for_zero`
   match the builder's `V4Slot0Data` decoding (`& 0xFFF` / `>> 12`).
4. **Fix not compiled into the running bot.** Ruled out: `.so` contains the
   new symbols.

## Remaining hypotheses (for the follow-up to localize)

- **(b) Solver CL-math divergence from the real PoolManager.** The solver's
  `compute_swap_step` / crossing math may diverge from V4's stepwise
  `computeSwapStep` walk on a multi-tick crossing by a rounding `±1`, OR V4
  applies the protocol fee at a step the solver's `gamma_numer` doesn't
  capture exactly. This is the W2UWZO residual, now re-surfacing on paths where
  the protocol-fee correction shifted but did not eliminate the delta.
- **(d) Composer / encoding.** The executor's V4 swap command stream may
  encode an expected amount that doesn't match the solver's prediction
  (rounding/setting direction), surfacing as the unlock settlement failure.

## The localization the follow-up must perform

For failing path 884 (block 25640190, pool `0x4f88...24a7`):

- solver `optimal_input = 22297725057800597`
- solver `hop_outputs[0]` (V4 USDC→WETH) = `42773449`

Actions:
1. Capture the V4 pool `slot0` + `liquidity` + `tickBitmap` + `ticks(tick)`
   storage slots at block 25640190 via `cast` (StateView
   `0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227`, NOT the PoolManager —
   `getSlot0` lives on the StateView, the PoolManager reverts for it).
2. Re-derive the on-chain actual via `cast call` to the PoolManager `swap`
   with the solver's exact input; compare to `42773449`.
3. If actual ≠ `42773449`, the gap is solver CL-math (b) → pin a RED
   byte-exact `v4_simulate_swap` test.
4. If actual == `42773449`, the gap is composer/encoding (d) → the executor's
   command stream diverges from the solver's prediction.

## Artifacts

- `logs/debug/sim_divergence_V4-V3-V3_postfix.log` — first short run (28
  candidates, 0 CurrencyNotSettled, trapped on IIA at block 25640133).
- `logs/debug/sim_divergence_V4-V3-V3_postfix_soak.log` — the soak (59
  candidates, 35 CurrencyNotSettled, 10 IIA).
- `logs/debug/sim_V4-V3-V3_cns_fixture.log` — trap-on-failure fixture with
  the V4 pool_id (path 884).
