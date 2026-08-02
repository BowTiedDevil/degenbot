# Exploration: Live Backrun-Bot Debug Session

Session tracking doc for driving the backrun bot live, observing failures, and
debugging them. This doc is appended to as the session progresses.

## Session goal

Continue the `no-profit` / V3→V4→V3 executor-Halt investigation (2LTKVO /
`docs/exploration-no-profit-crash.md`) by running the bot against live
mainnet, capturing failures with the new instrumentation, and root-causing
each class. The two current live-debug enablers:

- **Full call-trace dump** (`DEGENBOT_DUMP_CALL_TRACE=1` in `run_bot.sh`,
  commit `a6937131`): on a sim failure, `CallTrace::render_debug()` prints the
  whole nested revm call sequence (execute → v3c.swap → callback → v3a.swap →
  callback → V4_UNLOCK → unlockCallback → swap → …) so a depth-8 empty-calldata
  PoolManager `Halt` can be attributed against the executor Vyper source.
- **FSM WS recovery** (commit `ecf8518e`): the false "unreliable WS" shutdown
  on stall-backfill should no longer recur.

## Session log

### 2026-08-02T23:40Z — AV42C7 fixed + bot re-operating (no wedge)
- The AV42C7 gate's `panic!` was converted to a CLEAN shutdown (`shutdown`
  store + return) so `run_with_stream` exits at the loop top instead of
  unwinding the pump task and wedging the bot (commit `6834e132`). Detection
  logic unchanged — it still catches a >3-block-stale CL pool via the solve-
  block re-check; only the response is now fail-safe-clean.
- Added `av42c7_gate_sets_shutdown_cleanly_instead_of_panicking` (mocked
  getReserves mismatch -> gate sets shutdown, returns, no panic); FakeDrainSink
  gained configurable `path_refs`. 318 tests pass, clippy clean.
- Restarted bot (pid 11468): advanced 25670441 → 25670552+, alive, **0 AV42C7
  panics / 0 wedges / 0 WS shutdowns**, 4 header-stall→backfill recoveries
  (FSM recovery handling the slow-but-connected WS), CPU ~115% (solving). The
  bot now operates reliably through WS slack instead of wedging.

### 2026-08-02T23:19Z — AV42C7 gate PANIC wedged the pump (pre-fix)

- `DEGENBOT_ASSERT_SOLVER_STATE` gate panicked at solve block 25670441,
  path_idx=20: hop2 V3 `0xaCDb27b2…` solver snapshot
  `(sqrt=3435805256447190374086530, liq=13454508496170479, tick=-200927)`
  no longer matches on-chain at 25670441 (`sqrt=3437958426149627176209284`).
  The three hops were stale by 4–37 blocks (V3 by 37, V4 by 17, V3 by 4).
- Immediately preceded by `HEADER STALL`s of **56.7s and 62.5s** — the WS is
  slow/bursty, so pool state lags on-chain by many blocks. Contrary to the
  earlier clean-catch-up, the WS is NOT keeping pools fresh this run.
- **Secondary bug:** after the AV42C7 `panic!` in the tokio worker, the pump
  did NOT shut down cleanly — it burned CPU in a no-progress busy loop
  (`[gil-probe] main loop idle: no progress … since_progress=498091`) for ~8
  min. The panic wedged the pump instead of exiting (or restarting).
- Stopped the bot (pid 346) at 23:28Z. Left healthy otherwise.

**Theme:** the WS instability (60s header stalls) → pool states lag 4–37
blocks → solver over-predicts on stale sqrt (the IIA cluster) AND trips the
AV42C7 gate. This is distinct from the FSM disconnect recovery: it's a
SLOW-but-connected WS causing lag, not a drop. The overcome is either faster
state refresh (apply missed swaps) or treating >N-block staleness as a
recovery trigger (not just a panic).

### 2026-08-02T23:18Z — REPRODUCIBLE V3-→V4→V3 failure: 1-wei V4 rounding → USDC overdraw (2LTKVO family)

- `[sim-fail] bucket=ERC20: transfer amount exceeds balance` on 2× **V3-V4-V3**
  paths (10338, 10234), depth=9, target=USDC `0xa0b86991…`, `swaps_before=0`.
- Path 10338 solver-st: `[V3 fee=30 zfo=true; V4 fee=1 zfo=false; V3 fee=25
  zfo=true]`, V4 `sq=79231869042278935382727675145, liq=94294142` (a fee-1
  pool, tiny amounts).
- `[sim-revert-swap]` hop=1 (the V4 hop, emitter=PoolManager):
  `actual_out=9585 predicted=9586 matched=false` — the on-chain PoolManager
  swap yields **1 wei LESS** than `v4_simulate_swap`.
- **Mechanism:** `three_hop_v3_v4_v3` (`composers.rs`) emits
  `enc_v4_take_compact(…, out_b)` where `out_b = hop_outputs[1]` = the
  PREDICTED amount. When V4 actual < predicted by 1 wei, the executor takes
  9586 USDC but only 9585 exists → `ERC20: transfer exceeds balance` → the
  whole executor path reverts.
- This is the same `V4_TAKE_COMPACT(predicted)` choice that was made to
  satisfy v3c's IIA (vs `take_delta(actual)` which silently under-pays) — the
  take-EXACT-predicted approach now fails when the solver's V4 math is off by
  1 unit. It is a genuine solver-vs-on-chain **V4 rounding divergence** at
  extreme-low-fee (fee=1) / tiny amounts.
- **Fix directions (both = 2LTKVO/tier-3 territory):** (a) make
  `v4_simulate_swap` byte-exact vs the canonical PoolManager (so
  predicted==actual and take_compact is safe), and/or (b) make the executor's
  V4 take amount = min(predicted, actual available) instead of EXACT predicted
  (a defensiveness floor, but it re-opens the v3c-IIA tradeoff — needs the
  path to be re-sized rather than the take floored).

### 2026-08-02T23:15Z — IIA sim-reject cluster on UNI/WETH-anchored paths

- ~11x `[sim-fail] bucket=IIA` (Invalid Input Amount, the canonical
  UniswapV3Pool error) on **V3-V2-V3** and **V3-V3-V3** paths, all with hop0 on
  V3 pool **`0x1d42064fc4beb5f8aaf85f4617ae8b3b5b8bd801`** (UNI/WETH, `tick`and
  fee hidden but `fee()`=3000 — matches solver `fee=30`), at block 25670416.
- `[sim-revert-swap]` diagnostic (the TR6GWT seam) shows hop0 `actual_out`
  (the sim on-chain Swap) is a **constant ~2.727 bps BELOW `predicted`**
  across every amount (computed exactly 2.727 bps on both path 3535 and 3473) —
  a constant fraction ⇒ fee/state effect, not tick-boundary rounding.
- On-chain slot0 @25670416: `sqrtPriceX96=3745828247728485008658659812`,
  `tick=-61037`, `cardinality=300`, **`feeProtocol=0x66` (0b01100110)**.
  Solver cached hop0 `sq=3745318163155369097190507590` (≈0.014% LOWER),
  `liq=74266365662455562952177`.

**Classification from the sim `[sim-revert-swap]` (matched=false):** the sim
runs the real UniswapV3Pool bytecode and compares its Swap output to the
solver's prediction. Whether the divergence is (a) stale cached state vs the
live chain, or (b) a CL-math/fee discrepancy on the SAME state, the net is the
solver OVER-predicts this pool's output, so UNI/WETH-anchored paths are
mis-routed and correctly rejected as IIA (safe — no bad submission — but a
missed-opportunity / mis-routing class). Discriminator is open: check how
`simulate_path_on_evm` seeds the V3 pool storage (solver cache vs fresh
on-chain slot0 via the state-overrides).

Note: the pool's `feeProtocol=0x66` is set; in standard uniswap-v3 the protocol
fee only splits the fee LP↔protocol and does not change swapper output, so
it is likely a red herring for the output shortfall, but worth confirming the
pool is not a non-standard fork whose `swap` taxes input.

### 2026-08-02T23:10Z — Session start, clean catch-up

- Bot `eth_backrun_v2_v3_v4_rust.py` started (pid 346), executor injected
  (15605 bytes from `contracts/cmd_executor_runtime_bytecode.txt`), DB snapshot
  at block 25670298 (newer than the earlier 25664576).
- Caught up to live head cleanly (~25670404). Through ~22 blocks: **0 sim
  failures, 0 profitable**; no `unreliable WS` / `PanicLateForward` shutdown
  (the FSM recovery fix held — no false restart).
- The target UNI/MATIC V4 pool `0x929b9b09` (the V3→V4→V3 Halt trigger) has
  NOT traded in a qualifying path yet — no `sim-fail`/`sim-trace` fired.
  The `debug-v4-solve` churn is a different popular V4 pool (`430db0…`).
- Status: HEALTHY, waiting on the rare UNI/MATIC trigger.

