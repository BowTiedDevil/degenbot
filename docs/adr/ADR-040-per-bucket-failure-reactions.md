# ADR-040: Failure reactions are per-bucket — the closed failure taxonomy carries its own reaction

**Status: accepted (architecture).** Supersedes the *reaction* clause of
[ADR-021](ADR-021-solver-state-accuracy-is-a-fail-fast-tripwire.md) D1
(abort-only) and retires the coarse `DEGENBOT_FAILURE_MODE=exit|harden|continue`
channel (epic D63GSE scaffolding, `failure_policy.rs`) before it ever governed
the tripwire. **Supersedes nothing about ADR-021 D2 (classes first-class) and
D4 (never-heal boundary) — both are retained verbatim.**

## Context

The bot fails in many ways, and they are not interchangeable:

- A **verified state desync** (ADR-021 strict gate: `MissedLog`,
  `UnhandledReorg`, `StorageMutated`) means the solver's stored state for a hop
  is *provably wrong against canonical chain state right now*. Any subsequent
  solve over that path trades on wrong prices.
- A **simulation revert** can fall anywhere from pure economics (a marginal
  candidate whose gas cost swamps gross profit — the revenue gate working as
  designed) to a **deterministic encoding bug** (the same calldata reverts the
  same way every block, poisoning every candidate on that path) to a
  state-dependent revert that *may* be a desync surfacing early.
- Delivery-level failures (`WS log drop`, drainer death) are process-wide: there
  is no local surface to quarantine, because the feed or the drain pipe itself
  is broken — and a blind or wedged bot loses money as surely as a dead one,
  just more quietly.

The recorded postures on either end are both wrong at whole-epic scale:

1. **Abort-everything (today's tripwire, ADR-021 D1).** Any verified desync
   kills the process. Detection fidelity is excellent; availability is
   unforgiving — an unattended overnight run dies on the first trip
   (the FRKBGP drift-watch motivation: an OOM-kill or abort presents as an
   *availability* failure, not a solver symptom, and nothing trades until an
   operator returns).
2. **A single global failure mode** (D63GSE's `DEGENBOT_FAILURE_MODE=
   exit|harden|continue`). Three process-wide strokes cannot express the
   actual structure of the decision: the *correct* reaction depends on which
   bucket failed, and a global stroke either over-reacts (quarantining benign
   churn, exiting on a single RPC bounce) or under-reacts (letting a
   strict-gate desync keep trading through `continue`). A mode that must be
   second-guessed per bucket is not a simplification — it is a second channel
   that can contradict the first.

The raw material for granularity already exists in the codebase: the tripwire
emits a typed `TripwireClass`; the simulator's `classify_revert` frame
attribution already discriminates *where* and *why* a simulated execution
reverted; `RegistrationLifecycle::Quarantined` provides owned pool-lifecycle
machinery (ADR-022). What was missing is the decision layer that turns
*kind + reason* into a *per-surface* reaction.

## Decision

### D1 — Every failure bucket declares `{severity, taint scope, action}`; the reaction is a pure function of the bucket.

Failure reactions come from a **closed, per-bucket policy table** — not a
process-wide mode. Three orthogonal per-bucket properties:

**Severity** — how much damage continuing can do:

- `benign` — expected domain economics, not a malfunction. Never touches the
  `degenbot.errors` family (an expected outcome is not an error).
- `degraded` — transient or environmental; the next solve re-derives from
  fresh state and is trustworthy.
- `tainted` — the failure *poisons future trading decisions* on its scope.
- `fatal` — continuing is meaningless or unsafe at process scale.

**Taint scope** — the smallest surface whose future decisions are
untrustworthy: `pool`, `path`, or `process`.

**Default action**, derived from severity (overridable per bucket, §D3):

- `benign` → observe (counters only)
- `degraded` → keyed loud event + cooldown, keep running
- `tainted` → **quarantine the scope + keyed loud event**
- `fatal` → loud event, flush, exit

The reaction function is **total over the closed taxonomy**: a new failure
kind cannot compile without a table entry (the `error_kinds_are_unique`
test pattern extends to matrix completeness). There is deliberately **no**
three-way `FailureMode` envelope: escalation floors are undefined because
there is nothing to escalate — a tainted bucket *always* quarantines, a
benign bucket *never* surfaces through `degenbot.errors`.

The registered matrix (initial decision table; entries are named, not
hard-coded magic):

| Bucket | Severity | Scope | Default action |
|---|---|---|---|
| `sim_failure.revert_economics` (min-out / cost-envelope revert; net profit negative by gas) | benign | path | observe |
| `sim_failure.rpc` (transport flake) | degraded | process | event + keep running |
| `submit_failure` (broadcast rejection) | degraded | process | event + keep running |
| `monitor_failure` (unconfirmed/expired) | degraded | path | event + keep running |
| `sim_failure.revert_pool_state` (state-dependent revert) | degraded-with-cross-check¹ | pool | event; quarantines the pool only if the tripwire corroborates divergence |
| `sim_failure.pre_encode` (calldata/config built wrong) | tainted | path | quarantine path + event |
| `solver_state_desync.MissedLog` / `.UnhandledReorg` / `.StorageMutated` (strict gate) | tainted | pool | quarantine pool + event + repro dump |
| `solver_state_desync.Unclassified` (eth_call read failure etc.) | tainted | pool | quarantine pool + event (conservative) |
| `solver_state_desync.DeliveryLag{N}` | degraded² | pool | report-only (WARN; trip only at the operator's `delivery_lag_trip_blocks` bar, unchanged from ADR-021 Part B) |
| `verify_mismatch` (registration liquidity verify) | tainted³ | pool | deny admission (pool stays unregistered — ADR-022 typed terminal error; equivalent to quarantine-by-omission) |
| `ws_completeness` (live WS log drop, DFQYM5 invariant) | fatal | process | loud event + exit⁴ |
| `drain_stall` (B3 watchdog) | fatal | process | loud event + exit |
| `drain_dead` (drainer task gone) | fatal | process | event + exit |

¹ The cross-check is the ADR-021 tripwire judge over the suspect pool's
hops-at-anchor; a corroborated divergence promotes to `tainted`, an honest-at-
anchor read de-promotes to `degraded`. Suspect is not a fourth severity — it
is a degraded verdict with a tripwire escalation seam.
² `DeliveryLag` is report-only at any mode — it was settled as report-only by
ADR-021 Part B and no reaction tier re-litigates that here.
³ Containment already exists by omission: the pool is simply not admitted to
solve. Recorded here so the matrix is total over today's seams.
⁴ Operator may override a fatal to `degraded` (§D3) for a config-driven
keep-alive stance — a per-bucket, informed decision, not a global one.

### D2 — Loud events are the reaction's face; dumps are its memory. (kept intent, new mechanism)

ADR-021 D2 (classes first-class) is retained verbatim and extended to every
bucket: a surfaced event names the class and reason. Every non-benign surface
emits through `record_exception_keyed` (the D63GSE storm-deduped seam —
`(kind, primary_id)` cooldown, `COOLDOWN_BLOCKS` window): the alerting surface
shows one event per distinct bug per window; trace spans still carry every
occurrence. A **tainted** event additionally writes the reproduction
dump (span `degenbot.desync.trip`-style structured fields + a JSON artifact
under `logs/desync/`) sufficient to write a deterministic follow-up test from
the artifact alone.

### D3 — Operator control is per-bucket, declared, and validated — not a global mode.

The single knob this ADR ships is a per-bucket override table in
`config.toml` (`[failure_policy]`, closed keys, validated at boot against the
taxonomy; unknown keys are a boot error, not a warning). There is **no**
`DEGENBOT_FAILURE_MODE`-style global env channel: a tool that re-brackets
every bucket at once is exactly the second channel this ADR retires.
Overriding a `tainted` bucket to `degraded` is permitted but every instance is loud —
the override is logged as a boot-time event so a dashboard shows
*the operator disabled containment on purpose*.

### D4 — The never-heal boundary is unchanged. (verbatim from ADR-021)

Delivery recovery (re-fetching dropped WS *logs*, ADR-008 Edge-B backfill,
backfill reconciliation) is correct behaviour, in scope. **Scalar-state
repair** — re-writing the sqrt/liquidity/tick a hop solves on, or applying
missed swaps to mutate pool state — is the forbidden auto-heal, out of scope,
unchanged. Quarantine replaces *abort* as the guarantee that the solver never
consumes a divergent hop; it does not touch the divergent state itself.

### D5 — Death remains available, and stays loud where it remains.

`fatal` buckets (and any operator-forced exit) keep ADR-021's loudness
discipline: the grep-able breadcrumb prints byte-identically before exit, the
exception record names kind + class, and the new process-liveness alert
(`DegenbotProcessDown`) closes the scrape-side blind spot (a dead process is
signal — the old `DegenbotHeaderStall` deliberately ignored down targets).
Quarantine reactions emit `degenbot.quarantine.events{scope, cause}` + a
depth gauge (`degenbot.engine.quarantined_pools`) so containment itself is
chartable: "the bot quarantined 3 pools today" is a first-class dashboarding
fact, not a log-line rumor.


## Operator reference

The rendered table above is operationalized in
[docs/failure-policy.md](../failure-policy.md) - the config.toml
[failure_policy] knob (per-bucket action overrides, closed keys,
boot validation), the action vocabulary, and the guidance for
choosing stances.

## Consequences

- **Availability:** a strict-gate desync no longer ends the session. The bot
  quarantines the divergent pool, keeps trading its remaining paths, and the
  repair happens in parallel with revenue. The overnight OOM/abort class
  (FRKBGP) graduates from "bot was dead by morning" to "bot quarantined the
  offending pool at 04:17 and kept trading."
- **Safety invariant preserved by construction:** the solver cannot consume a
  quarantined hop (solve-path resolution excludes it). Trading-on-divergent-
  state — the load-bearing fear behind ADR-021 D1's abort — is impossible in
  any action tier, because a taint *requires* quarantine as part of its
  definition. The invariant moves from "process integrity" to "surface
  integrity".
- **The D63GSE mode lattice is retired before ever governing the tripwire.**
  `failure_policy.rs`'s `FailureMode`/`failure_mode()`/env parsing and its
  seams are removed per the hard-cutover rule; `CooldownRegistry` +
  `record_exception_keyed` survive as the dedup surface.
- **Tripwire semantics:** detection (the strict gate, the anchor model, the
  class discrimination) is untouched; only `trip_and_exit`'s reaction is
  re-routed per the matrix. The `exit` path keeps its byte-identical
  breadcrumb for the sessions where an operator overrides to exit or where a
  fatal bucket fires.
- **Test surface:** the subprocess abort tests gain per-bucket twins (survive +
  quarantine + scrapeable counter for tainted; byte-identical abort for fatal);
  matrix completeness and override validation are unit-tested.
- **Cardinality discipline unchanged:** `kind`/`reason`/`scope`/`cause` labels
  are closed sets from the taxonomy; pool/path identity stays in trace span
  fields and the JSON dump, never metric labels (ADR-021 D2 + `metrics.rs`
  cardinality rule).

## Alternatives considered

- **Keep ADR-021 D1 abort-only, add dashboards around it.** Rejected: alerting
  on one's own availability incident is telemetry for a wake-up, not a fix;
  the overnight dead-bot class remains.
- **Auto-repair on desync (apply missed swaps / re-sync scalars).** Rejected
  for the same load-bearing principle as ADR-021 — it converts a bug detector
  into a bug hider. Not revisited here; D4 keeps the boundary verbatim.
- **Keep a coarse mode *and* the matrix** (mode as a stroke, buckets as
  refinement). Rejected (the 2026-09-04 design review): two channels that can
  contradict each other are worse than either alone; the per-bucket override
  table gives every legitimate operator need at the grain the decision is
  actually made.
- **Resume/heal the quarantine after a cold chain-read re-check (a soft
  quarantine-expiry).** Deferred, deliberately not designed here: re-admitting
  a once-diverged pool needs an evidence threshold (N consecutive honest
  anchors) that risks becoming a silent auto-heal seam. An operator can always
  re-register the pool through the existing registration lifecycle; automation
  of that decision is future work with its own ADR.

## References

- [ADR-021](ADR-021-solver-state-accuracy-is-a-fail-fast-tripwire.md) — the
  superseded posture (D1) and the retained decision core (D2 classes, D4
  never-heal). The tripwire module, verdict types, and env stances live there.
- [ADR-022](ADR-022-registration-verify-lifecycle-core-ownership.md) — the
  registration verify lifecycle `verify_mismatch` maps onto (deny-admission
  containment by omission).
- [ADR-008](ADR-008-block-state-machine.md) — Edge-B delivery backfill; the
  D4 in-scope boundary.
- Epic D63GSE / `failure_policy.rs` — the retired prior art: the
  `exit|harden|continue` scaffold, the `CooldownRegistry`, and
  `record_exception_keyed` (the latter two are the survivors, now serving the
  matrix).
- Ergo epic `DO5Q5E` — the implementation task graph (matrix module, tripwire
  seam, solve-path exclusion, repro artifact, alerts + dashboard surfacing).
