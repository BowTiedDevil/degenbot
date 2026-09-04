# ADR-021: Solver-state accuracy is a fail-fast tripwire — divergence is a defect, never an input to auto-repair

**Status: accepted (architecture).** Recorded during the architecture review
(2026-08-02) to settle the *posture* of the state-accuracy verification so a
future review does not re-suggest the auto-repair path. It both records the
current fail-fast behaviour of the AV42C7 gate and names the forward deepening
(a single discriminating tripwire module) as the intended shape — the extracting
is the work; the *posture* (detect, classify, stop loudly, never heal) is
decided here.

## Context

The live backrun session (2026-08-02, `docs/exploration-live-debug-session.md`)
repeatedly surfaced a slow-but-connected WS: pool state lags on-chain by 4–37
blocks while the solver keeps solving on a desynced snapshot. The AV42C7
solver-state accuracy gate (`DEGENBOT_ASSERT_SOLVER_STATE`) detects this
correctly — it `eth_call`s the canonical on-chain scalar state (V2 reserves;
V3/V4 sqrt/liquidity/tick) and diffs it against the `BotState` snapshot
`resolve_path` consumed, per hop at its OWN `update_block` anchor so normal
latency is tolerated. The live observation, though, surfaced two orthogonal
tensions:

1. **The response to detection.** When the gate tripped, the initial reaction
   was a `panic!` that unwound the pump tokio task while the process kept
   running — wedging the bot into a no-progress busy loop for ~8 minutes
   (fixed in-session by converting the `panic!` to a clean `shutdown` flag).
   That fix is narrow; the broader question of *what the gate is for* was open.
2. **The temptation to recover rather than stop.** The obvious-sounding fix for
   the state-lag class is "apply missed swaps" or "re-sync the pool from
   chain" so the bot keeps running through temporary WS lag. This review rules
   that out on principle: a divergence is evidence of a *defect* in
   state-tracking/decoding (a missed log, an unhandled reorg, a directly
   storage-mutated pool, a decode bug) — auto-repairing the scalar state
   silently masks exactly the class of bug this module exists to surface.

The two concerns must not be conflated. **Delivery reconciliation** (re-fetching
*dropped WS logs* so the pump's block clock is correct — ADR-008 Edge-B
backfill) is correct behaviour, not smoothing. **Scalar-state repair**
(re-writing the sqrt/liquidity/tick a hop solves on so it matches the chain) is
the forbidden auto-heal.

## Decision

### D1 — State-accuracy verification is a tripwire: detect, classify, stop loudly, never heal.

> **Partially superseded (2026-09-04) by
> [ADR-040](ADR-040-per-bucket-failure-reactions.md):** the *reaction* is no
> longer uniformly a clean loud stop. The strict gate now reacts per the
> closed failure-bucket matrix — tainted classes (`MissedLog`,
> `UnhandledReorg`, `StorageMutated`, `Unclassified`) **quarantine the
> divergent pool** (solver never consumes a divergent hop) and keep the
> session alive; `DeliveryLag` stays report-only per Part B; process-scope
> fatals still exit loudly. D2 (classes) and D4 (never-heal) are unchanged.

The solver-state accuracy check is a **fail-fast, loud, discriminating
tripwire** whose only job is to surface a defect so it gets root-caused. It must
not re-sync pools, must not apply missed swaps, and must not treat staleness as
recoverable input. When a divergence is genuine (at the hop's own anchor), the
correct outcome is a **clean, loud stop** with enough breadcrumb to name the
defect class — never a silent repair and a continued run. `shutdown` (clean
exit at the loop top) is the sanctioned reaction; a wedging `panic!` is not.

### D2 — Divergence classes are first-class, so the bug is cheap to solve.

The tripwire's leverage is *fidelity*: a trip should name the class, not just
report a scalar diff. The classes the live session conflated by hand are
canonical and are to be discriminated by the verdict:

- **`MissedLog`** — a swap/liquidity event was never delivered/applied.
- **`UnhandledReorg`** — a reorg was not rolled back before solve.
- **`StorageMutated`** — the pool's storage changed without a corresponding
  event (direct storage mutation, non-canonical fork).
- **`DeliveryLag{blocks}`** — the WS is slow-but-connected: state lags the
  chain at the solve block but is honest at its own anchor.

Delivering the class (not a raw before/after scalar) is what turns the
live session's "discriminator is open" archaeology into an instant root-cause
starting point.

### D3 — The tripwire deepens into one module; the three verifiers converge.

The accuracy concern currently lives across three verifiers with three
mechanisms (the solver-state verifier, the liquidity verifier, and the
sim-revert diagnostics), each copying the same on-chain-diff shape into its own
stop/panic reaction. The intended shape is **one state-accuracy tripwire
module** with a single typed verdict and one probe backend — the shared
`degenbot-rpc::abi` home (ADR-003's cross-consumer onchain-probe
infrastructure). Two adapters doing the same probe on different triggers is the
two-adapter signal that a real seam exists; this decision names that seam and
its module. The pump loop keeps only the trip and the exit; classification
concentrates in the module.

### D4 — The scope boundary: delivery recovery is in-scope, scalar-state repair is out.

- **In scope (delivery):** re-fetching dropped WS *logs* and reconciling the
  block clock (ADR-008 Edge-B backfill), clean shutdown on an
  unreliable-WS late forward. This is correct delivery, not a heal.
- **Out of scope (state):** re-syncing the scalar state a hop solves on,
  applying missed swaps to mutate pool state, or treating >N-block staleness
  as a recoverable condition. This is the forbidden auto-heal.

A future delivery-recovery seam (the `block_pump` deepening — Candidate 1 of the
same review) may generalise Edge-B to dropped-log delivery; it must never extend
to scalar-state repair.

## Consequences

- The AV42C7 gate stays stop-only, but gains the D2 discriminator so a trip is
  actionable ("This pool is `DeliveryLag{37}` at the solve block", not a raw
  sqrt diff).
- Root-causing the live-session failure classes becomes faster — the "open
  discriminator" gap in `docs/exploration-live-debug-session.md` is closed by
  the typed verdict rather than by hand-decoding log lines.
- No runtime behaviour regression: today the gate admits/pans the same states;
  this ADR only resolves *what it is* and *where classification lives*. The
  deepen (extract single tripwire module, converge the three verifiers) is
  downstream work, not an acceptance-time code change.
- A future architecture review that re-suggests "heal the state on divergence"
  now is re-litigating a settled posture; the correct response is: treat the
  divergence as the bug, fix state-tracking/decoding at its source.

## Alternatives considered

- **Auto-repair on detection (apply missed swaps / re-sync pool from chain).**
  Rejected as the load-bearing principle of this ADR: it converts a bug
  detector into a bug hider. A slow-connected WS that masks a genuine
  missed-log/reorg/decode defect behind a silent re-fetch would never surface
  that defect; the whole point of the accuracy gate is to surface it. Rejected
  decisively, not "for now."
- **Auto-repair gated behind a flag (off by default).** Rejected for the same
  reason: the posture is "treat divergence as a defect to solve," which a
  dormant-but-wired heal path would still violate on principle and would invite
  re-enabling. No hidden healing path.
- **Keep the three verifiers as-is, only change the reaction.** Partially
  retained (the stop-only reaction is confirmed), but rejected as the end
  state: it leaves the "open discriminator" archaeology in place. The D3
  convergence is the deepening that makes the tripwire worth having.

## Deferred

- ~~**The extraction itself** (one tripwire module, typed verdict, converge the
  three verifiers onto the `degenbot-rpc::abi` home)~~ — **settled**: the
  tripwire module landed (D3 slices 1+2, above). The probe backend is the
  pump's shared `AlloyProvider` path (the ADR-003 `degenbot-rpc` home is
  unchanged — the module consumes it, it does not own it).
- ~~**Per-class thresholds/policy** (e.g. whether `DeliveryLag{N}` for different
  N should trip immediately or only past an anchor-exact mismatch)~~ —
  **settled** in "D2 complete" below (default-off operator opt-in).

## References

- [ADR-003](ADR-003-botcore-state-layer.md) — the onchain probe as
  cross-consumer infrastructure; the `degenbot-rpc::abi` home the tripwire's
  single backend realizes.
- [ADR-008](ADR-008-block-state-machine.md) — the block state machine and its
  Edge-B delivery backfill; D4's in-scope/out-of-scope boundary.
- [ADR-020](ADR-020-tier3-onchain-accuracy-oracle.md) — the accuracy-oracle /
  fail-fast posture this ADR aligns with (a divergence is a defect, not a
  messy input).
- [`docs/exploration-live-debug-session.md`](../exploration-live-debug-session.md)
  — the 2026-08-02 slow-connected-WS failure class and the "open discriminator"
  gap this ADR's D2 closes.
- `rust/crates/degenbot-bot/src/bot_core/solver_state_tripwire.rs` (renamed from
  `solver_state_verifier.rs` at the D3 cutover) + `block_pump.rs
  (solver_state_verify_loop + trip_and_exit) — the AV42C7 gate the posture
  governs.

### D3 slice 1 — single judge interface (done 2026-08-20; ergo G4DGX2)

The gate is now a single in-module call — `judge(provider, config,
path_hop_states, anchor) -> GateVerdict { Ok, Divergent(TripwireDivergence) }`
— with the pump reduced to trip + exit (`trip_and_exit`: eprintln the
byte-identical `[SOLVER-STATE] ABORT` breadcrumb + `class={:?}` token, then
`std::process::abort`; no panic, no task unwind). The module reads ZERO env:
the pump packs `TripwireConfig { enabled, divergence_scan, anchor_probe,
staged_clock_probe }` at construction (default-on gate via
`bot_env_flag_default_on`; default-off diagnostics via the new
`bot_env_flag_default_off`). Strict-gate failures now carry their
`TripwireClass` (StorageMutated / MissedLog per the D2 mapping; read
failures stay Unclassified). The trip SET is unchanged (behaviour-parity
contract); D2's remaining classes (UnhandledReorg, DeliveryLag-as-trip)
and — at slice 1's time — the registration/sim-revert seam convergence was
the D3 remainder.

### D3 slice 2 — one tick-map verify seam (done 2026-08-20; ergo X6I3LN)

"Does the stored CL tick-map state match the chain at block B?" now has ONE
implementation. Design decisions (user-confirmed):

- **Coverage stays per-consumer** (OQ1): the pure compare takes a
  caller-supplied tick set — registration keeps its whole-stored-map forensics
  (full divergent set under DEGENBOT_VERIFY_DBG), the solve-time probe keeps
  stored ∪ bitmap-discovered (the UO3JM4 off-range class).
- **Seam home** (OQ2): the pure per-tick compare
  (`degenbot_pools::tick_map_verify::compare_tick_maps -> Vec<TickDivergence>`,
  ascending-tick deterministic, one shared `Slot0HeadScalars` fact type) lives
  in degenbot-pools next to the ADR-004 `TickMap` trait; the provider-bound
  batch reads (`batched_v3/v4_tick_reads`, one Multicall3 per tick set) stay in
  `bot_core::liquidity_verifier`. The dangling `crate::liquidity_verifier`
  intra-doc links in `degenbot-pools/src/tick_map.rs` now point at the real
  seam.
- **sim-revert finding** (OQ3): the `[sim-revert-swap]` diagnostic is NOT a
  tick-diff — it attributes captured reverted swaps to path hops by emitter
  and compares actual swap output vs the solver's predicted `hop_outputs[i]`.
  Its convergence is the fact type only: `RevertedSwapMatch.sim_scalars` is
  now `Slot0HeadScalars` (degenbot-pools), so this log and the tripwire's
  scalar state speak one type language. Log literals byte-identical.
- **Verdict, not reaction** (unchanged): registration keeps the typed terminal
  `VerificationMismatchError` (ADR-022, never aborts), the Python engine
  verify keeps its bridged error, the judge keeps `GateVerdict::Divergent` +
  trip + exit, sim-revert stays a non-aborting log. The batch shells
  (`verify_v3/v4_pools`, `verify_v3/v4_liquidity_map`) are now thin loops over
  one shared read + compare. Behaviour parity: byte-pinned first-mismatch
  messages, `[SOLVER-STATE] ABORT` / `[sim-revert-swap]` literals, batch-call
  counts (asserter-pinned) all unchanged. One documented delta: the
  historical first-mismatch choice followed `HashMap` iteration order
  (nondeterministic with multiple divergences); it is now deterministic
  ascending tick, same class precedence (stored gross > net > on-chain-only).

D3 remainder after slice 2: D2's remaining classes (UnhandledReorg evidence
via the reorg coordinator, DeliveryLag-as-trip policy per the ADR's "Deferred"
decision) — the seam-convergence scope of D3 is complete.


### D2 complete — UnhandledReorg evidence + settled DeliveryLag policy (done 2026-08-20; ergo IKKKWU)

**UnhandledReorg is now emittable (Part A, user-confirmed (a)).** The pump
already observed every reorg window (FSM `EnterReorg`/`ContinueReorg`/
`CloseReorg` → `ReorgCoordinator::dispatch_reorg_log` →
`restore_before_block`); it now also records them as a bounded
(`TRIP_REORG_WINDOW_CAP = 16`) `TripReorgWindow { deepest_removed_block,
opened_at, closed_at }` evidence list, snapshotted (Copies) before each
`judge()` await. The refinement is deliberate and conservative: a
strict-gate own-anchor mismatch (coarse `StorageMutated`) is re-labelled
`UnhandledReorg` only when a recorded rollback crossed the hop's anchor
(`deepest_removed_block < anchor`). Soundness: at a historical block the
chain state is immutable — a replaced block is the only mechanism that can
move chain@anchor away from the value an honest apply recorded — so the
refinement only re-labels (the trip is unchanged) and a decode/apply bug
that wrote a wrong value at derivation keeps the `StorageMutated` label.
`MissedLog` (honest at the anchor, moved later) is never re-labelled.

**DeliveryLag-as-trip is settled (Part B, user-confirmed (a) — default-off
operator opt-in).** Policy: lagging-but-honest trailing is the designed
operating mode below the operator's bar (report-only; the ~143
WARNs/block benign baseline is aggregated, not tripped); tripping on lag
is an explicit operator stance, not a default. Mechanism:
`TripwireConfig::delivery_lag_trip_blocks: Option<u64>` (env
`DEGENBOT_DELIVERY_LAG_TRIP_BLOCKS`, unset/empty/unparseable/zero = off):
when `Some(N)`, a new stage 5 of `judge` trips the gate as
`DeliveryLag{blocks: worst}` when the stage-2 aggregate worst `stale_by`
exceeds `N` — evaluated only after the strict gate passes, which still
wins when it fires. The default (`None`) keeps the trip set byte-for-byte
today's. The threshold is chain-dependent (37 blocks ≈ minutes on a 12s
chain, seconds on a 200ms L2), so no chain-agnostic constant is shipped —
that is the stated decision.

All red/green (7 new tests: crossing/non-crossing reorg re-label,
MissedLog non-refinement, beyond/within/default lag trip, window-record
helpers). Verified: degenbot-bot 509 green, `just test-rust` green, clippy
clean on degenbot-bot, extension rebuilt + engine-verify pytest green (10).
