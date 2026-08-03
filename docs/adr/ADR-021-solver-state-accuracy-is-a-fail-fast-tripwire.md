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

- **The extraction itself** (one tripwire module, typed verdict, converge the
  three verifiers onto the `degenbot-rpc::abi` home) — recorded as the intended
  shape here; the code change is a separate task.
- **Per-class thresholds/policy** (e.g. whether `DeliveryLag{N}` for different
  N should trip immediately or only past an anchor-exact mismatch) — a policy
  detail to settle when the tripwire module lands, within the hard boundary
  that it never heals.

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
- `rust/crates/degenbot-bot/src/bot_core/solver_state_verifier.rs` +
  `block_pump.rs` (`verify_solver_state_against_chain`) — the AV42C7 gate the
  posture governs.
