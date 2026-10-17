//! `seat_host` — the ONE host machinery for the fleet hosts: the
//! byte-identical `WorkQueue` pair, the `seat_loop` pair, and — since
//! The ONE [`HostPump`] host-message triple (`apply_host_msg` +
//! the backlog-draining grant pump + the recv loop) that ALL THREE fleet
//! hosts run (the pooled pair AND the solve host), plus the
//! construction-stamped boot install/global boilerplate.
//! `fleet_sim_executor`, `fleet_registration_executor`, and
//! `fleet_solve_executor` parameterize the triple; the seat models stay
//! per host kind: the pooled `WorkQueue` here, the solve
//! host's per-seat keyed mailboxes there.
//!
//! # The admission model (design gate — decided BEFORE any code moved)
//!
//! All three fleet roles reconcile under ONE invariant — the never-drop
//! ledger (design doc §10): no admission path drops or refuses a unit. A
//! full per-role queue and a cordon hold BOTH convert to a wait (spill
//! into the unbounded host backlog, drain FIFO); the only failure shape is
//! a closed host channel, and that is a loud abort (a lost unit strands
//! its receipt pipe). What differs per role is the cordon effect at
//! admission and the seat model:
//!
//! - registration (`PoolStateUpdater`, Deferrable cordon class): the
//!   never-drop flood with unbounded backlog; a `Cordoned` posture HOLDS
//!   new intake — held units wait in the backlog, never dropped
//!   (`fleet_intake.rs`; test
//!   `fleet_intake_facade_preserves_the_never_drop_flood` and
//!   Part A's reached-Cordoned test
//!   `a_cordoned_posture_holds_registration_intake_until_the_cordon_
//!   lifts`).
//! - sim (`SimDriver`, `SimPool` cordon class): seat-pool pacing — the seat
//!   pool IS the grant lane, capping concurrent executing sims at the
//!   budget's sim slot cap; a `Cordoned` posture still ADMITS sim leases
//!   (floored, not held — the floor is the host FSM's sim-intake lane,
//!   dispatcher-side; tests `sim_seat_pool_is_the_budget_sim_slot_cap`,
//!   `concurrent_sims_are_bounded_by_the_sim_slot_cap`).
//! - solve (`Solver`, `CordonClass::Never`): posture-INVARIANT admission
//!   at the TYPED submit seam — a Cordoned posture must NEVER refuse a
//!   Solver bin (`fleet_solve_executor.rs`; test
//!   `submit_in_cordoned_posture_still_admits_solver_units_and_running_
//!   units_complete`).
//!
//! DECISION: the host serves the pairwise-
//! compatible `WorkQueue` pair (sim + registration); the solve executor's
//! SEAT MODEL (per-seat mpsc mailboxes keyed by the Solver pin — T3/T6
//! warm arenas, `seat_loop(seat, rx, done)`) and its typed submit seam
//! (`Result<SubmitReceipt, SubmitError>`) stay in
//! `fleet_solve_executor.rs` — folding the seat models themselves is the
//! documented misfit. The fold executes the spike's settled WAITING answer
//! on top: the host-MESSAGE triple (`apply_host_msg` + `pump`, backlog
//! drain included) folds ONCE here as [`HostPump`], and the solve host
//! JOINS it — the cap/consult/hold rules now apply ONCE per host instead
//! of twice per executor. The posture consult is unconditional in the
//! unified shape and behavior-EXACT for solve (spike-proven; the proof
//! restated on [`HostPump`]): `admits_lease` blocks ONLY the
//! `(Cordoned, Deferrable)` pair and `Solver` is `CordonClass::Never`.
//! The dissolved two-arm `CordonAdmission` descriptor
//! field entirely: admission consults the ONE shared posture owner
//! directly and derives the policy from the role's own cordon class —
//! `FleetHost::posture_admits_role` is the SAME `admits_lease`
//! predicate the dispatcher's gates consult, so Deferrable (registration)
//! intake holds while cordoned and `SimPool` (sim) leases are
//! floored-never-held, with no per-role enum arm and no hand mirror to
//! drift. (Solve's retired `HostMsg::Throttle` arm went with it — the
//! process posture owner is fed by the block pump; every host consults
//! the same owner.) Solve follows the same never-drop PRINCIPLE (unbounded
//! backlog, §10): its queue-len mirror + typed submit receipt stay at its
//! submit seam — the mirror stamps ride the unified triple through
//! [`HostPump`]'s `mirror` field.
//!
//! # Invariants preserved (behavior byte-stable)
//!
//! - Boot/census/thread names: seat threads take `WorkerRole::thread_name()`
//!   with `{n}` substituted (`work-fleet-sim-{n}` /
//!   `work-fleet-poolupd-{n}`), host threads take the descriptor's
//!   `host_thread`; the census rows self-register inside `FleetHost::boot`
//!   — the `BootRole` labels byte-stay (`fleet_simdriver_slots` /
//!   `fleet_pool_state_updater_slots`).
//! - `boot_stamp.rs` logic untouched: every install routes the ride ledger
//!   (`record_ride`) with the descriptor's [`BootRole`], exactly as before.
//! - The `fleet_intake.rs` frozen facade untouched: the port + 2 pub fns
//!   surface is unchanged, and `InnerWork = Box<dyn FnOnce>` carries NO
//!   pyo3 in any host signature (`Python::attach` stays at the pyo3 leaf).
//! - Loud aborts: every seat/host failure funnels through the ONE abort
//!   emission (`host::abort_unrecoverable`, ADR-079 D3) — one stranded-pipe
//!   template, with the tag + noun per host (pooled: the descriptor's
//!   `[fleet-sim]` / `[fleet-reg]`; keyed solve: its mirrored constants).
//!
//! # The lane interface (FF-T3 — lanes stay logical)
//!
//! Lanes are LOGICAL: the LANEWARDEN lane vocabulary names WHO owns
//! which receipts and ledger writes, never which thread runs them — the
//! BINDING is the adapter that maps lanes to threads (two today: pinned,
//! and the serial binding that lands with FF-T4). One lane interface,
//! binding-independent by construction:
//!
//! - **H (reserve)** — the pump-driven host lane: the block pump feeds
//!   the ONE posture owner and the stage machine; it owns no fleet
//!   receipts of its own (its “writes” are the stage-machine rows).
//! - **A (ambient)** — the ambient I/O runtime (`degenbot-io-rt-{n}`):
//!   pump/dispatch/delivery/pyo3-async work; no per-unit receipts.
//! - **R (resolve)** — the pooled resolve seats (dispatcher lane): the
//!   resolve units' completions ride the slot FSM (T5), no caller pipes.
//! - **M (merge)** — the merge sidecar's per-path result pipe: EVERY
//!   solved/suppressed/failed path's terminal send lands here — the
//!   exactness fuse (solved + suppressed + failed ==
//!   submitted) is enforced at the merge drain, per cycle.
//! - **`PoolStateUpdater` seats** — the registration intake: each unit
//!   owns its intake receipt (the awaiting caller's join), held in the
//!   unbounded §10 backlog under a cordon (never dropped).
//! - **`SimDriver` seats** — the inline sims: each request owns its
//!   per-request receipt channel (the walker's `PendingSim`), admitted
//!   under the cordon sim-intake floor.
//! - **Solver seats** — the keyed bins: each bin's per-path result
//!   sends feed the merge pipe through the lane witness (`SolveLane`:
//!   the per-cycle one-outcome-per-path ledger; a panicked bin's
//!   undelivered pids arrive as typed `Failed` records).
//!
//! The outcome ledger, intake receipts, and the exactness fuse live at
//! the LANE level: a binding changes WHICH THREADS run the lanes, never
//! the ownership. The census `binding` field prints the mapping per
//! entry (`pinned` = dedicated seat threads, `shared` = pooled runtimes,
//! `logical` = a lane riding other threads' time).

// The concept modules (ADR-079 D1): the pure admission verdicts in
// `admission`, the unified host-message triple (`HostPump`) in `pump`,
// the seat host + loops in `host`; the process-wide boot statics live in
// the sibling `fleet_boot` module.
mod admission;
mod host;
mod pump;

pub(crate) use admission::GrantContract;
pub(crate) use host::{
    abort_unrecoverable, host_loop, impl_seat_hosted, intake_close_abort, HostLoop, SeatHost,
    SeatJob, SeatRoleDesc, SERIAL_SEAT_NAME,
};
pub(crate) use pump::{HostDiscipline, HostMsg, SeatSink};
