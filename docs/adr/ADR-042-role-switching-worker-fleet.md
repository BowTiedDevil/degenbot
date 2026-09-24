# ADR-042: One role-switching worker fleet — a bounded, budgeted host for every execution resource

**Status: proposed** (2026-09-09, ergo task `ZXSGTX`, epic `GTX`).
Settled in the originating architecture conversation and recorded here so every
fleet task can execute without it. **Implementation is gated on a user sign-off
checkpoint: the role/state table and the budget allocation table in the
[design doc](../architecture/worker-fleet.md) must be approved before fleet-core
work begins.** The canonical, maintained form of the design lives in the design
doc; this ADR records the decisions and the evidence. **The user sign-off checkpoint is satisfied (2026-09-09) — fleet-core (O4CCVX) may begin.**

## Context

The bot carries a *per-era pile of parallelism mechanisms*, each with its own
private sizing rule that independently derives a worker/thread/slot count from
the cgroup budget, and nothing that bounds their sum:

- the **solve-executor fleet** (dedicated multi-thread runtime, host + one
  persistent worker per LPT bin, `cpu_budget::solve_worker_count`);
- the **SimSlots cap** (`solve_sim_inflight`), a global semaphore sized as
  `leftover × 2` independently of everything else;
- the **inline-sim runtime** (a second dedicated multi-thread runtime inside
  the Python-installed sim hook, `sim-hook` workers);
- the **rayon global pool** and the RAYPAR T3 rayon partitions for resolve;
- the **detached merge sidecar** (one std thread);
- the **ambient I/O runtime** (`crate::runtime`, sized from the same
  `cpu_budget` after SMTH6M/VJGZJ2);
- the per-cycle **`arb-sim-*` std-thread fan-out** (~85–105 short-lived
  threads per cycle, 3.3 ms CPU each, mostly parked).

Measured sizing inputs (2026-09-08/09 survey + the post-ADR-041 solve-cycle
tail, task `CIRZHH`; full distributions in `logs/solve-tail-20260908.md`, a
gitignored `logs/` run artifact not in the tracked tree):

- Steady state on an 8-core cgroup quota: instrumented solve+sim CPU is ~1.8%
  of quota; the rayon global pool is **idle** (0.4 s CPU / 1158 s across 6
  threads); the solve-executor fleet does ~12 s CPU/worker over the window;
  6 LPT bins are balanced (bin-thread lifetime CPUs equal within 0.75%).
- The solve cycle tail is bursty, not flat: whole-cycle detached solve span
  avg 208 ms, p95 384 ms, max 480 ms; per-bin walk load ~86 ms vs a ~175 ms
  bin window — bins park about half the window in SimSlots joins. **Per-bin
  makespan is not instrumented** (no bin span/label — a gap this design
  closes). LPT weights are *not* the bottleneck; rebalancing headroom sits in
  sim/join pooling.
- Burst overlap per cycle reaches ~10–11 runnable threads against 8 cores;
  cgroup throttling fires ~2.7 events/min at steady state (0.18% duty) and
  798 events lifetime including registration storms.
- `degenbot.cgroup.throttled` exists as a metric with **no consumer**.
- Quota consumer census: a worker census registry (epic `FPNT36` / `PE4FPM`)
  is queued so every execution resource self-registers (name/count/sizing
  rule) with distinct thread names and metric export.

Demand is bursty and role-shifting; pools are static. A fleet in which a
worker's *role* is the scheduling unit — bounded by one budget authority —
replaces per-era mechanisms with one host.

## Decision

### 1. One fleet host in a new crate: `degenbot-workers`

A new standalone crate `rust/crates/engine/degenbot-workers` owns the fleet host, the
`WorkerRole` state machine, and the budget authority. It depends on
`degenbot-core` (cgroup detection) and *nothing engine-specific*; `degenbot-bot`
plugs roles in as closures/tasks. It must be lower in the dependency graph
than the engine because future roles (pool-state updater, registrar, verifier,
submitter) are bot-state-level, not `arb_engine` concerns — and the umbrella
`degenbot` crate re-exports it, so a pure-Rust consumer gets the fleet.

### 2. `WorkerRole` — an enum FSM with a legal-transitions table

Roles are units of work a fleet worker can lease; worker slots are the
persistent resources. The FSM, its sized `ALL_ROLES` const, and the legal-
transition table (mirroring `stage_handlers::ALL_STAGES` and its conformance
stub) are specified in the design doc. Per-repo style: an enum-based state
machine, not ad-hoc rules.

The v1 role set — **decided, not deferred**:

| Role | v1? | Absorbs |
|---|---|---|
| `Solver` (per-LPT-bin pin) | yes | the solve-executor runtime's per-bin pinned jobs |
| `SimDriver` (slot pool) | yes | SimSlots pipelined drivers **and** the `arb-sim-*` per-sim spawn (the first pooling candidate — folding it in is cheap now and removes the per-cycle thread storm) |
| `Resolve` | yes | the rayon persistent-pool partitions (RAYPAR T3 walk semantics retained; the idle rayon *global* pool retires at cutover) |
| `Merge` | yes | the detached merge sidecar |
| `PoolStateUpdater`, `Registrar`, `Verifier`, `Submitter` | declared, not v1 | the roles are named in the enum's parent set and their shed/cordon class is defined now, so adding them is an entry, not a redesign |

Python/FFI placement is part of the decision: simulation **never** round-trips
Python. The FFI is crossed only for runtime/startup concerns (installing the
sim closure, config, budget echo) and to deliver standalone results to
consumers. The inline-sim hook's private runtime becomes fleet-hosted
`SimDriver` workers; the closure stays Rust end to end.

### 3. One budget authority bounding the SUM

`FleetBudget` (in `degenbot-workers`, sourcing `degenbot-core::cpu_budget`
detection) is the single authority: every consumer — ambient I/O runtime,
resolve, solver pins, merge, sim slots, and the reserve for Python/pump/
OTel — declares a peak-CPU share and a thread count, and the fleet refuses to
start if the declared shares exceed the quota. Today each pool derives its own
count and the *sum* is bounded only by luck; the burst evidence (10–11
runnable vs 8) is that luck running out.

Fractional-quota ceil policy — reviewed here, verdict recorded in the design
doc: detection keeps `v2_quota_cpus`' ceil (a worker that exists must be
schedulable), but the allocation arithmetic floors: integer shares sum against
`floor(Q)` and the fractional remainder is spendable only by I/O-dominant
consumers (sim slots, ambient I/O), whose measured duty is partial.

### 4. Throttle reaction: a `Nominal ⇄ Cordoned` posture FSM

`degenbot.cgroup.throttled` gets a consumer: on throttle onset the fleet
cordons (no *new* leases for deferrable/background roles, sim-slot intake
throttled), never sheds a running unit, never strands a result pipe, and
hysteresis-exits after a clean window. Full policy in the design doc. **Threshold tuning (sign-off amendment 2026-09-09):** the enter/exit thresholds and hysteresis windows are typed config keys, runtime-adjustable through the operator channel (wired: op `set_fleet_posture` + `degenbot fleet posture set`, design doc §6), and calibrated from captured soak data — the posture feeds back into its own thresholds; share arithmetic (design doc section 5) is outside this authority.

### 5. RAYPAR T3 and the deadlock ledger carry over

Per-bin worker pinning, warm L1/L2 and allocator arenas, no split/steal, sim
> solve precedence, and the deadlock ledger — no scoped-rayon join under a
held `parking_lot` guard; merge pipe never stranded; loud `abort` on executor
death — are carried over verbatim (design doc §10). Pinning is job→bin task
affinity, which survives role switching by construction: a `Pinned(Solver,
bin k)` slot is leased only to bin k, across cycles, until an explicit
epoch-boundary rebalance.

**Tokio fact-check (Q8d-1, verified against tokio 1.52/1.53 source):** tokio's multi-thread runtime is work-stealing by design and exposes **no task-to-worker affinity API** (the source tree contains none); its recommendation for CPU-bound work is spawn_blocking or a separate pool — precisely the two-runtime split this ADR keeps. Stealing only moves *runnable* tasks between workers at yield/wake boundaries, so fleet bin units (which never yield mid-unit) cannot be migrated by tokio, while pooled SimDriver/Resolve units remain freely stealable — tokio's good default, not disabled by the fleet. Conclusion: bin pinning must be owned above tokio (as designed); the no-work-stealing non-goal covers fleet-level stealing across bin units, not fighting tokio's scheduler.

### 6. Conformance harness: `NoopStubFleetHost`

A `NoopStubEngine`-style executable spec (mirroring `stage_handlers::ALL_STAGES`
and its u8-indexed conformance stub) is test-declared only: it walks every
role through every legal transition, asserts the budget-sum invariant across a
scripted quota resize, asserts pin/arena stability across cycles, and fires
the stranded-pipe tripwire. Never runtime-selectable.

### 7. The Q&A decision record

| Q | Decision |
|---|---|
| Q1 | **Crate home:** new `degenbot-workers` crate, engine-agnostic, below `degenbot-bot` |
| Q2 | **v1 roles:** Solver, SimDriver (incl. folding the `arb-sim-*` spawn in), Resolve, Merge; future roles declared in the enum's parent set now |
| Q3 | **Pinning/arenas:** job→bin affinity keyed by bin id; pinned slots survive cycles and role switches; re-pin only at epoch boundary on quota change |
| Q4 | **Bin telemetry:** `arb.solve.bin` span per bin with bin index and makespan histogram — closes the per-bin makespan gap; every fleet worker self-registers with the census registry (`FPNT36`) |
| Q5 | **Python/FFI:** cross only for runtime/startup and standalone-result delivery; simulation never round-trips Python; the inline-sim runtime is fleet-hosted |
| Q6 | **Migration:** `DEGENBOT_FLEET` stance flag (`legacy`/`fleet`), parallel implementations during migration with pinned tests passing in both stances, hard cutover deleting the legacy mechanisms at the end — per the repository switch-over policy (0.6 alpha; no back-compat layer) |

## Consequences

- **The quota has one authority.** Oversubscription by independent sizing
  becomes a startup failure (fail-loud), not a runtime throttle storm.
- **The thread storm leaves.** ~85–105 per-cycle `arb-sim-*` spawns become
  pooled `SimDriver` leases on warm workers.
- **The idle mechanisms retire at cutover** — the rayon global pool, the
  per-mechanism derivation rules, the sim hook's private runtime.
- **Telemetry gains the missing dimension.** Per-bin makespan and fleet posture
  counters land with the fleet; sizing from the tail (not the mean) becomes
  measurable.
- **Cost: one more crate and a stance flag during migration.** The flag is
  deleted at cutover; only one implementation survives it.

## Non-goals

- **No new scheduling machinery beyond role dispatch.** No work-stealing
  re-introduction, no io_uring, no executor replace: the Tokio CPU/I-O split
  and RAYPAR T3 semantics are retained, hosted.
- **No auto-tuning of shares.** Budget shares are declared, logged, and overridden by
  config; they are not heuristically re-derived at runtime. Posture *thresholds* are excluded from this per the sign-off amendment: they stay data-tunable (runtime adjustment via the operator channel + soak-capture feedback, design doc section 6) and never touch shares. Share auto-tuning itself is neither ruled in nor out (Q8d-3, data-gated): v1 keeps shares declared and config-static, revisited as a follow-up once fleet telemetry (per-role busy/idle, census, throttle duty) supplies the evidence.
- **No Python-visible fleet API.** The FFI surface is unchanged except for the
  sim-closure install and result delivery that already exist.
- **No backwards-compatibility layer.** Legacy mechanisms are deleted at
  cutover, mirroring ADR-041's seam retirement; the 0.7 kill list
  (ADR-010/011) is untouched.

---
*User checkpoint: sign off on the role/state table and the budget allocation
Table in [worker-fleet.md](../architecture/worker-fleet.md) before fleet-core
implementation begins.*
