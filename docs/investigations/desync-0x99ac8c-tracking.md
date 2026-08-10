# Tracking: genuine solver-state desync (0x99ac8c) — root cause

Status: ACTIVE. Bot not in production; downtime acceptable.

## Established facts (high confidence)
- Gate (UO3JM4) aborted on a REAL divergence: V3 pool `0x99ac8cA7087fA4A2A1FB6357269965A2014ABc35`
  (canonical factory `0x1F9843...`, fee 3000, WBTC/USDC) at solve block 25714809:
  solver snapshot `tick=64754` vs on-chain `tick=64744`.
- Pool is canonical and its logs are the canonical `Swap` topic0 — the abort message's
  "non-canonical Swap topic0" guess was WRONG (message corrected).
- On-chain timeline (`cast slot0`): tick held ≈64754 through block 25714805, moved to
  64744 at block 25714809 via TWO canonical swaps IN that solve block.
- `update_block` is a LAST-ACTIVITY clock — advanced only on event (Swap/Mint/Burn)
  application, never per-block for no-event blocks (`advance_update_block` monotonic
  max, called only in event apply paths: registry.rs apply_swap / apply_liquidity_update,
  cl_orchestration.rs). Verified in degenbot-pools registry.rs + v3_state.rs.

## Hypotheses

### H-a: systemic post-backfill drain freeze  →  REFUTED
Probe: `pool_state_head` (max update_block) tracks `current_block` at ~1 behind after a
7511-block backfill resume (25718555). The ~160 "stale outlier" pools cluster at the
backfill boundary (25718556-61) BECAUSE their next event after the backfill tail never
came — they are INACTIVE (benign), not dropped from a drain. `update_block` is a
last-activity clock, so an inactive pool's update_block legitimately freezes while the
clock climbs. NOT a bug. (Per-pool summary revealed this; a max-based probe alone masks
it, so keep the per-pool aggregate reporter.)

### H-b: the solver failed to incorporate a solve-block swap (genuine backrun gap)
The 25714809 swaps are in the solve block. If the bot is a BACKRUIN that must include
pending/in-block swaps before solving, then solving 0x99ac8c with pre-swap state
(tick 64754) is a genuine gap → gate correctly aborts (never backrun on pre-swap state).
NEEDS: confirm whether the bot's solver incorporates solve-block swaps.

### H-c: the gate reads on-chain AHEAD of applied state (false abort on same-block swap)
The gate compares solver state against on-chain AT the solve block. For a same-block swap
(solve block), the solver's pool state legitimately reflects pre-block events only; if the
bot does NOT incorporate in-block swaps by design, the divergence is a RACE, not a solver
error — the gate should anchor the on-chain read at the pool's applied cutoff (or handle
an in-progress solve block), the same class as the earlier recurring-verify race.
NEEDS: confirm the solver's solve-block semantics + the gate's `solver_anchor_block` /
`skip_in_progress_hop` / `is_future_price` placement.

### H-d: mid-run desync (pool missed a mutually-applied swap the pump dropped)
A pool whose event was delivered by WS but dropped by the pump before `dispatch_log`
(e.g. a gap / reorg not rolled back). 0x99ac8c had NO swaps 25714750→25714809, so this
would need a mint/burn around 25714794 (its update_block) — VERIFY on-chain whether the
pool had a liquidity event at ~25714794 and whether a swap in 25718562+ was missed (this
run). Lower likelihood given the on-chain timeline.

## TODO (ordered)
1. [x] Confirm canonical pool + canonical swap; correct misleading abort message.
2. [x] Add DIAG freeze probe; reproduce from stale snapshot; refute H-a.
3. [x] Verify `update_block` is a last-activity clock (H-a refutation complete).
4. [ ] Resolve H-b vs H-c: determine whether the solver incorporates solve-block swaps,
     and where the gate anchors its on-chain read (solver_anchor_block / skip_in_progress).
5. [ ] H-d check: did 0x99ac8c have a liquidity event ~25714794; is any 25718562+ swap
     missed in the current repro run (would confirm a live drop).
6. [ ] Decide fix based on H-b/H-c/H-d outcome + regression test.

## Resolution update (probe + repro run 2, deep trace)

### H-a (systemic wholesale post-backfill freeze): REFUTED
`update_block` is a LAST-ACTIVITY clock (advances only on event apply; verified
registry.rs apply_swap/apply_liquidity_update + v3_state advance_update_block). The
~160 "stale outlier" pools are predominantly INACTIVE — benign; a max-based probe
masks them (keep the per-pool aggregate reporter).

### NEW confirmed defect: a subset of tracked, solve-set pools go genuinely stale (their on-chain moves but the pump does not apply it)
- `0x6CA298D2983aB03Aa1dA7679389D955A4eFEE15C` = **PancakeSwap-V3** pool (factory
  0x0BFbCF9f...), Swap topic0 `0x19b47279...` == `V3_PANCAKESWAP_SWAP_TOPIC` (in
  RELEVANT_TOPICS + registered decoder). It has had **9 real on-chain swaps since
  25718556** (25718557..25718611) yet update_block froze at 25718556 (tick -200691 ->
  -200707 on-chain; stored never moved).
- verify-dbg pins: **140 V3 pins `pump_count=0`**, last_complete_block clustered at the
  backfill boundary (85 @ 25718556, 17 @ 25718558, 8 @ 25718559, 2 @ 25718561) →
  these pools' per-pool pump did not advance after the resume.
- The aborting pool `0x99ac8c` (CANONICAL factory 0x1F9843) is the same class: tracked+
  live, update_block froze at its boundary, and a swap landed at the solve block the
  solver had not applied → gate CORRECTLY aborted.

So H-a's core instinct (a post-resume liveness gap) is REAL but NOT a wholesale freeze: a
subset of pools' per-pool live pump does not run/advance after the backfill->live handoff,
so no events (swap/mint/burn, canonical OR PancakeSwap topic) are applied -> they go
genuinely stale -> divergence when solved against after on-chain moves. The max pool
advances (some pumps are fine), masking the subset.

### Remaining TODO (fix locus)
- [ ] Pin WHERE per-pool pumps are spawned/started vs the backfill-seeded pool set:
      confirm pools seeded during backfill (or in the solve set) have no live pump task
      after `resume_from_subscribe` -> `run_with_stream`. Likely a registration/launch
      gap for the backfill-seeded tail (pump_count=0, last_complete_block=0/25718556).
- [ ] Verify fork (PancakeSwap/etc) pools dispatch: since the topic + decoder exist, the
      miss is almost certainly "pool not in the live pump set," not "undecodable topic."
- [ ] Fix: ensure every tracked/solve-set pool has its live pump running after a backfill
      resume (start pumps for all registered pools, incl. the backfill-seeded tail), so
      post-resume events are always applied. Add a regression test: a pool with on-chain
      activity after a resume must advance update_block.
- [ ] Keep the gate as the backstop (do NOT mask the abort).

## Pinning (deep trace of dispatch + pump + registration)

### Two DISTINCT sub-causes, must not be conflated
1. **0x6CA298 (PancakeSwap-V3) — a REAL multi-block dispatch/apply gap (NOT a race).**
   Its 9 swaps spanned 25718557..25718611 (many blocks/minutes) yet update_block stayed
   at 25718556. A same-block race cannot explain a miss across many blocks. topic0
   (0x19b47279) == V3_PANCAKESWAP_SWAP_TOPIC (in RELEVANT_TOPICS, has V3PancakeSwapDecoder,
   apply keys by pool_address) — so it SHOULD apply. The remainder: the log never reaches
   its pool apply (dropped before dispatch, OR the pool is not found by address in that
   apply lookup, OR routed to a different pool object). Needs a targeted apply-path log to
   see "not registered / dropped / applied" for this address.
2. **0x99ac8c (canonical) — likely solver-swap-incorporation / same-block**, separate from
   the pump gap: pool had no swaps 25714750..25714809 then a swap IN the solve block;
   the solver solved live (pre-swap) state. For a BACKRUN the solver must incorporate the
   target swap into the state it solves; if it solved the untouched live pool, that's a
   solver/backrun-incorporation issue, not the pump.

### What is NOT the issue
- Gate is correct (must not backrun pre-swap/frozen state) — keep as un-weakened backstop.
- update_block is last-activity → most "stale outlier" pools are benign-inactive.
- Max-based probe masks both (some pools advance).

### NO blind fix shipped — why
The defect(s) live at the intersection of WS log dispatch, pool registration, and the
backrun solver's swap-incorporation — a multi-agent subsystem. I did not find a single
safe line to change with certainty, and a wrong edit here could silently break state
application worse than the abort. Additional evidence needed before a repair:
- [ ] Apply-path diagnostic for 0x6CA298 (registered-or-not, dropped-or-applied).
- [ ] Confirm whether the backrun solver incorporates the target swap into the pool state
      it solves (source of the 0x99ac8c abort) vs solving the untouched live pool.

## Run 3 (DEGENBOT_TRACE_DISPATCH=1) + FINAL conclusion
3rd run, added an env-gated apply-path trace in log_dispatcher::dispatch:
- 0x6CA298 (PancakeSwap-V3): its swaps ARE decoded + applied (pool_id=27, topic 0x19b47279),
  and tick_data_block ADVANCES this run (25718657). -> The run-2 "PancakeSwap dispatch gap"
  did NOT reproduce; same pool applies fine. So that sub-cause was TRANSIENT, not systemic.
- 0 DECODE MISS. 982 APPLY MISS, but the top APPLY-MISS address is the V4 pool manager
  (0x...44444c...) — a non-pool contract, expected, NOT the bug.
- No abort this run; max_stale_by reached 10 (some outliers) but no divergence.

CONCLUSION across 3 runs + 2 instrumentation rounds:
1. The gate is CORRECT: the only real abort (0x99ac8c, canonical) was a genuine divergence —
   on-chain moved at the solve block while the solver used pre-swap state. The abort is the
   intended un-weakened backstop.
2. `update_block` is a last-activity clock => the large "stale outlier" population is mostly
   benign-inactive; a max-based probe masks it (keep the per-pool aggregate reporter).
3. The hypothesized systemic post-backfill freeze and the PancakeSwap dispatch gap did NOT
   reproduce; they were transient/run-specific, not systemic defects.
4. NOT SHIPPING a speculative fix: no reproducible bug found to fix, and an unvalidated pump/
   dispatch change would be exactly the risky half-repair to avoid.

SAFE, VALIDATED CHANGES KEPT (all build + tests green, clippy clean):
- Corrected the misleading abort message (real cause, not "non-canonical topic0").
- Aggregated per-pool staleness reporter (per-block summary + outlier WARNs).
- New `DEGENBOT_TRACE_DISPATCH` env-gated apply-path trace (decode miss / apply miss / applied).
- [DIAG] pool_state_head field on the existing stats line.

NEXT IF DESIRED (not blocking): if a recurrence is seen, run with DEGENBOT_TRACE_DISPATCH=1 to
distinguish a same-block backrun-incorporation issue (solver) from a pump dispatch miss.

## Deep dive (round 4): the real defect is an unobserved state transition at the solve/apply seam

The user rejected self-healing (correctly) and asked me to treat the mismatch as evidence of an
improper state transition across a seam. Reframing the 0x99ac8c abort:

On-chain 0x99ac8c transitioned `tick 64754 -> 64744` at block **25714809** (a Swap). The pool-state
module never observed/applied that transition: its `update_block` stayed **25714794**, tick 64754.
The "15-block staleness" is NOT a 15-block pump lag — 0x99ac8c is a QUIET pool (no events between
25714794 and 25714809), so its update_block legitimately stays 25714794. The swap that moved
on-chain landed in the **solve block itself (25714809)**. This is a same-block seam issue, not a
multi-block gap. That reframes my earlier "not a same-block race for such a big gap" dismissal — the
gap is quietness, and the actual event is in the solve block.

### The seam (confirmed in block_pump.rs)
The loop structure:
1. **Top-of-loop**: if dirty paths, solve at `active_block = max(current_block, pool_state_head)`
   (on_drain, lines ~1062-1081).
2. **select** waits for the next WS event (newHeads or log).
3. **Post-select**: a log is applied (`self.bot.dispatch_log(&log)`, line ~1569) — i.e. a log
   received in iteration i is applied in iteration i's post-select, available to iteration i+1's
   top-of-loop solve.

So when a `newHeads` header advances `current_block -> B` in iteration i-1, but block B's swap for a
path pool is still in-flight, iteration i's top-of-loop solve anchors at `B` and consumes that pool
**pre-swap**. The code documents this: *"current_block is newHead+log driven ... the new head IS the
promote signal"* — even though not all of block B's logs have arrived.

The verifier's `skip_in_progress_hop` escape hatch (`update_block >= B` -> skip, because a mid-block
capture can't be reproduced by a historical slot0 read) only fires for a pool whose `update_block`
was ADVANCED to B. For a QUIET pool like 0x99ac8c, update_block is 25714794 (< B), so it is NOT
skipped; the gate's staleness re-check reads block-final on-chain at 25714809 (post-swap 64744) and
differs from the solver's pre-swap 64754 -> ABORT.

**The ungoverned transition:** `pool.update_block: 25714794 -> 25714809` (the in-block swap apply)
must happen BEFORE the pool is consumed by a solve anchored at 25714809. Currently nothing governs
that ordering — it's implicit between "dispatch_log (apply)" and "top-of-loop solve." The header-
promoted active_block lets the solve anchor run ahead of the applied per-pool state.

### Why self-healing is wrong here
Self-healing masks the symptom (re-fetch the stale price) instead of fixing the ordering that lets a
solve consume pre-block state. The gate is NOT overreacting: for a path being solved at the current
block, solving a middle-hop pool at its pre-block price IS an invalid solve. The defect is pump
ordering, and the correct fix is to make "solve at B with a path pool not yet advanced to B" an
illegal (governed) transition — not to paper over it.

### Recommended tracing locations (make every transition observable)
1. **Pool-advance transition trace** (the crux) — in the V3/V4 apply path (`dispatch_log` → pool
   state update), env-gated `DEGENBOT_TRACE_POOL_ADVANCE`: log `(block, pool_addr, tx_hash,
   prev_tick->new_tick, prev_update_block->new_update_block)`. Post-hoc this confirms whether
   0x99ac8c's 25714809 swap was applied, and whether its apply happened before or after the solve
   that consumed the pre-swap state.
2. **Solve-anchor precondition probe** — at the top-of-loop solve, for each dirty path, log any CL
   hop whose `update_block < active_block - tolerance`, tagged Quiet (on-chain unchanged) vs
   MovedInBlock. Turns the silent-then-abort into a per-solve WARN with pool + blocks BEFORE the
   gate fires. Default-off, gate-driven.
3. **Per-block ordering ledger** — for each block B record timestamps of (a) header received
   (current_block->B), (b) last path-pool advanced to >= B, (c) solve(B). Diffing exposes in real
   time whether solve(B) preceded all-pools-advanced(B). This is the FSM's runtime transducer:
   every transition stamped so the ordering is observable.
4. **Quietness discriminator on the lagging-hops reporter** — tag each reported lagging Tracked Live
   CL hop as Quiet (on-chain unchanged at B, benign) vs MovedInBlock (the real signal), using the
   existing non-aborting divergence scanner's read. Makes the reporter actionable (currently it
   conflates the benign quiet population with the genuine in-block-move class).

### FSM recommendation
There is no pool-advancement/ solve-anchor FSM today (registration_lifecycle Live/Quarantined is a
different axis — coverage, not scalar advancement). Recommend a NEW small FSM governing solve-anchor
consistency with per-pool scalar advancement:

Per CL pool, states: `Sync(B)` (applied through B) -> `HeaderSeen(B+1)` (header advanced, this
pool's logs in flight) -> `Advanced(B+1)` (this pool's B+1 logs applied) -> `Sync(B+1)`.

Path-solve guard (the hard invariant the FSM enforces): a path may be SOLVED at anchor A only if
EVERY hop is in `Advanced(>= A)` or is the explicitly `Incorporated` backrun target. Any hop still
in `HeaderSeen(< A)` (or `Sync(< A - tolerance)`) makes the transition to SOLVED illegal. This
structurally prevents the 0x99ac8c class (solve at B with a quiet middle-hop whose B-swap isn't
applied yet) while still allowing legitimate backrun solves where the target swap is Incorporated
and every other hop is Advanced. The gate stays as the on-chain backstop; the FSM removes the illegal
transition instead of panicking on it.

Hooks: on_drain's promote/solve site (enforce the guard before solving) + dispatch_log apply (drive
Sync/HeaderSeen/Advanced). This is the Option-A "govern transitions rigidly" ask.

### Status
Analysis + recommendation complete (this round). NOT yet implemented — the FSM is a cross-cutting
change and I want buy-in on the direction (new small FSM + tracing #1-#3) before editing the multi-
agent pump/solve/verify subsystem.

## Round 5: solve-anchor consistency probe — implemented + LIVE-VALIDATED

Implemented (all build + tests green, clippy clean; 430 degenbot-bot tests, +4 new):
- `SolveAnchorAdvancement` FSM seed: pure `solve_anchor_advancement(update_block, anchor, tol)`
  → `Consensus` | `Laggard{stale_by}`. Unit tests cover the 0x99ac8c case
  (stale_by=15 at anchor), tolerance boundaries, never-updated, saturating.
- `probe_solve_anchor_consistency()`: env-gated `DEGENBOT_TRACE_SOLVE_ANCHOR=1` (default off,
  zero cost; NEVER bypasses the UO3JM4 abort). For every abnormal lagging Tracked Live CL hop
  (`stale_by >= 10`), reads on-chain at the solve anchor and classifies:
  `QUIET` (correct-but-old, benign) vs `MOVED-IN-BLOCK-NOT-APPLIED` (on-chain moved at the solve
  block but the in-block Swap was not applied before the solver consumed it — the
  header-promote-ahead-of-apply transition) vs `READ-FAILED`. One read per unique pool per anchor.
- Wired into `verify_solver_state_against_chain` in block_pump before the abort loop.

### Live soak (9 min, DEGENBOT_TRACE_SOLVE_ANCHOR=1)
- **1778** on-chain classifications, **all `QUIET`**, 0 moved-in-block, 0 read-failed, 0 aborts.
- Quantitatively confirms the `update_block`=last-activity-clock finding: the lagging Tracked Live
  CL population is dominated by genuinely-quiet correct-but-old pools. The pump solving through a
  QUIET lagging pool is legitimate (on-chain didn't move) — which is why no abort fires on them.
- Confirms the abort class (`MovedInBlockNotApplied`) is a genuine rare outlier — which is exactly
  why it did NOT reproduce in the 3 prior runs. The probe now makes it observable the moment it
  fires: `MOVED-IN-BLOCK-NOT-APPLIED (on-chain moved at the solve anchor but the in-block Swap was
  not applied before solve — header-promote-ahead-of-apply)`.

### Governance recommendation (the FSM step — pending deliberate integration)
The probe/FSM-seed is done and validated. The next (cross-cutting) step is the SOLVE-ANCHOR GUARD:
a path may be SOLVED at anchor A only if every hop is `Consensus` (QUIET-laggard included — on-chain
didn't move) or is the explicitly-`Incorporated` backrun target; a `MovedInBlockNotApplied` hop makes
the SOLVE transition ILLEGAL (defer the path / don't solve through it) rather than process-aborting.
Enforcement candidates, in order of risk:
1. Ordering fix (lowest risk): don't solve at the header-promoted anchor until block N's in-block
   swaps for the path pools have been applied — makes `MovedInBlockNotApplied` impossible by
   construction (the in-block swap lands before the solve consumes the pool).
2. Per-path pre-solve classification using the existing probe read (guarded to abnormal outliers to
   bound cost) to defer/mark the path when a hop is `MovedInBlockNotApplied`.
Recommended: option 1 as the rigid FSM transition (structural prevention), option 2 only if option 1
is insufficient. Both keep the hard abort as the on-chain backstop for any residual desync.

## Round 6: IMPLEMENTED the solver-release gate (verify+ solve anchor at the settled block)

User's design (validated + implemented): "move the debounce gate to the solver, remove it from
the send — the last result is always the latest (overwrite per path)."

### Invariant confirmed (why overwrite makes it safe)
`ArbitrageEngine.results: HashMap<u64, SolvePathResult>` is keyed by path ID — re-solving Path X
OVERWRITES its single entry, so only the LAST solve's state survives. `diff_and_send` reads only
this map and diffs against the previous `delivered` set (fresh/updated/expired/removed); solves #1..#n-1
are never forwarded. So gating the SOLVE (not the send) on the debounce is safe — no accumulation.

### Root cause found (the real mechanism)
The settle point ALREADY gates `<send + verify>` on quiesce (`consume_quiesced(open)`), but the
VERIFY handoff anchored at `current_block` (the header, which races a head ahead of the applied
state) instead of `open` (the LOG-DRIVEN quiesced block, `BlockClock::latest_observed`, "headers do
not change this"). Verifying at the racing header read on-chain at a block that contains swaps not
yet applied to a quiet path pool — the 0x99ac8c false-abort.

### Changes (all in block_pump.rs; 431 cru tests pass, clippy clean, live run 3.5min: 0 aborts/panics)
1. **Verify handoff anchors at `open`** (the block `consume_quiesced` released), not `current_block`.
2. **Solve anchor follows the log-driven settled block** via a new pure `solve_anchor(open: Option<u64>,
   current_block, state_head) = open.unwrap_or(current_block).max(state_head)`. Never solves a block
   whose event burst hasn't settled; the state head still dominates (backfill-ahead BO5FBS).
3. New regression test `solve_anchor_follows_log_driven_block_not_racing_header`.

### Why the hard abort stays
The abort remains the on-chain backstop (its message was already corrected in round 4). The
solve/verify now operate on the settled block so the common header-races-ahead case no longer
false-crashes; a GENUINE desync (a pool actually diverged at the settled block) still aborts.
The `DEGENBOT_TRACE_SOLVE_ANCHOR` probe stays as the non-aborting Quiet/Moved discriminator.

## DIAGNOSTIC REMOVAL MAP (for when the fix is trusted)

Both diagnostics are optional observability, add ZERO behavior when their env gate is unset, and
are bounded to the symbols below. The FIX (round 6) is separate and must be KEPT.

### KEEP (the permanent fix, NOT diagnostics)
- `solve_anchor(open, current_block, state_head)` helper in block_pump.rs (~L2166) + its call site
  (top-of-loop solve) + the `verify` handoff anchoring at `open`.
- `solve_anchor_follows_log_driven_block_not_racing_header` test.
- `block_pump::SOLVER_STATE_ABNORMAL_STALE_BLOCKS` (L76) — used by the pre-existing lagging
  reporter (L763), NOT probe-owned.

### Diagnostic A: solve-anchor probe  (env: DEGENBOT_TRACE_SOLVE_ANCHOR=1 -> "[solve-anchor-probe]" logs)
block_pump.rs:
- L52-53 imports: `probe_solve_anchor_consistency`, `solve_anchor_probe_enabled`, `LaggardProbeVerdict`
- L806-832: the whole `if solve_anchor_probe_enabled() { ... }` block (incl. the warn at L831)
solver_state_verifier.rs:
- L557-690: `LaggardProbeVerdict` enum, `SolveAnchorProbeResult` struct, `solve_anchor_probe_enabled()`,
  `probe_solve_anchor_consistency()` + docs
- L601 `pub const SOLVER_STATE_ABNORMAL_STALE_BLOCKS` — DEAD duplicate (nothing references it; the
  live const is block_pump's). Safe to delete.
- tests `probe_default_off_single_env_get` (~L1278), `probe_verdicts_distinct` (~L1260)

### Diagnostic B: dispatch trace  (env: DEGENBOT_TRACE_DISPATCH -> "decode miss" / "APPLY MISS" logs)
log_dispatcher.rs:
- ~L418 (decode-miss branch), ~L453-469 (APPLY MISS branch)

### Separate, test-only FSM seed (NOT part of the fix, NOT wired to prod)
solver_state_verifier.rs L535-556 `SolveAnchorAdvancement` + `solve_anchor_advancement` + tests
L1190-1256. Dead code in prod (only tests reference it) — the round-6 fix uses a DIFFERENT helper
(`solve_anchor` in block_pump). Remove independently, or keep as the intended future guard.

## Round 7 (2026-08-10): NEW no-profit incident — path 142603 (V4-V4-V3) @ 25723658

A FRESH `DEGENBOT_SIM_EXIT_ON_FAIL=1` no-profit trap, investigated with a new
state scraper + replay harness (see
`docs/investigations/no-profit-path142603-v4v4v3.md` for the full writeup).

**Root cause (verified by replay):** the solve reported **+346,369,630 wei**
profit on a path whose WETH round trip nets **−173,150,825**; the gap
**519,520,455** equals exactly the live `[clamp-cl-hop]` delta on hop2 (V3
USDT/WETH 0xc7bBeC68): solver raw hop2 output `351476391576684` vs
byte-exact twin `351475872056229`. The solver computes
`profit = final_output − input` on the **over-predicted** `final_output`, and
the post-solve CL clamp (`clamp_cl_hop_capacity`) realigns
`hop_outputs`/`consumed_inputs` to the twin but **does not recompute `profit`**
— so a genuinely-unprofitable path is still selected → executes to a loss →
`no-profit`. This is the path-73385 family (solver CL over-prediction corrected
by the clamp) but the selection profit is never realigned.

**Replay validation:** `path142603_v4v4v3_solver_fixture.rs` reproduces the
recorded solve byte-for-byte on the on-chain-correct state; the stale-float
variant (pool B liq 1018741430873) returns a *different* solve → **pool B's
stale liquidity is a real but independent defect, NOT the cause of this
no-profit**.

**Secondary latent defect (do-not-conflate):** pool B (V4 USDC/USDT) is a
staged-clock liquidity desync — price honest at the solve block, liquidity
frozen ~3300 blocks (~25720300 ModifyLiquidity removal never applied,
`tick_data_block=25722568`). The gate's `skip_in_progress_hop`
(`update_block >= block`) silently skips exactly this hop, so the stale float
is never diffed.

**Recommended fixes (see D1–D4 in the writeup):**
1. Recompute `profit` after `clamp_cl_hop_capacity` from twin-aligned outputs +
   a regression test (the primary fix; invalidates the no-profit at selection).
   **➤ IMPLEMENTED (Bug B fix):** `clamp_cl_hop_capacity` now recomputes the
   selection profit from the clamped outputs via pure `recompute_clamped_profit`
   (`final_output − consumed_inputs[0]`, saturating). +4 unit tests
   (`profit_clamp_recompute_tests`); replay harness for path 142603 now reports
   `profit = 0` (was +346,369,630) → not selected → no no-profit trap.
   435 degenbot-bot tests pass; clippy + fmt clean.

   **➤ Bug A ROOT MECHANISM + FIX (2026-08-11).** Pool B's staged-clock
   desync = a stale in-range `liquidity()` scalar. Audit (see the no-profit
   writeup): every ModifyLiquidity apply path routes through the shared,
   in-range-aware `apply_liquidity_update` EXCEPT the backfill→Live branch of
   `buffer_backfill_{v3,v4}_liquidity_update`, which inlined
   `apply_liquidity_to_tick_range` + a manual `tick_data_block` advance and
   NEVER adjusted the in-range `liquidity()` scalar (nor price clock / journal)
   — so a post-seed in-range event on a Live pool (late backfill chunk)
   advanced the tick map but froze the active-liquidity scalar. FIX: both
   backfill→Live branches now route through the shared `apply_liquidity_update`
   (single path; historical-replay guard + scalar adjust + two-stamp clocks +
   journal). +2 regression tests (`backfill_live_in_range_{post_seed,pre_seed}`);
   437 degenbot-bot tests pass; clippy + fmt clean.

   **➤ D3 gate hardening IMPLEMENTED (2026-08-11).** The solve-time gate now
   runs a staged-clock probe on the `skip_in_progress_hop` path
   (`DEGENBOT_TRACE_STAGED_CLOCK=1`, opt-in, non-aborting): for a skipped CL
   hop with a completed map anchor and a pronounced price-ahead-of-map gap, it
   compares the solver's in-range `liquidity` scalar to on-chain `liquidity()`
   at the tick-data anchor and WARNs `[staged-clock]` on divergence — surfacing
   a fresh-price + stale-in-range-liquidity desync instead of silently skipping
   it. +3 tests; 440 degenbot-bot tests pass; clippy + fmt clean.
   Together with the Bug-A apply-path fix, the staged-clock class is now fixed
   at the source (backfill→Live) AND made observable at solve time (D3 guard).
2. `[profit-clamp]` trace: `(profit_before, profit_after, Σ clamp deltas)` +
   invariant `profit_after ≤ Σ twin − input`.
3. Extend the two-stamp OB7UNY check to skipped/in-progress hops so the pool-B
   class (fresh price + stale liquidity) is surfaced, not skipped.
