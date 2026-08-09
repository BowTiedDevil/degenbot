# Live-run findings — solver-state desync instrumentation (2026-08-08, dev run)

Live empirical run to pinpoint the real solver-state failure class, per the
low-MTBF dry-run directive. Ran the actual bot off a fuzz-forked local **live
mainnet** node (`ws://host.containers.internal:8546`; block 25713668 had 181 txs /
24M gas / real baseFee — real activity, not synthetic).

## Run configuration
- `examples/eth_backrun_v2_v3_v4_rust.py`, **dry-run** (no `--live`), scoped to
  the incident topology with `--permutation V3-V4-V3`.
- `DEGENBOT_SOLVER_STALENESS_BLOCKS=0` (tightest; trie exact divergence).
- Instrumentation in place: generalized `[solver-state]` lag reporter (OC34VZ) +
  UO3JM4 abort. Log: `logs/dev/dev_run2.log` (~101 MB).
- Duration ~9 min, blocks 25713600 → 25713670.

## Results
- **17,814** `[solver-state]` reporter fires across **93 distinct** Tracked V3/V4
  pools, `stale_by` up to **30** blocks (e.g. `0x8ad599c3` USDC/WETH at
  `update_block=25713621`, solve block 25713651, stale_by=30).
- **0** genuine divergences (**0** aborts / `verified desync`).

## Why so many fires but zero divergences (the key finding)
On-chain cross-check (corrected decoding — the raw tick word is real; USDC/WETH
shows tick 200722 ✓):
- Pools flagged by the reporter were **quiet in their lag window**: on-chain
  `sqrtPriceX96` was byte-identical at the pool's `update_block` and at the solve
  block (e.g. USDC/WETH `0x8ad599c3`: identical sqrt at 25713621 and 25713651;
  moved only by 25713663).
- A pool with no qualifying swap for N blocks keeps an old `update_block` tag AND
  on-chain is also unchanged → **state honest → correctly NOT a desync, correctly
  NOT aborted**.

**Conclusion: the lag-only reporter is a NOISE signal for failure-hunting.** It
fires on any pool whose `update_block` trails the anchor past the threshold,
regardless of actual divergence. ~93 real liquid pools routinely show 1–30 block
tag lag here (they just didn't swap in those windows). Genuine solver-state
desyncs — where the bot MISSES/DELAYS a swap and on-chain moves, so the stored
price differs (the original `0fb0e40` case) — are what the **abort** catches, and
they did **not** reproduce in this window even though `0fb0e40` itself participated
and was among the lagging pools.

## What this tells us
1. The `update_block` tag lag is **expected and largely benign** (quiet pools).
   Using it alone to "find the failure" yields 93 false positives, not the 1 real
   divergence.
2. Real divergences are **rare** and require the missing-swap condition (pool that
   moves on-chain but whose state the pump failed to advance). This is the ACTUAL
   issue class from the original incident (UO3JM4).
3. The lag reporter cannot localize it; only on-chain divergence can.

## Recommended next instrument (low-MTBF pinpointing of the real desync)
Add an **opt-in, non-aborting divergence scanner** (env `DEGENBOT_SOLVER_DIVERGENCE_SCAN=1`,
default off → the UO3JM4 abort is fully preserved). For lagging Tracked CL hops it
performs the fresh solve-block on-chain read and logs `[solver-state] REAL-DESYNC
HONEST-vs-DIVERGENT` **without aborting**, so one long dry-run accumulates every
genuine divergence (any pool, any block) instead of dying on the first. This is the
instrument that makes the rare real failure observable fast, while leaving the
production abort semantics untouched.

## Validation
- Reporter + threshold knob verified end-to-end via a real launch.
- On-chain reads validated against a known pool (USDC/WETH realistic tick).

## Open decision (formerly GEKZ25)
With divergence confirmed rare, implement the non-aborting divergence scanner and
run a longer window to collect genuine desyncs → then fix the missed-swap path
(option (c)-style: advance/refresh a diverging participating pool before solving,
keeping the abort as backstop).

## Addendum: divergence scanner implemented + live-run (run4)

Implemented the opt-in **non-aborting divergence scanner** (`DEGENBOT_SOLVER_DIVERGENCE_SCAN=1`,
default off, UO3JM4 abort fully preserved):
- `solver_state_verifier.rs`: `is_lagging_tracked` shared predicate, `lagging_tracked_hops`
  refactored onto it, `divergence_scan_enabled()`, `DivergenceVerdict {Honest,Divergent}`,
  `scan_lagging_hops_for_divergence()` (reads on-chain at the solve block, classifies, never aborts).
- `block_pump.rs`: when scan mode is on, dedupe unique lagging Tracked CL hops and run the scanner
  instead of the aborting gate; log per-hop (DEBUG HONEST / ERROR DIVERGENT) + an INFO per-block summary.
- TDD: 3 new tests (predicate agrees with reporter; scanner classifies honest vs divergent via mock
  provider; skips non-lagging/V2). `cargo test -p degenbot-bot --lib` → **423 passed**.

Live run (dry-run, `--permutation V3-V4-V3`, `STALENESS=0`, `DIVERGENCE_SCAN=1`):
- Verified on-process env; scanner active.
- Over ~15 min / blocks 25713836→25713901, scanned **9,710 lagging Tracked pool-instances**:
  **9,710 HONEST, 0 DIVERGENT, 0 REAL-DESYNC**.

Conclusion (now empirically airtight): the pervasive `update_block` lag is benign — the bot's
stored scalar matches on-chain every time it lags. A genuine solver-state desync (missed-swap
divergence) is a RARE, discrete event that the scanner will surface as `REAL-DESYNC` when it
occurs. The scanner now runs a long dry-run window accumulating those rare events.

## Addendum: divergence scanner implemented + live-run (run4)

Implemented the opt-in non-aborting divergence scanner (DEGENBOT_SOLVER_DIVERGENCE_SCAN=1, default off, UO3JM4 abort preserved):
- solver_state_verifier.rs: is_lagging_tracked shared predicate, lagging_tracked_hops refactored onto it, divergence_scan_enabled(), DivergenceVerdict{Honest,Divergent}, scan_lagging_hops_for_divergence() (reads on-chain at solve block, classifies, never aborts).
- block_pump.rs: scan branch dedupes unique lagging Tracked CL hops and runs scanner instead of aborting gate; logs per-hop + INFO per-block summary.
- TDD: 3 new tests (predicate agrees with reporter; scanner classifies honest/divergent via mock; skips non-lagging/V2). cargo test -p degenbot-bot --lib -> 423 passed.

Live run (dry-run, --permutation V3-V4-V3, STALENESS=0, DIVERGENCE_SCAN=1): verified on-process env; scanner active. Over ~15 min / blocks 25713836->25713901, scanned 9,710 lagging Tracked instances: all HONEST, 0 DIVERGENT, 0 REAL-DESYNC.

Conclusion (airtight): pervasive update_block lag is benign (stored scalar matches on-chain every time). A genuine desync is a rare discrete missed-swap divergence the scanner surfaces as REAL-DESYNC when it occurs.

## Addendum 2: root cause of the sporadic liq-map verifier errors = a head-race (fixed)

The run-terminating `VerificationMismatchError` (V3 pool 0x841820... tick -87600) was NOT a true tick-map desync — the user's hypothesis was correct: the recurring liquidity-map verifier ran at the wrong block.

Race: `run_recurring_verify_until_done` (recurring_verify.py) took block numbers from the pump's header stream and called `verify_liquidity_maps(block_number=<raw head>)`. The header is accepted before the pump has applied all of that block's events to every pool's tick map, so the on-chain read (at the raw head) ran AHEAD of the engine's applied tick map -> sporadic false `liquidityGross` mismatch. The design (engine_registry.py) already documents that `block_number=None` resolves to `last_processed_block()` (the applied-state cutoff) for determinism; the recurring path bypassed that by passing the raw head.

Fix (src/degenbot/arbitrage/recurring_verify.py): pass `block_number=None` so verification anchors at the applied state (same block the engine's tick map reflects) -> race-free. Tests updated to assert the deferred anchor + preserved cadence (27 recurring/backrun tests... actually 37 tests pass).

## Addendum 3: removed the redundant Python recurring verify (Rust owns verification)

The recurring verify + its block-stream tee were REMOVED, per "we don't need a redundant Python verification for a pool that has already passed the two-step gate":
- Every pool passes the Rust two-step [verify-seed]@snapshot + [verify-drain]@backfill gate (degenbot-python/src/bot/pump.rs) at registration, before the loop.
- The startup whole-batch re-verify was ALREADY removed as redundant + racy (bot_runner.py).
- The recurring T7 whole-batch re-verify was the same redundant re-check of the already-verified pool set, and its raw-head anchor caused the sporadic false liquidityGross mismatches.

Changes:
- consume.py: removed `_tee_block_stream` + `_TEE_SENTINEL`; block_stream fed directly to the single consumer.
- bot_runner.py: removed the recurring-verify task, the tee fan-out, the `run_recurring_verify_until_done` shim/alias/import, and `RECURRING_VERIFY_INTERVAL` import.
- Deleted recurring_verify.py + test_recurring_verify.py; repurposed the two backrun_session tests (once-only block_stream now single-consumer; registration-climbs-concurrently).
- Left `driver_constants.RECURRING_VERIFY_INTERVAL = 50` as an orphaned constant.

Residual gap (documented, Rust-side): in-loop drift on pools that desync mid-run AND never participate in a solve is no longer covered by a Python whole-batch re-check; the Rust solve-time solver-state verifier already covers pools in solves. Any idle handling belongs in Rust, not a Python re-verify.

## Addendum 4: removed Python `next_base_fee` duplication (FFI-backed Rust math)

Audit of `src/degenbot/runner/` (backrun bot + BotRunner) for lingering Python duplication of Rust core found ONE genuine case: `next_base_fee` (EIP-1559) re-implemented in pure Python (`degenbot/calculations/evm_math.py`) while Rust owns the same math (`degenbot-evm-math/src/lib.rs:93`). The BotRunner computed `base_fee_next` with the Python function and piped it INTO the Rust `dispatch_profitable`/`dispatch_and_submit` seam — a driver co-implements-core inversion.

Everything else audited clean: dispatch/simulate/submit is a Rust FFI seam (Python chains + renders); build_paths is Python-companion orchestration (calls `build_pool`/`register_*`); tick-map assembly already offloaded to Rust; no Python swap/price/tick math; the example is a thin 124-line driver.

Fix:
- Added `degenbot._ffi.evm_math` submodule (Rust: `degenbot-python/src/evm_math/mod.rs`, `next_base_fee` pyfunction over `degenbot-evm-math`; registered in c_api.rs; dep+mod decl in Cargo/lib). Zero-target degenerate input raises clean `ZeroDivisionError` (mirrors the Python oracle, avoids a Rust PanicException leak).
- `consume.py` now calls `degenbot._ffi.evm_math.next_base_fee` instead of the pure-Python version.
- Pure-Python `next_base_fee` retained only as a library/parity/test util (still used by `v3_libraries/tick.py`'s `evm_divide` and tests).
- Verified byte-exact parity vs the Python oracle across realistic inputs + the ZeroDivisionError edge case.

## Addendum 5: silenced the "tracked CL hop staleness" WARN flood (benign settle lag)

Running the bot again surfaced ~4876 `[solver-state] Tracked Live CL hop trails...` WARNs in one run — 0 real desyncs, 0 aborts. Cause: the reporter's threshold `MAX_CL_STALENESS_BLOCKS = 3` sits BELOW the natural WS settle lag (~4-5 blocks), so every hop in the pipeline re-tripped it, and the reporter emitted one WARN per hop per path per block with NO dedup → ~143 WARNs/block on the same benign pools (34 blocks, 169 distinct pools, stale_by mostly 4-5).

Fix: extracted `aggregate_lagging_hops` (dedupe per pool, keep max stale_by) in solver_state_verifier.rs. block_pump now emits ONE summary WARN per block (n_pools, max_stale_by) and WARNs individually only for genuine outliers (`stale_by >= SOLVER_STATE_ABNORMAL_STALE_BLOCKS = 10`, well above baseline); the benign bulk stays at DEBUG. Safety gate (UO3JM4 abort) untouched.

Observed live: `block=25714797 n_pools=44 max_stale_by=4`, `block=25714798 n_pools=147 max_stale_by=5`, 0 outliers — flood gone, lag picture preserved. 2 new unit tests (`aggregate_lagging_hops_dedupes_by_pool_keeps_max_stale`, `_keys_by_pool_and_filters_non_lagging`); 21 solver-state verifier tests pass; clippy clean.

## Addendum 6: genuine desync — systemic post-backfill pool-state drain freeze (ergo 3YA7ZJ)

The bot stopped on a GENUINE solver-state desync (UO3JM4 abort) at solve block 25714809; this is the gate doing its job. Root cause characterized on-chain + from logs:

**The failure:** V3 pool 0x99ac8cA7087fA4A2A1FB6357269965A2014ABc35 (CANONICAL factory 0x1F9843..., fee 3000, WBTC/USDC) aborted at solve block 25714809: solver snapshot tick=64754 vs on-chain tick=64744.

**The abort message was misleading:** it blamed "non-canonical Swap topic0." On-chain shows the pool is canonical and all 14 of its recent logs are the canonical Swap topic0 (0xc42079...). Corrected the message (solver_state_verifier.rs) to lead with the far more likely "pump drain/state-advance stall" and mention non-canonical topic0 only as a forked-pool possibility.

**On-chain timeline (cast slot0):** tick held ~64754 through block 25714805 (matched the solver), moved to 64744 at block 25714809 via two canonical swaps IN that solve block. So the solver was never wrong about price until the solve block — the solve-block swaps simply weren't applied before solving.

**The real anomaly:** pool update_block froze at 25714794 while the pump's clock/current_block climbed to 25714809 — a 15-block gap vs the ~4-block normal settle lag. And ~160 CL/V2/V4 pools clustered at update_block 25714793-95 (the backfill→live handoff boundary) in the aggregated reporter.

**Conclusion:** SYSTEMIC post-backfill pool-state drain freeze. This run resumed from a DB snapshot at 25711404, backfilled 3388 blocks to 25714792, then went live; the first ~3 live blocks applied (re-injected WS events), then per-pool update_blocks stopped advancing while the engine clock (headers + finalize) kept climbing. Any pool with on-chain activity after the freeze diverges; 0x99ac8c tripped it first because its swaps landed in the solve block.

**Note:** most of the ~160 "outlier" pools are benign (inactive — their update_block simply doesn't move while the clock climbs); only pools where on-chain actually moves after the freeze are lethal. The aggregated reporter + outlier flagger correctly surfaced 0x99ac8c at stale_by 11→13→15 before the abort.

**Hypothesis:** after the backfill→live handoff, the per-pool drain does not re-arm to keep advancing pool update_block while the engine clock advances — the first few re-injected (drained-during-backfill) blocks apply, then it stalls. recovery_anchor starts at 0 (not pre-raised at resume), so it does not appear to be dropping the live blocks.

**Next (instrumentation, not a blind fix):** add a per-loop drain-progress probe in run_with_stream on the post-backfill path (e.g. log pool-apply block vs clock per settle tick) so the next resume from a stale snapshot pins which component (drain sink vs per-pool pump vs WS log arm) stops advancing. Then fix + regression test. After ANY stale-snapshot resume the bot's pool state can trail the solve head by 15+ blocks, so a restart should re-seed from a fresh snapshot until this is fixed.
