# Solver-state desync 2026-08-08 — Tracked-pool lag vs the solve anchor

Investigation of the single `SOLVER-STATE` abort in `logs/bot_run.log` (~4.4 GB,
9.79M lines). The bot subscribed at block 25712153, aborted at 25713079
(~925 blocks over 3h06m). This document characterizes WHY a Tracked pool could
trail the promoted solve anchor, whether the observed 5-block lag was chronic or
one-off, and which pools could exhibit the same behavior.

## 1. The incident (recap)

- `DEGENBOT_ASSERT_SOLVER_STATE: verified desync — ABORT` at solve block
  **25713079**, `path_idx=2` (`solver_state_verifier.rs`, UO3JM4 gate).
- Failing hop: **Tracked** V4 pool `0x000000000004444C5dC75cB358380D2e3dE08A90`
  / id `0fb0e40cec…`, `update_block=25713074`, `stale_by=5` (`cov=Tracked,
  lifecycle=Live`). On-chain at 25713079 had moved; the solver consumed the
  stale `sqrt`. A V3 hop in the SAME path was current (`update_block=25713079`).
- Solve anchor is promoted as `active_block = max(current_block,
  pool_state_head())` (`block_pump.rs`), where `pool_state_head()` is the **global
  max `update_block`** across all pools — so one lagging pool can be solved
  against stale state while siblings are current.
- **This was the only verifier trip in the entire run.** The gate aborted before
  any on-chain submission (dispatch for 25713079 was still mid-solve).

## 2. Pump clock — normal, no backlog

| metric | value |
|---|---|
| blocks | 925 (25712153 → 25713079) |
| avg cadence | 12.04 s/block (= Ethereum mainnet) |
| gap_secs p50 / p90 / p99 | 12.0 / 13.5 / 15.4 s |
| gap_secs min / max | 0.0 / 25.8 s |
| gaps > 15 s | 18 of 925 |

- The worst 2-slot stalls (gap_secs 24.5–25.8) were at blocks
  **25712349, 25712734, 25712969** — all far before the abort.
- The abort block itself had a **normal 12.3 s** cadence.
- Conclusion: the bot kept pace with mainnet end-to-end; the abort was **not**
  preceded by a header stall or an accumulating backlog.

## 3. Solve load — heavy and sustained (the pressure)

- **11,868** `rebuild_and_solve_affected` calls ≈ **12.8 / block**.
- Dispatch phases: `phase_candidate_count` p50=288, p90=493, p99=670, max=805;
  **514,618** candidate paths evaluated across **1,736** phases.
- Under this load a single hot pool's event application can transiently trail
  the promoted solve anchor by a few blocks — the mechanism behind the 5-block
  lag — while other pools keep up.

## 4. Pool activity — 0fb0e40 is NOT uniquely hot

- **60** distinct V4 pools appear in `debug-v4-solve`; the top ~10 each emit
  >190k snapshots.
- `0fb0e40…` is **#2 most active** (288,172 snapshots), just behind `8aa4e11…`
  (331,130).
- Conclusion: the lag class applies to **dozens of equivalently-hot pools**, not
  a single special one. Any of them can transiently lag → the diagnostic must be
  pool-agnostic (done in OC34VZ).

## 5. Was the 5-block lag chronic?

- The **only hard proof** of >3-block lag is the single abort (extractor read
  `update_block=25713074`). Its state caught up to the true block-25713079 value
  (`79254690086189808564741417144` = on-chain at the solve block) immediately
  after the solve.
- Over the run the pool's sqrt oscillates; the block-25713074-family value
  appears 1,857× and the block-25713079-family 1,026× — consistent with
  **transient per-block catch-up**, not a chronically frozen snapshot.
- Conclusion: best characterized as a **transient, load-induced catch-up lag on
  an ultra-hot pool** — not chronic, and not a permanent decode miss
  (contrast the PancakeSwap non-canonical-topic0 class in
  `docs/exploration-no-profit-crash.md`).

## 6. Which OTHER pools lag? — not answerable retroactively

Per-block, per-pool `update_block` is **not** logged at INFO, so the lag pattern
across the other ~59 active pools cannot be enumerated from this log. This is the
exact visibility gap that OC34VZ closes: the generalized `lagging_tracked_hops`
WARN reporter will surface **any** Tracked Live pool trailing the anchor by
>3 blocks, without needing an abort, on whichever pool demonstrates the behavior.

## 7. Correlation summary

- Abort occurred under **steady high solve-load** (~13 rebuilds/block, ~300–500
  candidates/phase) with a **normal header cadence** (12.3 s).
- The rare 2-slot header stalls occurred elsewhere and did not themselves trip a
  desync.
- Net: the primary load-bearing conditions are (a) a global-max solve anchor and
  (b) heavy solve load that can let a single hot pool's state-application run a
  few blocks behind.

## Reproducible commands (against `logs/bot_run.log`)

```bash
# Cadence / gaps
grep -oE "gap_secs=[0-9.]+" logs/bot_run.log | cut -d= -f2 | sort -n | awk '{a[NR]=$1}END{n=NR;print n,a[1],a[int(n*0.5)],a[int(n*0.9)],a[int(n*0.99)],a[n]}'
grep "gap_secs=2" logs/bot_run.log | sed -E 's/\x1b\[[0-9;]*m//g' | grep -oE "number=[0-9]+.*gap_secs=[0-9.]+"
# Solve load
grep -c "rebuild_and_solve_affected called" logs/bot_run.log
grep -oE "phase_candidate_count=[0-9]+" logs/bot_run.log | cut -d= -f2 | sort -n | awk '{a[NR]=$1;s+=$1}END{n=NR;print n,a[1],a[int(n*0.5)],a[int(n*0.9)],a[n],s}'
# Pool activity
grep -oE "pool_id=[0-9a-f]{64}" logs/bot_run.log | sort | uniq -c | sort -rn | head -10
grep -oE "pool_id=[0-9a-f]{64}" logs/bot_run.log | sort -u | wc -l
# Only abort
grep -c "verified desync" logs/bot_run.log
```

## Open item (feeds the decision task)

Whether to (a) keep the verifier as the only tripwire, (b) make the solve wait
on / stall for a lagging Tracked pool before promoting the anchor, (c) self-heal
by advancing the lagging pool's state from chain at the anchor before solving, or
(d) raise `MAX_CL_STALENESS_BLOCKS` (likely wrong — weakens the guard). Addressed
in the decision task, pending approval.

---

## 8. Decision (GEKZ25) — fix strategy for lagging Tracked pools at the solve anchor

### Root-cause statement
A **single Tracked Live V4 pool that trails `pool_state_head()` by a few blocks**
can silently poison one path's solve: the solve anchor promotes to the GLOBAL max
`update_block`, so a hot pool a few blocks behind is solved against a stale `sqrt`
while siblings are current. The UO3JM4 verifier catches it and aborts (correct +
capital-safety-preserving), but only after the stale solve is already built.

### Evidence summary
- **Rare**: one trip in ~925 blocks / 3h06m; pool caught up to the true
  block-25713079 state immediately after the solve.
- **Not chronic, not a decode miss**: `0fb0e40…` is merely #2 of ~60 equivalently
  hot V4 pools; its lag is a transient load-induced event-application lag under
  ~12.8 rebuilds/block and ~300–500 candidates/phase.
- **Not load-agnostic**: the abort came at a normal 12.3 s cadence (no header
  stall), so it is solve-load-driven backpressure on a hot pool, not a WS/header
  gap.

### Options considered
- **(a) status quo** — abort is the only tripwire. Safe, but zero pre-solve
  signal and kills the whole bot on a transient.
- **(b) wait/stall before solving** on a lagging Tracked pool. Fragile: under
  sustained load a lagging pool could stall the pump loop indefinitely.
- **(c) bounded pre-solve refresh** — at the anchor, refresh a participating
  Tracked Live pool's scalar (+ tick-map) state from chain when it trails past
  threshold; keep the UO3JM4 abort as backstop if the refresh still diverges.
  Directly fixes the mechanism; adds RPC reads to the solve path.
- **(d) raise MAX_CL_STALENESS_BLOCKS** — REJECTED: normalizes stale solves and
  weakens capital safety (violates UO3JM4 "do not silence").

### Recommendation
**Two-phase, evidence-gated.**
1. **Now (no behavior change):** ship the generalized visibility — the
   `lagging_tracked_hops` WARN reporter (OC34VZ) now surfaces ANY Tracked Live
   pool trailing >3 blocks, and this doc characterizes the run. The robust UO3JM4
   abort remains the safety net. No solve-time behavior is altered.
2. **Approve (c) for a follow-up task, gated on a live run:** before implementing
   a pre-solve refresh, run the bot with the reporter over a live window to
   confirm whether >3-block lag recurs on this or another pool (a few blocks per
   run = transient, worth surfacing, but likely not worth the solve-path RPC cost
   of (c); sustained multi-block lag = implement (c)). If recurrence is negligible,
   keep (a)+reporter and skip (c).

### Awaiting approval
Decide between: **(i)** approve the follow-up task to implement (c) now, or
**(ii)** defer (c) until the reporter has gathered a live window of lag data and
the decision is revisited (status quo + reporter in the interim). The UO3JM4 abort
is preserved in all cases.
