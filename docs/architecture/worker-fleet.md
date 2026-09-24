# The role-switching worker fleet (`degenbot-workers`)

Ergo task `ZXSGTX` (epic `GTX`) · decision record: [ADR-042](../adr/ADR-042-role-switching-worker-fleet.md) · sizing evidence: 2026-09-08/09 survey + solve-cycle tail (task `CIRZHH`, `logs/solve-tail-20260908.md` — a gitignored
`logs/` run artifact)

> **Proposed.** The role/state table (§3) and the budget allocation table (§5)
> are the sign-off checkpoint; implementation of fleet-core is gated on user
> approval of both. This doc is the working design reference the fleet tasks
> build against.

This is the design reference for one generalized, bounded worker fleet whose
workers **switch roles** — hosting every execution resource the bot needs —
replacing the per-era pile of parallelism mechanisms (solve-executor runtime,
SimSlots, the inline-sim hook's private runtime, rayon partitions, the
`arb-sim-*` per-sim spawn, the merge sidecar), each of which independently
derives its own sizing and nothing bounds their sum.

## 1. Sizing evidence (what the fleet must fix)

Measured on the live dry-run bot, 8-core cgroup quota:

| Signal | Value | Implication |
|---|---|---|
| Solve+sim CPU at steady state | ~1.8% of quota | fleet is sized for bursts, not means |
| Rayon global pool | idle (0.4 s CPU / 1158 s, 6 threads) | retires at cutover |
| Solve-executor fleet | ~12 s CPU/worker over window; 6 balanced bins (lifetime CPU ±0.75%) | pinning works; keep RAYPAR T3 |
| Cycle tail | avg 208 ms, p95 384 ms, max 480 ms (n=333) | tail, not mean, drives sizing |
| Bin walk load vs bin window | ~86 ms vs ~175 ms — bins park ~half the window in SimSlots joins | rebalance headroom is in sim pooling, not LPT weights |
| Per-bin makespan | **not instrumented** | this design adds bin spans (§7) |
| `arb-sim-*` spawn | ~85–105 threads/cycle, 3.3 ms CPU each | first pooling candidate |
| Burst overlap | ~10–11 runnable threads vs 8 | today's SUM is unbounded; §5 fixes it |
| Throttle | ~2.7 events/min steady (0.18% duty), 798 lifetime | §6 wires the reaction |
| `degenbot.cgroup.throttled` | metric exists, no consumer | consumed by the posture FSM (§6) |

## 2. Decisions at a glance

1. **Crate home:** new standalone crate `rust/crates/engine/degenbot-workers`; depends
   on `degenbot-core` (cgroup detection) only for budget input. Engine-agnostic:
   `degenbot-bot` plugs roles in; future roles (pool-state updater, registrar,
   verifier, submitter) must live below the engine, which rules out an
   `arb_engine` module. The umbrella crate re-exports it (pure-Rust consumers
   get a fully functional fleet).
2. **FSM, not ad-hoc rules:** one `WorkerRole` enum + worker-slot state machine
   with a legal-transitions table, mirroring the `stage_handlers::ALL_STAGES`
   sized-const + conformance-stub style.
3. **One budget authority** (`FleetBudget`) declaring and bounding the SUM of
   every consumer's peak share to the cgroup quota.
4. **Throttle reaction:** a `Nominal ⇄ Cordoned` posture FSM consuming
   `degenbot.cgroup.throttled`.
5. **Python/FFI rule:** cross only for runtime/startup concerns and standalone-
   result delivery; **simulation never round-trips Python**; the inline-sim
   hook's runtime becomes fleet-hosted.
6. **Migration:** `DEGENBOT_FLEET` stance flag and parallel implementations
   shipped during 0.6; the hard cutover LANDED at LW-T9 (ergo CQLMM2): the
   stance flag and the legacy tokio-stance mechanisms are deleted — **fleet
   is the only stance since LW-T9**. A surviving `DEGENBOT_FLEET` env var or
   `fleet.stance` TOML key fails the config load loudly for one release.

## 3. `WorkerRole` — the role/state/cycle table

### 3.1 The role enum

```rust
/// Work a fleet worker slot can be leased for. Sized const + conformance
/// stub mirror `stage_handlers::ALL_STAGES` (idiomatic per the repo FSM rule).
pub enum WorkerRole {
    /// One persistent pin per LPT bin (RAYPAR T3). Pinned by bin key;
    /// warm L1/L2 + allocator arenas survive across cycles.
    Solver,
    /// Pipelined inline sims behind the slot pool. Absorbs SimSlots' drivers
    /// and the per-cycle `arb-sim-*` spawn (the first pooling candidate).
    SimDriver,
    /// Path resolution (the rayon partition consumers). I/O-adjacent CPU work.
    Resolve,
    /// The detached merge sidecar: drains the result pipe so per-path sends
    /// land in a pipe somebody drinks from. Pinned (exactly one).
    Merge,
    // ---- declared now, implemented after v1 (adding a variant is an entry,
    // ---- not a redesign; each names its cordon class below) ----------
    /// Pool-state update application (deferrable: sheds artifact-free).
    PoolStateUpdater,
    /// Registration verify-lifecycle driver (deferrable).
    Registrar,
    /// Published-edge verification reads (deferrable).
    Verifier,
    /// Settlement submission delivery (never deferrable: latency-critical).
    Submitter,
}
```

`pub const ALL_ROLES: [WorkerRole; 8]` is the sized list (v1-active roles:
`Solver`, `SimDriver`, `Resolve`, `Merge`; the remainder are declared, with
`Known`/planned gating in dispatch). The conformance stub indexes by position
into `ALL_ROLES` exactly as the `StageHandlers` stub does.

### 3.2 Worker-slot states

A worker **slot** is a persistent host resource (a thread/booted task). State
per slot:

```
Idle → Leased → Running → (Pinned | Idle) ;  Running → Draining → Idle
```

| State | Meaning |
|---|---|
| `Idle` | parked; no lease; zero CPU |
| `Leased(role)` | dispatch granted a unit of work (or a pin claim) |
| `Running(role, unit)` | executing the unit |
| `Pinned(role, key)` | steady lease held **across cycles** (Solver per bin, Merge); `Running → Pinned` on unit completion, warm |
| `Draining(role)` | finishing its in-flight unit under shed/cordon; takes nothing new |
| (posture) `Cordoned` | process-level fleet posture (§6); not a slot state — gates transitions |

### 3.3 Legal transitions

| # | From | To | Guard / trigger |
|---|---|---|---|
| T1 | `Idle` | `Leased(r)` | dispatchable(role) ∧ free capacity for r ∧ posture admits r (cordon blocks deferrable r) |
| T2 | `Leased(r)` | `Running(r, unit)` | worker dequeues the unit / claims the pin |
| T3 | `Running(Solver, bin k)` | `Pinned(Solver, k)` | unit complete; pin survives for the next cycle's unit (warm arenas intact) |
| T4 | `Running(Merge)` | `Pinned(Merge)` | exactly one merge pin; pipe drain handoff verified |
| T5 | `Running(SimDriver \| Resolve)` | `Idle` | unit complete; back to the pooled set |
| T6 | `Pinned(r, k)` | `Running(r, k)` | next cycle's unit for the same key (only `-same key-`: pin is keyed) |
| T7 | `Running(r)` | `Draining(r)` | cordon onset on a deferrable r, or fleet resize mid-cycle — the **in-flight unit always completes** |
| T8 | `Draining(r)` | `Idle` | in-flight unit done; nothing taken |
| T9 | `Pinned(r)` | `Idle` | explicit rebalance (quota change / config override) — **epoch boundary only**, never mid-cycle |

Illegal (the FSM must reject, and the conformance stub asserts rejection):
`Idle → Running` (lease required); `Running → Idle` mid-unit; `Pinned(Solver,
k) → Pinned(Solver, k')` (re-keying is T9 then T1); any `→ Running` under
cordon for a cordon-class-deferrable role; `Draining → Leased`.

### 3.4 How pinning/arenas survive role switching

Pin affinity is **job → bin key**, not worker-thread identity: a `Pinned`
slot's lease *is* the key. Role switching happens between units at cycle
boundaries; while pinned, a slot takes no other role (that is what makes the
warm L1/L2 and allocator arenas meaningful — RAYPAR T3 "no splitting, no
stealing"). Re-pinning only occurs via T9 at an epoch boundary, e.g. on a
quota change altering the pin count; pins release before slots re-lease, so
an arena is never live across a role switch.

**Elastic repinning (data-driven option, sign-off amendment):** v1 ships T9-only (epoch-boundary re-pin). If bin-makespan telemetry (section 7) shows systematic pinned-parked waste — slots pinned to bins that no longer earn their share — an elastic variant (quota-aware re-key at cycle boundaries) may be proposed as a follow-up amendment with its own sign-off; it must preserve release-before-re-lease so arenas never cross a role switch.

## 4. Priority and queue semantics

- **sim > solve precedence.** A queued sim unit preempts *queue position*, not
  a running walk: the sim dispatch queue is drained by pooled `SimDriver`
  leases before new `Solver` queue intake when both contend for free slots.
  (In-flight walks are never preempted — walks are pinned units.)
- **Per-role queues, one dispatcher.** Each role has its own bounded queue;
  one dispatch loop grants leases in precedence order: `Solver` pins first
  (they are cycle-critical), then queued `SimDriver` units (slot-pool
  permitting), then `Resolve` chunks, with `Merge` always pinned and never
  queued.
- **Slot pool vs core shares:** thread/slot counts may exceed a role's CPU
  share for I/O-dominant roles (`SimDriver` slots are mostly RPC/storage
  awaits — measured 3.3 ms CPU/sim); the CPU *share* is what the budget
  authority bounds (§5).
- **Bounded queues, loud overflow.** A full queue fails loudly (ADR-021
  posture: classify, stop loudly, never silently drop). Backpressure is the
  budget authority's job, not silent loss.

## 5. Budget allocation — one authority bounding the SUM

`FleetBudget::derive(Q)` (Q = `cpu_budget::effective_cpu_budget()`) is the
**single authority**. Every consumer declares `(peak_cpus, thread_count)`;
override via typed config / `DEGENBOT_*` env (terminal, as today). Startup
**fails loudly** if the declared peak shares exceed the quota — oversubscription
is a configuration bug surfaced at boot, not a runtime throttle storm.

Worked allocation at Q = 8 (the current deployed case):

| Consumer | Declared peak (cores) | Threads/slots | Rule |
|---|---|---|---|
| Reserve `H` (Python bridge, pump, OTel, async GC) | 1 | — | fixed; the fleet must never starve I/O |
| Ambient I/O runtime `A` | 2 | 2 | `max(1, floor((Q−H)/4))`, override `DEGENBOT_IO_WORKERS` |
| Resolve `R` | 1 | 1 | fixed v1 (12.4 ms/cycle measured) |
| Merge `M` | 1 | 1 | exactly one sidecar |
| Solver pins `S` | 3 | 6 pins (2:1 over-subscription of *parked* pin wait; concurrent walk admission = 3) | `S = Q − H − A − R − M`; ≥ 2 or fail-fast |
| SimDriver slots (duty-counted) | fractional remainder only | ≤ 4 (today's SimSlots cap) | I/O-dominant: measured duty ≈ 0.07 cores sustained; slots oversubscribe ×2 of fractional/idle headroom, cap preserved |
| **SUM** | **8** | | `H + A + R + M + S = Q` (floor; see below) |

Notes:

- **Pins are threads, shares are cores.** Six pins exist (one per LPT bin —
  structural, from the partition, not from the budget); at most `S` bin walks
  are *runnable* concurrently (admission-gated walk start; a gated bin parks —
  exactly what bins already do half their window waiting on sim joins, so
  makespan impact is bounded well under the current ~175 ms window).
- **Fractional-quota ceil policy (reviewed):** detection keeps
  `v2_quota_cpus`' ceil (an existing worker must be schedulable). Allocation
  arithmetic **floors**: integer core shares sum against `floor(Q)`; the
  fractional remainder (e.g. the 0.5 of a 4.5-core quota) is spendable only by
  I/O-dominant consumers (SimDriver slots, ambient I/O), whose measured duty
  is partial-core by construction. A fractional quota below `sum(H, A, R, M)
  + 2` fails fast — the fleet cannot host the two pinned latency roles there.
- **Overrides are terminal** (same rule as `DEGENBOT_SOLVE_CPUS` today): a
  configured value wins, is logged at startup, and participates in the same
  sum check.
- **Counts, not shares, are what varies under cordon** (§6): shares are static
  for the process lifetime unless the quota itself changes (re-detected on
  cgroup file focus; a change is a posture event, logged).

## 6. Throttle-reaction policy — the `Cordon` posture FSM

`degenbot.cgroup.throttled` (and `cpu_budget::cgroup_throttle_delta`) get
their first consumer: a process-level fleet posture.

```
Nominal ⇄ Cordoned
```

- **Enter** (any, with hysteresis): ≥ 2 throttle events in a rolling 1 s
  window; or throttled-time duty > 2% over a trailing 5 s window. Entering is
  loud: posture transition span + counter increment (not a silent degrade).
- **Exit:** 10 s of clean windows — hysteresis prevents flapping.
- **Threshold tuning & runtime feedback (sign-off amendment 2026-09-09):** the enter triggers (event count/window, duty percent), exit window, and cordon effects (e.g. the sim-intake floor) are typed config keys at boot, runtime-adjustable via the operator channel (wired — the re-tune channel below), and calibrated from captured soak data; posture-transition metrics (enter/exit counts, cordoned dwell, intake suppression) are exported so thresholds are tuned against measurements rather than heuristics. This authority never touches share arithmetic (section 5).
- **Cordon effects (v1):** (a) no *new* leases for cordon-deferrable roles —
  in v1 that set is empty among the four active roles, so the operative
  effects are (b) sim-slot *intake* throttled (new leases floored at half the
  slot cap; in-flight sims never cancelled) and (c) intake for declared
  background roles (PoolStateUpdater/Registrar/Verifier) held, since their
  shed class is defined now; (d) **pinned walks are never shed mid-unit**, and
  (e) the merge pin and ambient I/O runtime are never cordoned.
- **The deadlock ledger applies in cordon:** shed never abandons a unit whose
  results feed a pipe; `Draining` always runs to completion (T7/T8).

**Soak ruling (7OGY5V, 2026-09-10): Solver admission is posture-INVARIANT.**
LW-T5 (MOVE3D, Seam E) briefly introduced a submit-seam gate refusing Solver
bins under `Cordoned`; the first in-container soak after the LW-T9 cutover
found the correctness hole: the bin had been issued before the refusal, the
result pipe stranded, and the cycle-abort killed the bot on ordinary cgroup
throttling. The gate contravened this section (cordons hold only Deferrable
intake + the sim-intake floor; pinned walks are never shed) and
`workers::role`'s `CordonClass::Never` table for Solver — the gate was
removed, the LW-T5-era seam test rewritten to pin posture-invariant
admission, and the submit-seam posture mirror retired with it.

Posture transitions are metrics, not behavior changes to the engine: stages,
priority, and correctness are posture-invariant; only lease intake changes.

**Runtime re-tune channel (JCI2FW Part B, wired):** the six typed keys are
adjustable on a LIVE process through the operator channel — op
`set_fleet_posture` (a partial patch over `cordon_enter_events`,
`cordon_enter_window_ms`, `cordon_duty_percent`, `cordon_duty_window_ms`,
`cordon_exit_clean_ms`, `cordon_sim_intake_floor`; at least one key required,
absent keys keep the live value, `cordon_sim_intake_floor: null` restores
half the slot cap) and the read-only `get_fleet_posture`, both behind the
`degenbot fleet posture set|show` CLI. Validation lives ONCE in the Rust
core (`PosturePolicyPatch::validate`: windows > 0 ms, duty percent in
(0.0, 100.0], enter events >= 1, floor >= 1 when set) and REJECTS with the
typed `degenbot.fleet.PostureRetuneError` — never clamps; the op layer adds
the unknown-key/empty-patch wire checks in front of it. The write path is
the PyO3 verb -> `PostureOwner::retune` (the atomic policy swap that keeps
posture state and the trailing sample window); the response echoes the
EFFECTIVE policy (all six fields + the current `Nominal|Cordoned` posture),
and every change emits ONE loud `tracing::warn!`
`[fleet-posture] operator retune` line listing old -> new per changed key.
Boot config stays the default source: the re-tune applies to the running
process only.

## 7. Instrumentation: bin spans + the census registry

- **`arb.solve.bin` span** per bin per cycle, attrs: `bin` index, `paths`,
  `walk_ms`; a `degenbot_fleet_bin_makespan_seconds` histogram. This closes
  the measured gap ("per-bin makespan is not instrumented") and turns
  sim-join-overlay analysis from derived arithmetic into direct reads.
- **Census registration:** every fleet worker slot self-registers with the
  worker-census registry (epic `FPNT36`/`PE4FPM`): name, role, count, sizing
  rule, thread name (`work-<host>-<role>-<n>` style, distinct and greppable),
  with metric export. The `arb-sim-*` ad-hoc thread names disappear at
  cutover with their spawn site.
- `tracing` carries the epoch on fleet spans/metrics per ADR-041's invariant
  (Jaeger/Grafana tell the migration story directly).

## 8. The Python/FFI boundary rule (hard constraint)

Cross the FFI only for:

1. **runtime/startup concerns** — installing the sim closure
   (`install_inline_simulator`), config, and a budget echo for the Python
   driver's log line; and
2. **delivery of standalone results** to consumers (result bridge). 

Simulation **never round-trips Python**: the `SimDriver` role hosts the sim
body; the inline-sim hook's *private dedicated runtime* is replaced by the
fleet-hosted slot pool, and the hook retains only its (Rust, pure) simulation
closure and config. The `WrapDatabaseAsync` runtime-capture caveat (its
`Handle::try_current()` at build time + `block_in_place` escalation) is
resolved by hosting sims on fleet `SimDriver` workers booted *inside* the
fleet's runtime — the worker is the ambient runtime for the sim, so the
capture succeeds and no per-call runtime is built (VJGZJ2's rule retained).

## 9. Migration plan

Stance flag `DEGENBOT_FLEET` (`legacy` | `fleet`; typed-config alias) shipped
during the migration; **RETIRED at LW-T9 (fleet is the only stance since
LW-T9)** — the flag fails the config load loudly for one release, and the
legacy mechanisms it selected are deleted (§11 for the cutover ops notes).

| Task | Content | Gate |
|---|---|---|
| F1 | crate skeleton: `WorkerRole`, states, `ALL_ROLES`, transition table, `NoopStubFleetHost` conformance stub | conformance tests green |
| F2 | `FleetBudget` authority; startup sum check + quota re-detection hook; census registration seam | config/env + failure-mode tests |
| F3 | `Solver` + `Merge` hosting behind the flag (port solve-executor jobs; keep the module ledger) | existing solve pipes' pinned tests pass in both stances |
| F4 | `SimDriver` hosting (retires SimSlots' private drivers, the sim hook's private runtime, and the `arb-sim-*` spawn) | sim soak A/B: tail p95 no regression |
| F5 | `Resolve` hosting (retire the rayon global pool; keep RAYPAR T3 walk semantics) | resolve parity + capture-replay regression |
| F6 | Cordon posture FSM + `arb.solve.bin` spans + posture metrics | metric visibility in Grafana (synced from repo) |
| F7 | Soak (Jaeger/Grafana) + parity gate, then **hard cutover**: stance flag deleted, legacy mechanisms deleted (solve-executor as a separate mechanism, SimSlots sizing derivation from leftover, rayon global pool, sim-hook private runtime, `arb-sim-*` spawn) | switch-over policy: alpha, breaking ok; no back-compat layer |

The 0.7 ADR-010/011 kill list is untouched.

## 10. Deadlock ledger (carried over verbatim)

1. **No scoped-rayon join under a held `parking_lot` guard**
   (solve_executor.rs module docs). Fleet hosting keeps this: a scope-taker
   must hold no lock that the scope's workers need — the fleet's queues are
   lock-free or guard-free at the lease edge.
2. **Bins pin workers**: RAYPAR T3 semantics — no splitting, no stealing; a
   bin is one pinned unit on one slot; affinity is keyed by bin id (§3.4).
3. **The merge pipe is never stranded.** A dead executor would deadlock the
   first solve — its per-path sends would land in a pipe nobody drains — so
   swallowing the error is never an option: the loud `abort_executor`
   discipline (log at `error` + `std::process::abort`) carries over to the
   fleet host. Any fleet path that abandons a unit with in-flight result sends
   trips the same alarm.

## 11. Conformance harness: `NoopStubFleetHost`

Mirror of `NoopStubEngine` (ADR-041) at fleet scope — **test-declared only**
(`#[cfg(test)]` in `degenbot-workers`), never runtime-selectable:

- a scripted host that walks every role in `ALL_ROLES` through every legal
  transition (u8-indexed into the sized const, exactly like the stage stub),
  asserting each illegal transition is rejected;
- asserts the budget-sum invariant across a scripted quota resize (shares
  re-declared, sum re-checked, pin re-key possible only via T9);
- asserts pin/arena stability across N synthetic cycles (same pin key, warm
  handle identity); and
- exercises the stranded-pipe tripwire: a host that "dies" mid-drain must hit
  the loud-abort path.

Its u8 script is also the review artifact: any new role or transition lands in
the table or the stub fails loudly.

## 12. Non-goals

- **No auto-tuning of shares** at runtime: they are declared, logged, and
  overridden by config; re-derivation happens only on quota re-detection
  (a posture/logged event), and cordon adjusts *intake*, not shares. Posture *thresholds* are separately tunable (section 6, sign-off amendment) and never touch shares.
- **No work-stealing revival**; the Tokio CPU/I-O split and RAYPAR T3 are
  retained and hosted, not replaced. (Tokio fact-check, Q8d-1, verified 1.52/1.53:
  the multi-thread runtime work-steals by default and has no affinity API; stealing
  moves runnable tasks at yield/wake boundaries, so never-yielding bin units are
  immune by construction and pooled units stay freely stealable — desired.)
- **No Python-visible fleet API.** The FFI surface is unchanged except for the
  sim-closure install and result delivery that already exist.
- **No migration mechanics beyond the flag:** no soft handoff of in-flight
  units between stances (a stance switch is a restart-boundary config choice).

## 11. Ops note: fuse trips, loud stops, and mutex semantics (ergo CQLMM2)

The solve-path exactness fuses (QR3NUS: one path outcome exactly once — the
in-cycle drain's outcome ledger AND the detached merge sidecar's
seen-(cycle_seq, pid) ledger, carried by LW-T9 note (a)) trip LOUD, never
silent: a dup or an undercount logs a `tripping-the-fuse` error and aborts the
process (ADR-021 loud-stop discipline).

- **Tripped fuse = cycle panic.** Under the FFI it surfaces as the cycle
  thread's panic; the bin/pipe that double-emitted is the bug. Do not retry the
  same process region blindly.
- **No poisoned-Mutex recovery exists** — and none is needed: the engine and
  engine-stages mutexes are `parking_lot` (non-poisoning). There is no poisoned
  guard to recover; the loud stop IS the recovery contract. Restart the process
  (the supervisor's loud-stop restart path) after the dump is captured.
- **Soak gate (65GTJG-mirrored, host-side):** the 5-minute live soak plus
  capture-replay parity runs post-merge on the host machine (home-only caches;
  not in the container). Expect equal probe expectations: capture-replay parity
  green, no solve-tail regression, census rows sane (`arb_sim_workers` /
  `detached_merge_sidecar` / `sim_slots` rows are retired with LW-T9 — their
  presence in a post-T9 dump is itself a failure).

## 13. Lanes and bindings (FLEETFLOOR FF-T3)

Lanes are **logical**: the lane vocabulary names who owns which receipts and
ledger writes, never which thread runs them. A **binding** is the adapter that
maps lanes to threads. One lane interface; two bindings (the no-third-binding
rule holds until a forcing function demands one):

- **pinned** — today's topology, exactly: one dedicated thread per seat
  (`work-fleet-sim-{n}` / `work-fleet-poolupd-{n}` / the keyed solver seats),
  6+ core hosts (the pinned-role floor), the boot-frozen `SlotLayout`.
- **serial** — the 2-5 core arm (FF-T4, `Z6XTDX`): one cycle thread per
  host over the SAME queue and `HostPump` — the named seat
  `work-fleet-serial-0` runs every granted unit of the host's role in
  grant order (reserve -> resolve -> solve -> merge in order), and the
  solve host's keyed-mailbox construction runs the projection's ONE
  solver seat (`serial-0`). It is a `SeatSink` + ONE grant lane over the
  SAME `HostPump` — not a new lane interface. Intake stays the §10
  never-drop shape (no second waiting policy — the 6HE6RF amendment);
  saturation is the advisory queue depth, named and metered through the
  census's `logical` rows.

The lane-to-thread seam lives in two pieces, both already ONE shape
(`seat_host.rs`): `HostPump` (admission + backlog drain + grant loop — the
host-message triple every fleet host runs) and `SeatSink` (the per-host-kind
seat model: the pooled `WorkQueue` vs the solve host's per-seat keyed
mailboxes). A binding *instantiates* these seams per the boot plan
(`degenbot-workers` `plan.rs`, `fleetplan/1`): the executor boots gate on
`host.plan().binding` — `Pinned` runs today's instantiation verbatim, and the
`Serial` arm instantiates the one-cycle-thread seat model (FF-T4). A
forced pinned profile on a sub-floor host runs the marked oversubscription
(`plan.oversubscribed`), never a silent narrow.

**Lane ownership (receipts and ledger writes):**

| Lane | Owns |
|---|---|
| H reserve | the stage-machine rows; no fleet receipts |
| A ambient | pump/dispatch/delivery on `degenbot-io-rt-{n}`; no per-unit receipts |
| R resolve | pooled units' slot-FSM completions; no caller pipes |
| M merge | EVERY path's terminal send — the QR3NUS exactness fuse (solved + suppressed + failed == submitted) is enforced at the merge drain, per cycle |
| PoolStateUpdater | each intake unit's receipt (the awaiting caller's join), held in the unbounded §10 backlog under cordon — never dropped |
| SimDriver | each sim request's per-request receipt channel, admitted under the cordon sim-intake floor |
| Solver seats | each bin's per-path result sends into the merge pipe through the lane witness (the one-outcome-per-path ledger; a panicked bin's undelivered pids arrive typed as `Failed`) |

The outcome ledger, intake receipts, and the exactness fuse are
**binding-independent by construction**: a binding changes which threads run
the lanes, never the ownership. Parity across bindings is the promotion gate
(the pinned path is pinned by the executor suites + the LW-T7 golden replay
in CI; the serial arm rides the same corpus — FF-T5 folds the
profile-parametrized replay).

The worker census prints the lane-to-thread binding per entry (`binding`
field, FF-T2): `pinned` = dedicated seat threads (the fleet roles under the
pinned binding), `shared` = pooled runtimes (the ambient I/O runtime, the
inline-sim runtime), `logical` = a lane riding other threads' time (hoisted
capacities; the fleet roles become logical lanes under the serial binding).

## 14. The tier table and the cutover (FF-T5, NT7HJC)

"auto" resolves host tiers EVERYWHERE via the plan ("degenbot-workers
plan.rs", fleetplan/1 - one pure function of the budget; no call site
decides a tier on its own). The tiers:

| Host cores | "auto" resolves | What runs | Notes |
|---|---|---|---|
| < 2 | refused | nothing | BelowHostFloor - no tier can host the fleet sum; a typed boot error, the process survives (FF-T1) |
| 2 - 5 | serial | one named cycle seat per host (work-fleet-serial-0) over the same queue and HostPump; the solve host's keyed-mailbox construction runs the projection's ONE solver seat (serial-0) | the census rows print logical; carries the QuotaTooSmallForPinnedRoles it fell from (tier_refused); the production alert fires (degenbot_fleet_profile{binding="serial"}) |
| >= 6 | pinned | today's topology: one dedicated thread per seat (work-fleet-sim-{n} / work-fleet-poolupd-{n} / the keyed solver seats) | the boot-frozen SlotLayout; the promotion gate |
| forced pinned (any >= 2) | pinned | the pinned seats and fan-out, **marked oversubscribed** on sub-floor hosts (plan.oversubscribed) | the operator override is honored, never a silent narrow; on sub-floor hosts tier_refused names the overridden pinned floor (QuotaTooSmallForPinnedRoles) |
| forced serial (any >= 2) | serial | the serial seats | ditto |

**"degenbot.runtime_status()"** returns the live view: the plan
(fleet_booted, profile, quota_cpus, binding, oversubscribed,
tier_refused), the projected budget (the seat/share table), and the
census rows (with the lane-to-thread binding per resource).
Pre-construction it is the live default-profile projection
(fleet_booted: false).

**The metric**: degenbot_fleet_profile{profile,binding,oversubscribed}
(one series, value 1) - the ops alerting reads binding="serial" (a
production host on the small-host tier is a degradation signal, never a
silent narrow).

**The pin view (post-DNZQ5G)**: the pin table's single source of truth is
the slot table's SlotState::Pinned cells, rendered by pinned_slots in
slot-index order - the hand-maintained mirror is gone; the derived
renderer IS the representation.

**The binary loud-exit mapping (FF-T1)**: the BOOT-REFUSAL family
(BelowHostFloor, QuotaTooSmallForPinnedRoles under a forced profile
that cannot host it) is typed and sticky in the library - the process
survives; the degenbot binary maps the surfaced BootRefused to its
named exit. The RUNTIME strand aborts (seat/host thread spawn, enqueue
refusal, completion refusal, the closed-channel close arm) keep
abort_executor and their byte-pinned wording - ADR-040 fatal bucket, by
design.

**Lane-death terminal receipts (FF-T4)**: a lane that dies mid-flight
patches every still-owed path onto the pipe as one typed
Failed(LaneFailure::LaneDeath) record (the ledger stays exact), the
posture cordons via the sticky PostureCause::LaneDeath input, and the
process LIVES.

**The CI profile matrix (FF-T5)**: job A is "auto" on the standard 4-vCPU
runner (the real small-host path - the serial tier); job B is forced
"pinned" (the seats and fan-out, marked oversubscribed). The
budget-algebra (the pure plan/budget tiering tests) and the binding-parity
gates (the pinned-vs-serial outcome-corpus identity) run as named CI
steps.
