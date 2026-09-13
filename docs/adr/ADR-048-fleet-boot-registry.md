# ADR-048: Fleet boot registry — one keyed owner for the pooled roles' boot facts

**Status: accepted** (2026-09-13; architecture-review candidate #4, grilled + settled; ergo epic `DQA7YL`, task `YUMQU3`). Red contract pins landed at `b623e86de` (T1); the registry landed at `64854de2` (T2, "refactor(fleet): FleetBootRegistry owns the pooled roles boot").

## Context

Candidate 4 of the fleet architecture review found that
`fleet_sim_executor` and `fleet_registration_executor` were
byte-structural twins over the `seat_host` machinery, and that their boot
facts lived as **module-private statics reached across modules**:

- `fleet_status` read **registration's** statics as the process-canonical
  boot, while the fleet-profile record fired from **sim's** install path.
  Which role's boot was canonical was therefore decided by *which module
  happened to be reachable* — canonical-by-accident via public surface,
  not by a declared owner.
- The sim and registration role modules each carried a private
  `OnceLock` boot courier + executor courier and a boot fn; the shared
  seat host parameterized on one, and the status/intake call sites had to
  know which static to consult.

A settled decision (DQA7YL) required one keyed owner, with the pooled
roles shrinking to descriptor rows + thin boot functions.

## Decision

### D1 — `FleetBootRegistry`: one struct, two typed rows

`seat_host::FleetBootRegistry` is the ONE keyed owner of the two pooled
roles' boot facts. Each typed `BootSlot<T>` carries:

- the role's `SeatRoleDesc` (the descriptor row — role, grant kind,
  `BootRole`, abort tag/noun, host thread, stamp-missing message, seats
  fn), moved out of the role modules into the registry;
- the construction-stamped boot courier (`OnceLock<BootStamp>`);
- the process-wide executor courier (`OnceLock<Result<T, BootError>>`).

The registry also carries the **first-wins canonical process boot**
(`process_boot: OnceLock<FleetBoot>`). Whichever registry role installs
first owns it, stamped exactly once; `boot_installed()` reads that same
latch. `fleet_status` and `fleet_intake` read the registry — never a
role module's private static.

The role modules retain only their executor and a thin boot fn; the
per-role submit/port/test-shim surface is generated once by
`impl_seat_hosted!`.

### D2 — Two rows, not three: the solve executor is out of registry scope

`FleetSolveExecutor` is deliberately absent. Its seat model is a
different shape — per-seat keyed mboxes (warm arenas, T3/T6 re-pin),
typed `SubmitReceipt`/`SubmitError`, and an infallible global — so it
installs its own boot. Folding it into a pooled two-row registry would
force one type over two seat models; the candidate is two rows, not
three. `role(BootRole::Solve)` is an explicit `unreachable!` documenting
the exclusion.

### D3 — The retired shape: cross-module static reach

Reading another module's private `OnceLock` (or asking a call site to
know which module's static is canonical) is **retired**. The registry is
the declared owner; the compile is the guard against re-reaching around
it. This supersedes the role-module-static shape the fleet shipped with.

### D4 — This completes ADR-042's descriptor intent

ADR-042 §2 decided that a role is *an entry, not a redesign*: the role set
is data, declared once. `SeatRoleDesc` was the descriptor that made a
pooled role a row; candidate 4 puts the descriptor rows in the ONE keyed
owner, so a pooled role's boot facts are an entry in the registry rather
than a private module static. The solve role's exclusion is the concrete
statement of "declared, not forced".

## Considered options rejected

- **Keep the role-module statics and add a "canonical" pointer.** Rejected:
  a pointer is another hand-mirrored owner, exactly the accidental
  canonicality the review flagged; the first-wins latch is the owner.
- **Fold the solve executor into a third row.** Rejected by D2: its seat
  model (keyed mailboxes, typed receipts, infallible global) is a
  different shape; one registry type over two models is the documented
  misfit.
- **Make `fleet_status` read sim's or registration's static explicitly.**
  Rejected: that is the bug — install-order-dependent canonicality.
- **Keep the registry types `pub` for a future external consumer.**
  Rejected: no Python-crate or cross-crate consumer exists; the surface
  is `pub(crate)`, and the registration executor's `pub` surface narrowed
  to `pub(crate)` with it (sim was already `pub(crate)`).

## Consequences

- The canonical process boot is install-order-independent: whichever
  pooled role installs first owns it, and `runtime_status()` reports the
  same boot facts regardless of order.
- A pooled role gains a boot by adding a registry row, not by threading a
  new cross-module static through the status/intake call sites.
- The solve host's separate boot install remains, by explicit design, not
  by omission.
- The build receipt was verified after the Rust edits; no Python-visible
  API change.

## Related

- **ADR-042** (role-switching worker fleet) — D4: the descriptor intent
  this candidate completes; a role is an entry, not a redesign.
- **ADR-044** (fleet intake liveness) — the intake/status paths that read
  the registry's latch.
- The candidate-4 task chain: `DQA7YL` (design), `YUMQU3` / `64854de2`
  (registry T2), and its record/liability task `QU52AV`.
