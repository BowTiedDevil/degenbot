# YXHHKR result — remove solve-side staleness pre-gate

Commit: `9d4c2b8a` ("fix(rust): remove solve-side staleness gate (YXHHKR)")

## What changed
- Deleted `hop_is_too_stale`, `MAX_SOLVE_STALENESS`, the `[solve-stale]` defer branch
  and its two unit tests from `solver_dispatch.rs`.
- Rewrote the two behavioral tests to the post-fix semantics and renamed:
  `quiet_pool_frozen_far_behind_is_solved_not_deferred`,
  `no_update_block_age_defers_a_quiet_path`. Added
  `quiet_pool_that_swapped_11_blocks_ago_is_still_solved` (the RED→GREEN test).
- Kept `hop_is_future` + the `[future-price]` invariant (a genuinely-ahead clock is
  never legitimate).
- Updated the `solver_dispatch` module note and `bot_core::pool_update_block` doc to
  cite the ADR-021 verifier (`verify_solver_state_against_chain`) as the sole
  chain/solver-mismatch guard.

## The mechanism
The ADR-021 verifier already does the accurate job: per-hop fresh reads at each hop's
OWN `update_block` anchor, `std::process::abort` on the first real desync, before
simulation. The removed gate was a redundant, wrong heuristic (defer every quiet pool
by age, no read). On a genuine stale/desync pool the bot now fails HARD and LOUDLY —
the preferred behavior.

## Verification
- 404/404 `degenbot-bot` lib tests pass (incl. the verifier desync-abort tests in
  block_pump), clippy + fmt clean, pre-commit gate clean.
- Live run (`DEGENBOT_ONGOING_DISCOVERY=0 DEGENBOT_ASSERT_SOLVER_STATE=1`, logs/yxhhkr_validate.log):
  `[solve-stale]`=0 (gate gone), `[SOLVER-STATE] ABORT`=0 (tripwire armed+silent), pump
  healthy. Pure-live streaming produced no dirty-pool events (no backfill storm this
  time), so hot-loop quiet-co-hop solving is proven by the deterministic engine tests,
  not the live window.

View the live-verification detail in the task body.
