# ADR-079: The seat host split — two seat models behind one host loop

**Status: accepted (2026-10-16).** Drafted at the architecture-review grilling (candidate C5; rounds 1–4, Q7/Q16/Q23, Q26 as corrected by fact-finding).

## Context

`arb_engine/seat_host.rs` (3,349 lines) hosts four concepts plus ~1,500 lines of test scaffolding: admission policy (`AdmissionInputs`/`admission()`, `NoProgressGuard`, `GrantContract`), the pump (`HostPump`, `apply_host_msg`), the seat host + loops (`SeatHost`, `seat_loop`, `host_loop`), and the process-wide boot statics (`BootSlot`, `FleetBootRegistry`, the `global_executor!` macro).

Fact-finding corrected the review's initial framing: the fleet twins are **not** shallow mirrors. `host_loop` is already shared (`HostPump::run`); what `fleet_solve_executor.rs` carries is a genuinely different **seat model** — keyed per-seat mailboxes, the warm-arena `LaneCtx` (T3/T6 reuse), per-bin pin identity, `SolverPins` grants — versus the pooled model's contended `WorkQueue` and `LaneCtx::detached()`. Collapsing the two into one loop would break warm-arena reuse and pin identity. `fleet_sim_executor.rs` and `fleet_registration_executor.rs` define no twins at all — the pooled model serves them. What actually diverges gratuitously is **vocabulary**: two `SeatJob` shapes, and discipline wording forked per model (the "seat completion (T3)" vs "(T5)" log fork, "stranded result pipe" vs "stranded {noun} receipt pipe").

## Decision

**D1 — Split at the concept lines.** `seat_host.rs` becomes `arb_engine/seat_host/` with `admission.rs` (pure verdicts — returns results, no side effects, directly testable), `pump.rs`, and `host.rs` (the host loop). The boot statics move to a sibling `arb_engine/fleet_boot.rs` — they are process-wide, not seat-scoped.

**D2 — Two seat models, one host-loop module.** The pooled and keyed models stay as two adapters behind the one host-loop interface — two adapters make the seam real. The keyed model's `LaneCtx`/arena/pin semantics are load-bearing and stay; the shared loop's visibility widens to serve both.

**D3 — One vocabulary across the models.** One `SeatJob` shape with the keyed context riding as data (an absent context *is* the pooled case); one discipline wording — the T3/T5 fork and the abort-phrasing fork die.

## Consequences

Admission policy becomes testable as pure functions. A discipline or pump change has one home; a seat-model change touches exactly its adapter. The split is byte-identical relocation; no scheduling behavior changes in the same change.
